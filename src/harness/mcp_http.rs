use super::{model::McpServer, provider::Sse};
use reqwest::{Client, Response, Url};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Http {
    client: Client,
    url: Url,
    session: Option<String>,
    pub protocol: String,
    key_env: String,
}

impl Http {
    pub fn new(config: &McpServer) -> Result<Self, String> {
        let url = Url::parse(&config.url).map_err(|_| "Invalid MCP URL")?;
        let local = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if !(url.scheme() == "https" || url.scheme() == "http" && local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("MCP URLs must use HTTPS (or localhost HTTP), without embedded credentials, queries or fragments.".into());
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            url,
            session: None,
            protocol: "2025-06-18".into(),
            key_env: config.key_env.clone(),
        })
    }

    async fn post(&mut self, value: &Value) -> Result<Response, String> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", &self.protocol)
            .json(value);
        if let Some(id) = &self.session {
            request = request.header("Mcp-Session-Id", id);
        }
        if !self.key_env.is_empty() {
            request = request.bearer_auth(
                std::env::var(&self.key_env)
                    .map_err(|_| format!("MCP credential variable {} is not set", self.key_env))?,
            );
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("MCP HTTP connection failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("MCP server returned HTTP {}", response.status()));
        }
        if let Some(id) = response.headers().get("mcp-session-id") {
            let id = id.to_str().map_err(|_| "Invalid MCP session header")?;
            if id.len() > 1024 || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err("Invalid MCP session ID".into());
            }
            self.session = Some(id.into());
        }
        Ok(response)
    }

    pub async fn send(&mut self, value: Value) -> Result<(), String> {
        self.post(&value).await.map(|_| ())
    }

    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let mut response = self
            .post(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let streaming = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let mut size = 0usize;
        let mut bytes = Vec::new();
        let mut sse = Sse::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("MCP response interrupted: {e}"))?
        {
            size += chunk.len();
            if size > 1_000_000 {
                return Err("MCP HTTP response exceeds 1 MB".into());
            }
            if streaming {
                for data in sse.push(&chunk)? {
                    let message: Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
                    if let Some(result) = self.accept(message, id).await? {
                        return Ok(result);
                    }
                }
            } else {
                bytes.extend_from_slice(&chunk);
            }
        }
        if !streaming {
            let message = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid MCP JSON response: {e}"))?;
            if let Some(result) = self.accept(message, id).await? {
                return Ok(result);
            }
        }
        Err("MCP response ended before the matching result".into())
    }

    async fn accept(&mut self, message: Value, id: u64) -> Result<Option<Value>, String> {
        if message["jsonrpc"] != "2.0" {
            return Err("Invalid MCP JSON-RPC version".into());
        }
        if let Some(method) = message["method"].as_str() {
            if !message["id"].is_null() {
                let reply = if method == "ping" {
                    json!({"jsonrpc":"2.0","id":message["id"],"result":{}})
                } else {
                    json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Client capability not enabled"}})
                };
                self.send(reply).await?;
            }
            return Ok(None);
        }
        if message["id"] != id {
            return Ok(None);
        }
        if !message["error"].is_null() {
            return Err(format!("MCP error: {}", message["error"]));
        }
        message
            .get("result")
            .cloned()
            .map(Some)
            .ok_or("MCP result missing".into())
    }
}
