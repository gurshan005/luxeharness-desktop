use super::{
    control::Control,
    model::{Profile, Provider},
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub struct JsonProcess {
    child: tokio::process::Child,
    input: Option<tokio::process::ChildStdin>,
    events: mpsc::Receiver<Result<Value, String>>,
    pending: VecDeque<Value>,
    stderr: Arc<Mutex<String>>,
    seq: u64,
    paused: bool,
    readers: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for JsonProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

pub fn command(
    program: &str,
    args: &[String],
    root: &Path,
    provider: Provider,
) -> tokio::process::Command {
    let mut command = super::process::command(program, args, root);
    // Native agents own their existing credential stores; never copy their tokens into Luxe.
    for key in ["HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "XDG_CONFIG_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if provider == Provider::ClaudeCode {
        for key in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    let mut paths =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    paths.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(std::path::PathBuf::from),
    );
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
}

impl JsonProcess {
    pub async fn spawn(profile: &Profile, args: &[String], root: &Path) -> Result<Self, String> {
        let program = super::native::resolve(profile)?;
        let mut child = command(&program.to_string_lossy(), args, root, profile.provider)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start native agent: {e}"))?;
        let input = child.stdin.take().ok_or("Missing native stdin")?;
        let stdout = child.stdout.take().ok_or("Missing native stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("Missing native stderr")?;
        let (sender, events) = mpsc::channel(64);
        let stderr = Arc::new(Mutex::new(String::new()));
        let errors = stderr.clone();
        let error_reader = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            while let Ok(n) = stderr_pipe.read(&mut buffer).await {
                if n == 0 {
                    break;
                }
                let mut errors = errors.lock().unwrap();
                if errors.len() < 128_000 {
                    errors.push_str(&String::from_utf8_lossy(&buffer[..n]));
                }
            }
        });
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            loop {
                let result = bounded_line(&mut stdout).await.and_then(|bytes| {
                    serde_json::from_slice(&bytes)
                        .map_err(|e| format!("Native agent sent invalid JSON: {e}"))
                });
                let failed = result.is_err();
                if sender.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            events,
            pending: VecDeque::new(),
            stderr,
            seq: 0,
            paused: false,
            readers: vec![reader, error_reader],
        })
    }

    pub async fn send(&mut self, value: Value) -> Result<(), String> {
        let input = self.input.as_mut().ok_or("Native input is closed")?;
        input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        input.flush().await.map_err(|e| e.to_string())
    }

    pub fn sequence(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    pub async fn next(&mut self) -> Result<Value, String> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(event);
        }
        match self.events.recv().await {
            Some(Ok(value)) => Ok(value),
            result => Err(format!(
                "{}\n{}",
                match result {
                    Some(Err(e)) => e,
                    _ => "Native process closed its event stream".into(),
                },
                super::tools::clip(&self.stderr.lock().unwrap(), 4000)
            )),
        }
    }

    pub async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        control: &Control,
    ) -> Result<Value, String> {
        let id = self.sequence();
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        let response = async {
            loop {
                let value = self.events.recv().await.ok_or("Native server exited")??;
                if value["id"] == id && value.get("method").is_none() {
                    if let Some(error) = value.get("error") {
                        return Err(format!("{method}: {error}"));
                    }
                    return value
                        .get("result")
                        .cloned()
                        .ok_or("Native reply has no result".into());
                }
                if value.get("id").is_some() && value.get("method").is_some() {
                    let result =
                        super::native::codex_denial(value["method"].as_str().unwrap_or_default());
                    self.send(json!({"id":value["id"],"result":result})).await?;
                } else {
                    self.pending.push_back(value);
                }
            }
        };
        tokio::select! {result=tokio::time::timeout(Duration::from_secs(45),response)=>result.map_err(|_|format!("{method} timed out"))?,_=control.cancelled()=>Err("Stopped by user".into())}
    }

    pub fn pause(&mut self, paused: bool) -> Result<(), String> {
        if paused == self.paused {
            return Ok(());
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let result = unsafe {
                libc::kill(
                    -(pid as i32),
                    if paused { libc::SIGSTOP } else { libc::SIGCONT },
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        self.paused = paused;
        Ok(())
    }

    pub async fn exit(&mut self) -> Result<(), String> {
        // Closing the pipe sends EOF; shutdown alone does not close ChildStdin.
        self.input.take();
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(|_| "Native process did not exit after its result")?
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "Native process exited {status}: {}",
                super::tools::clip(&self.stderr.lock().unwrap(), 2000)
            ))
        }
    }
}

async fn bounded_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|e| e.to_string())?;
        if available.is_empty() {
            return Err("Native agent closed stdout".into());
        }
        let n = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        if bytes.len() + n > 4_000_000 {
            return Err("Native event exceeded 4 MB".into());
        }
        let done = available[n - 1] == b'\n';
        bytes.extend_from_slice(&available[..n]);
        reader.consume(n);
        if done {
            if bytes.iter().all(u8::is_ascii_whitespace) {
                bytes.clear();
                continue;
            }
            return Ok(bytes);
        }
    }
}
