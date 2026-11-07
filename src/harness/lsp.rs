use super::{model::LanguageServer, process, tools};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

pub struct Client {
    pub config: LanguageServer,
    child: Child,
    input: ChildStdin,
    output: tokio::sync::mpsc::Receiver<Result<Value, String>>,
    reader: tokio::task::JoinHandle<()>,
    root: PathBuf,
    seq: u64,
    pull: bool,
    opened: HashMap<String, i64>,
    diagnostics: HashMap<String, Vec<Value>>,
    reported: HashMap<String, Vec<Value>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

impl Client {
    pub async fn connect(config: &LanguageServer, root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let uri =
            reqwest::Url::from_directory_path(&root).map_err(|_| "Invalid LSP workspace path")?;
        let mut child = process::command(&config.program, &config.args, &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start language server {}: {e}", config.name))?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("Missing LSP stdout")?);
        let (sender, output) = tokio::sync::mpsc::channel(32);
        let reader = tokio::spawn(async move {
            loop {
                let result = Self::read_message(&mut stdout).await;
                let failed = result.is_err();
                if sender.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        let mut client = Self {
            config: config.clone(),
            input: child.stdin.take().ok_or("Missing LSP stdin")?,
            output,
            reader,
            child,
            root,
            seq: 0,
            pull: false,
            opened: HashMap::new(),
            diagnostics: HashMap::new(),
            reported: HashMap::new(),
        };
        let result=client.request("initialize",json!({"processId":null,"rootUri":uri.as_str(),"capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"publishDiagnostics":{"versionSupport":true},"diagnostic":{}}},"clientInfo":{"name":"LuxeHarness","version":"0.1.0"}})).await?;
        if result["capabilities"]["positionEncoding"]
            .as_str()
            .is_some_and(|encoding| encoding != "utf-16")
        {
            return Err("Language server selected an unsupported position encoding".into());
        }
        client.pull = !result["capabilities"]["diagnosticProvider"].is_null()
            && result["capabilities"]["diagnosticProvider"] != false;
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    pub fn supports(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| {
                self.config
                    .extensions
                    .iter()
                    .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
            })
    }

    async fn send(&mut self, value: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        self.input
            .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.input
            .write_all(&bytes)
            .await
            .map_err(|e| e.to_string())?;
        self.input.flush().await.map_err(|e| e.to_string())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    async fn receive(&mut self) -> Result<Value, String> {
        self.output
            .recv()
            .await
            .ok_or("Language server reader stopped")?
    }

    async fn read_message(output: &mut BufReader<ChildStdout>) -> Result<Value, String> {
        let mut length = None;
        let mut total = 0;
        loop {
            let mut line = Vec::new();
            loop {
                let available = output.fill_buf().await.map_err(|e| e.to_string())?;
                if available.is_empty() {
                    return Err("Language server closed stdout".into());
                }
                let n = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(available.len(), |n| n + 1);
                total += n;
                if total > 8192 {
                    return Err("LSP header exceeded 8 KB".into());
                }
                let done = available[n - 1] == b'\n';
                line.extend_from_slice(&available[..n]);
                output.consume(n);
                if done {
                    break;
                }
            }
            let line = String::from_utf8(line).map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| "Invalid LSP content length")?,
                );
            }
        }
        let length = length
            .filter(|n| *n <= 1_000_000)
            .ok_or("Missing or oversized LSP content length")?;
        let mut bytes = vec![0; length];
        output
            .read_exact(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    async fn notification(&mut self, message: &Value) -> Result<(), String> {
        if message["method"] == "textDocument/publishDiagnostics" {
            let params = &message["params"];
            if let Some(uri) = params["uri"].as_str() {
                let stale = params["version"]
                    .as_i64()
                    .zip(self.opened.get(uri))
                    .is_some_and(|(version, current)| version < *current);
                if !stale {
                    self.diagnostics.insert(
                        uri.into(),
                        params["diagnostics"]
                            .as_array()
                            .map(|a| a.iter().take(200).cloned().collect())
                            .unwrap_or_default(),
                    );
                }
            }
        }
        if !message["id"].is_null() && message["method"].is_string() {
            self.send(json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Client capability not enabled"}})).await?;
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.seq += 1;
        let id = self.seq;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let message = self.receive().await?;
                if message["method"].is_string() {
                    self.notification(&message).await?;
                    continue;
                }
                if message["id"] != id {
                    continue;
                }
                if !message["error"].is_null() {
                    return Err(format!("LSP request {method}: {}", message["error"]));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or("LSP reply missing result".into());
            }
        })
        .await
        .map_err(|_| format!("Language server timed out on {method}"))?
    }

    async fn sync(&mut self, path: &Path) -> Result<String, String> {
        if !path.starts_with(&self.root) {
            return Err("LSP document outside the workspace".into());
        }
        let uri = reqwest::Url::from_file_path(path)
            .map_err(|_| "Invalid LSP document path")?
            .to_string();
        let text = String::from_utf8(tools::read_bytes(path)?)
            .map_err(|_| "LSP documents must be UTF-8")?;
        let version = self.opened.get(&uri).copied().unwrap_or(0) + 1;
        if version == 1 {
            self.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":self.config.language,"version":version,"text":text}})).await?;
        } else {
            self.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]})).await?;
        }
        self.opened.insert(uri.clone(), version);
        Ok(uri)
    }

    pub async fn refresh(&mut self, path: &Path) -> Result<Option<String>, String> {
        let uri = self.sync(path).await?;
        if self.pull {
            let report = self
                .request(
                    "textDocument/diagnostic",
                    json!({"textDocument":{"uri":uri}}),
                )
                .await?;
            if report["kind"] == "full" {
                self.diagnostics.insert(
                    uri.clone(),
                    report["items"]
                        .as_array()
                        .map(|a| a.iter().take(200).cloned().collect())
                        .unwrap_or_default(),
                );
            }
        } else {
            let until = tokio::time::Instant::now() + Duration::from_millis(500);
            loop {
                match tokio::time::timeout_at(until, self.receive()).await {
                    Ok(Ok(message)) => self.notification(&message).await?,
                    Ok(Err(error)) => return Err(error),
                    Err(_) => break,
                }
            }
        }
        let current = self.diagnostics.get(&uri).cloned().unwrap_or_default();
        let before = self
            .reported
            .insert(uri.clone(), current.clone())
            .unwrap_or_default();
        if before == current {
            return Ok(None);
        }
        let added = current
            .iter()
            .filter(|d| !before.contains(d))
            .collect::<Vec<_>>();
        let removed = before.iter().filter(|d| !current.contains(d)).count();
        Ok(Some(tools::clip(
            &format!(
                "Language diagnostics for {}: {} new, {removed} resolved. New diagnostics: {}",
                path.strip_prefix(&self.root).unwrap_or(path).display(),
                added.len(),
                serde_json::to_string(&added).unwrap_or_default()
            ),
            12000,
        )))
    }

    pub async fn query(
        &mut self,
        path: &Path,
        method: &str,
        line: u64,
        column: u64,
    ) -> Result<String, String> {
        let uri = self.sync(path).await?;
        let mut params = json!({"textDocument":{"uri":uri},"position":{"line":line.saturating_sub(1),"character":column.saturating_sub(1)}});
        if method == "textDocument/references" {
            params["context"] = json!({"includeDeclaration":true});
        }
        let result = self.request(method, params).await?;
        Ok(tools::clip(&result.to_string(), 12000))
    }

    pub async fn inspect(&mut self, path: &Path) -> Result<String, String> {
        let _ = self.refresh(path).await?;
        let uri = reqwest::Url::from_file_path(path)
            .map_err(|_| "Invalid LSP path")?
            .to_string();
        Ok(tools::clip(
            &serde_json::to_string(self.diagnostics.get(&uri).unwrap_or(&Vec::new()))
                .map_err(|e| e.to_string())?,
            12000,
        ))
    }
}
