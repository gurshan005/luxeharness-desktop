use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Codex,
    ClaudeCode,
    OpenAI,
    Anthropic,
    Gemini,
    Compatible,
}

impl Provider {
    pub const ALL: [Self; 6] = [
        Self::Codex,
        Self::ClaudeCode,
        Self::OpenAI,
        Self::Anthropic,
        Self::Gemini,
        Self::Compatible,
    ];

    pub fn defaults(self) -> (&'static str, &'static str) {
        match self {
            Self::Codex | Self::ClaudeCode => ("", ""),
            Self::OpenAI => ("https://api.openai.com/v1", "OPENAI_API_KEY"),
            Self::Anthropic => ("https://api.anthropic.com/v1", "ANTHROPIC_API_KEY"),
            Self::Gemini => (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "GEMINI_API_KEY",
            ),
            Self::Compatible => ("http://127.0.0.1:11434/v1", ""),
        }
    }

    pub fn native(self) -> bool {
        matches!(self, Self::Codex | Self::ClaudeCode)
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Codex => "Codex · native",
            Self::ClaudeCode => "Claude Code · native",
            Self::OpenAI => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::Compatible => "Local / compatible",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub model: String,
    pub endpoint: String,
    pub key_env: String,
    pub context_tokens: usize,
    #[serde(default)]
    pub guided: bool,
    #[serde(default)]
    pub executable: String,
}

impl Default for Profile {
    fn default() -> Self {
        let (endpoint, key) = Provider::OpenAI.defaults();
        Self {
            id: id(),
            name: "OpenAI".into(),
            provider: Provider::OpenAI,
            model: String::new(),
            endpoint: endpoint.into(),
            key_env: key.into(),
            context_tokens: 32_000,
            guided: false,
            executable: String::new(),
        }
    }
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.provider.native() {
            return super::native::resolve(self).map(|_| ());
        }
        if self.model.trim().is_empty() {
            return Err("Choose a model ID in Connections first.".into());
        }
        let url = reqwest::Url::parse(&self.endpoint).map_err(|_| "Invalid endpoint URL.")?;
        let local = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            return Err("Use HTTPS, or HTTP on the local machine.".into());
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
            return Err(
                "Credentials and query parameters do not belong in the endpoint URL.".into(),
            );
        }
        if !self.key_env.is_empty() && std::env::var(&self.key_env).unwrap_or_default().is_empty() {
            return Err(format!(
                "{} is not set in Luxe's environment.",
                self.key_env
            ));
        }
        if self.key_env.is_empty() && !local {
            return Err("A remote provider needs an API key environment variable.".into());
        }
        if !(8_192..=1_000_000).contains(&self.context_tokens) {
            return Err("Context budget must be between 8192 and 1000000 tokens.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Plan,
    Ask,
    Build,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Plan, Self::Ask, Self::Build];
    pub fn next(self) -> Self {
        match self {
            Self::Plan => Self::Ask,
            Self::Ask => Self::Build,
            Self::Build => Self::Plan,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plan => "Plan · read only",
            Self::Ask => "Ask before changes",
            Self::Build => "Build · allow file edits",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Isolation {
    Worktree,
    Shared,
}

impl Isolation {
    pub const ALL: [Self; 2] = [Self::Worktree, Self::Shared];
}

impl fmt::Display for Isolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Worktree => "Separate worktree",
            Self::Shared => "Workspace · one writer",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Context {
        source: String,
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        text: String,
        error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    pub role: String,
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<Value>,
}

impl Turn {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            blocks: vec![Block::Text { text: text.into() }],
            wire: None,
        }
    }

    pub fn context(source: &str, text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            blocks: vec![Block::Context {
                source: source.into(),
                text: text.into(),
            }],
            wire: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cached += other.cached;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub id: String,
    pub task: String,
    pub workspace: PathBuf,
    pub profile: Profile,
    pub mode: Mode,
    pub isolation: Isolation,
    pub parent: Option<String>,
    pub check_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub spec: RunSpec,
    pub status: String,
    pub root: PathBuf,
    pub branch: Option<String>,
    pub rounds: usize,
    pub context: usize,
    pub usage: Usage,
    pub report: String,
    pub transcript: Vec<Turn>,
    pub started: u64,
    #[serde(default)]
    pub native_session: Option<String>,
    #[serde(default)]
    pub replay_only: bool,
}

#[derive(Debug, Clone)]
pub struct NativeStatus {
    pub profile: String,
    pub authenticated: bool,
    pub message: String,
    pub models: Vec<String>,
    pub login_url: Option<String>,
    pub login_code: Option<String>,
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub run: String,
    pub time: u64,
    pub kind: String,
    pub detail: Value,
}

#[derive(Debug, Clone)]
pub enum Event {
    NativeStatus(NativeStatus),
    Record(Box<RunRecord>),
    Trace(Trace),
    Delta {
        run: String,
        text: String,
    },
    ResetStream(String),
    Question {
        run: String,
        id: String,
        prompt: String,
        approval: bool,
    },
    Answered {
        run: String,
        id: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub event: String,
    pub program: String,
    pub args: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageServer {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub language: String,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profiles: Vec<Profile>,
    pub workspace: Option<PathBuf>,
    pub mcp_servers: Vec<McpServer>,
    pub hooks: Vec<Hook>,
    pub language_servers: Vec<LanguageServer>,
    pub summary_profile_id: Option<String>,
    pub safety_profile_id: Option<String>,
}
