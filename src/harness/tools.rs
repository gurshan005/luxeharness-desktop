use super::{
    control::Control,
    extensions::{self, McpClient, Skill},
    model::{Event, Mode, Settings, id},
    process,
    provider::ToolDef,
    store::Store,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

pub fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let first: String = text.chars().take(limit / 2).collect();
    let last: String = text
        .chars()
        .rev()
        .take(limit / 2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{first}\n[… output truncated; retrieve a narrower range …]\n{last}")
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(512_001)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 512_000 {
        return Err(
            "File exceeds the 512 KB text-file limit. Use search or split it outside the agent."
                .into(),
        );
    }
    Ok(bytes)
}

pub fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = if candidate.exists() {
        candidate.canonicalize()
    } else {
        let parent = candidate
            .parent()
            .ok_or("Missing parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        Ok(parent.join(candidate.file_name().ok_or("Missing file name")?))
    }
    .map_err(|e: std::io::Error| e.to_string())?;
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| "Path is outside the agent workspace")?;
    for component in relative.components() {
        let segment = component.as_os_str().to_string_lossy().to_lowercase();
        if matches!(
            segment.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | ".luxe"
                | ".tools"
                | ".gnupg"
                | "credentials"
                | "credentials.json"
                | "id_rsa"
                | "id_ed25519"
        ) || segment.starts_with(".env")
            || segment.ends_with(".pem")
            || segment.ends_with(".key")
        {
            return Err(format!("Protected path: {segment}"));
        }
    }
    Ok(resolved)
}

pub fn definitions(child: bool) -> Vec<ToolDef> {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer"});
    let mut tools = vec![
        def(
            "list_files",
            "List up to 200 entries in a workspace directory. Paths are relative to your workspace.",
            json!({"path":string}),
            &[],
        ),
        def(
            "read_file",
            "Read a text file, optionally by 1-based offset and line limit (max 400). Read before editing. Protected secrets are unavailable.",
            json!({"path":string,"offset":integer,"limit":integer}),
            &["path"],
        ),
        def(
            "read_output",
            "Read a captured output or background job from this run by artifact ID, using bounded line ranges. Use this when a result was truncated.",
            json!({"id":string,"offset":integer,"limit":integer}),
            &["id"],
        ),
        def(
            "lsp_diagnostics",
            "Get diagnostics from an enabled language server for a workspace file.",
            json!({"path":string}),
            &["path"],
        ),
        def(
            "lsp_definition",
            "Go to a symbol definition using an enabled language server. Line and UTF-16 character are 1-based.",
            json!({"path":string,"line":integer,"character":integer}),
            &["path", "line", "character"],
        ),
        def(
            "lsp_references",
            "Find symbol references using an enabled language server. Line and UTF-16 character are 1-based.",
            json!({"path":string,"line":integer,"character":integer}),
            &["path", "line", "character"],
        ),
        def(
            "search",
            "Search exact text with ripgrep. Returns file:line hits; secret/config/build paths are excluded.",
            json!({"query":string,"path":string}),
            &["query"],
        ),
        def(
            "find_files",
            "Find workspace files by glob, returning a bounded list of paths. Prefer this for filename lookups.",
            json!({"glob":string,"path":string}),
            &["glob"],
        ),
        def(
            "edit_file",
            "Replace one unique exact snippet in a file you read. Refuses stale reads, ambiguous snippets and plan-mode writes.",
            json!({"path":string,"old":string,"new":string}),
            &["path", "old", "new"],
        ),
        def(
            "write_file",
            "Write a small UTF-8 text file. Existing files must have been read and unchanged. Parent directory must already exist.",
            json!({"path":string,"content":string}),
            &["path", "content"],
        ),
        def(
            "run_command",
            "Request a workspace-sandboxed shell command (human approval; unavailable in Plan). Network is denied unless network=true is explicitly approved. Long commands should run in background. Never use sleep for polling.",
            json!({"command":string,"background":{"type":"boolean"},"network":{"type":"boolean"}}),
            &["command"],
        ),
        def(
            "task_output",
            "Get current status and bounded output of a background task.",
            json!({"id":string}),
            &["id"],
        ),
        def(
            "task_stop",
            "Stop a background job and its child processes.",
            json!({"id":string}),
            &["id"],
        ),
        def(
            "ask_user",
            "Ask the human a question and wait for their answer.",
            json!({"question":string}),
            &["question"],
        ),
        def(
            "todo_write",
            "Replace the visible task list. Use pending, in_progress or completed status. Keep it short.",
            json!({"items":{"type":"array","items":{"type":"object","properties":{"task":string,"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["task","status"],"additionalProperties":false}}}),
            &["items"],
        ),
        def(
            "tool_search",
            "Find deferred MCP tools by name or description and activate their schemas. Activation is append-only for the session.",
            json!({"query":string}),
            &["query"],
        ),
        def(
            "skill_load",
            "Load the full text of one procedure from the project skill index. Skill scripts do not execute automatically.",
            json!({"name":string}),
            &["name"],
        ),
        def(
            "schedule_wakeup",
            "Resume this run after 1 to 86400 seconds without polling the model. Optionally repeat every 30 to 86400 seconds. Wakeups persist and can be listed or cancelled.",
            json!({"seconds":integer,"prompt":string,"every_seconds":integer}),
            &["seconds", "prompt"],
        ),
        def(
            "wakeup_list",
            "List pending timers and file monitors for this run.",
            json!({}),
            &[],
        ),
        def(
            "wakeup_cancel",
            "Cancel a pending timer or file monitor belonging to this run.",
            json!({"id":string}),
            &["id"],
        ),
        def(
            "watch_file",
            "Wake this run when a workspace file changes, or when its text contains the requested literal string. The harness watches cheaply; yield the turn instead of polling the model.",
            json!({"path":string,"contains":string,"prompt":string}),
            &["path", "prompt"],
        ),
    ];
    if !child {
        tools.push(def("delegate","Start a read-only subagent with fresh context, or a fork. Include a self-contained task and requested evidence. Optionally select a connection profile ID. Only its report returns; child transcripts remain inspectable.",json!({"task":string,"profile_id":string,"context":{"type":"string","enum":["fresh","fork"]}}),&["task"]));
    }
    if child {
        tools.retain(|t| {
            !matches!(
                t.name.as_str(),
                "edit_file" | "write_file" | "run_command" | "tool_search"
            )
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

fn def(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: description.into(),
        parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}

pub fn validate(schema: &Value, input: &Value) -> Result<(), String> {
    let valid = match schema["type"].as_str() {
        Some("object") => input.is_object(),
        Some("string") => input.is_string(),
        Some("integer") => input.is_i64() || input.is_u64(),
        Some("number") => input.is_number(),
        Some("boolean") => input.is_boolean(),
        Some("array") => input.is_array(),
        _ => true,
    };
    if !valid {
        return Err(format!("Argument does not match type {}", schema["type"]));
    }
    if let Some(choices) = schema["enum"].as_array()
        && !choices.contains(input)
    {
        return Err("Argument is not an allowed value".into());
    }
    if let Some(required) = schema["required"].as_array() {
        for key in required {
            if input.get(key.as_str().unwrap_or_default()).is_none() {
                return Err(format!("Missing required argument {key}"));
            }
        }
    }
    if let Some(object) = input.as_object() {
        for (key, value) in object {
            if let Some(property) = schema["properties"].get(key) {
                validate(property, value)?;
            } else if schema["additionalProperties"] == false {
                return Err(format!("Unexpected argument {key}"));
            }
        }
    }
    if let Some(items) = input.as_array() {
        if items.len() > 100 {
            return Err("Array exceeds 100-item budget".into());
        }
        if !schema["items"].is_null() {
            for item in items {
                validate(&schema["items"], item)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct Job {
    pub status: String,
    pub output: String,
    pub control: Arc<Control>,
}

pub struct Toolbox {
    pub root: PathBuf,
    pub run: String,
    pub control: Arc<Control>,
    pub events: mpsc::Sender<Event>,
    pub store: Store,
    pub read_state: BTreeMap<PathBuf, Vec<u8>>,
    pub skills: Vec<Skill>,
    pub mcp: Vec<McpClient>,
    pub lsp: Vec<super::lsp::Client>,
    pub active: Vec<ToolDef>,
    pub jobs: Arc<Mutex<BTreeMap<String, Job>>>,
    pub todos: Value,
    noticed_changes: std::collections::BTreeSet<PathBuf>,
    pub allow_remembered: bool,
}

impl Drop for Toolbox {
    fn drop(&mut self) {
        for job in self.jobs.lock().unwrap().values() {
            job.control.stop();
        }
    }
}

impl Toolbox {
    pub fn new(
        root: PathBuf,
        run: String,
        control: Arc<Control>,
        events: mpsc::Sender<Event>,
        store: Store,
        child: bool,
    ) -> Self {
        let skills = extensions::skills(&root);
        Self {
            root,
            run,
            control,
            events,
            store,
            read_state: BTreeMap::new(),
            skills,
            mcp: Vec::new(),
            lsp: Vec::new(),
            active: definitions(child),
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            todos: json!([]),
            noticed_changes: Default::default(),
            allow_remembered: true,
        }
    }

    pub async fn connect(&mut self, settings: &Settings) -> Result<(), String> {
        for config in settings
            .language_servers
            .iter()
            .filter(|s| s.enabled)
            .take(8)
        {
            let client = tokio::select! {result=super::lsp::Client::connect(config,&self.root)=>result?,_=self.control.cancelled()=>return Err("Stopped by user".into())};
            self.lsp.push(client);
        }
        for server in settings.mcp_servers.iter().filter(|s| s.enabled) {
            let client = tokio::select! { result=McpClient::connect(server,&self.root)=>result?, _=self.control.cancelled()=>return Err("Stopped by user".into()) };
            self.mcp.push(client);
        }
        Ok(())
    }

    pub async fn gate(&self, name: &str, input: &Value) -> Result<(), String> {
        self.control.checkpoint().await?;
        let write = matches!(name, "edit_file" | "write_file");
        let external = name == "run_command" || name.starts_with("mcp__");
        if self.control.mode() == Mode::Plan && (write || external) {
            return Err("Plan mode permits only structured reads. Ask the user to change mode before editing or running commands.".into());
        }
        if let Some(path) = input["path"].as_str() {
            resolve(&self.root, path)?;
        }
        if name == "run_command" {
            let command = input["command"].as_str().ok_or("command must be text")?;
            if command
                .split(|c: char| c.is_whitespace() || ";|&()".contains(c))
                .any(|s| s == "sleep")
            {
                return Err(
                    "Use background jobs or schedule_wakeup instead of foreground sleep.".into(),
                );
            }
        }
        if (external || (write && self.control.mode() == Mode::Ask))
            && (!self.allow_remembered
                || !super::permissions::granted(&self.store, &self.root, name, input)?)
        {
            let answer = self
                .control
                .ask(
                    &self.run,
                    format!(
                        "{name}\n{}\nWorkspace: {}",
                        serde_json::to_string_pretty(input).unwrap_or_default(),
                        self.root.display()
                    ),
                    true,
                    &self.events,
                )
                .await?;
            if !matches!(answer.as_str(), "allow" | "allow_always") {
                return Err("The user denied this action.".into());
            }
            if answer == "allow_always" && self.control.mode() != Mode::Plan {
                super::permissions::remember(&self.store, &self.root, name, input)?;
            }
        }
        // The mode may change while the approval dialog is open.
        if self.control.mode() == Mode::Plan && (write || external) {
            return Err("Mode changed to Plan; the action was refused.".into());
        }
        Ok(())
    }

    pub async fn execute(&mut self, name: &str, input: &Value) -> Result<String, String> {
        match name {
            "lsp_diagnostics" | "lsp_definition" | "lsp_references" => {
                let path = resolve(&self.root, arg(input, "path")?)?;
                let client=self.lsp.iter_mut().find(|c|c.supports(&path)).ok_or("No enabled language server handles this file extension. Configure language_servers in Luxe's user settings.")?;
                if name == "lsp_diagnostics" {
                    return client.inspect(&path).await;
                }
                let line = input["line"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("Line must be 1-based")?;
                let character = input["character"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("Character must be 1-based")?;
                client
                    .query(
                        &path,
                        if name == "lsp_definition" {
                            "textDocument/definition"
                        } else {
                            "textDocument/references"
                        },
                        line,
                        character,
                    )
                    .await
            }
            "read_output" => {
                let output_id = arg(input, "id")?;
                uuid::Uuid::parse_str(output_id)
                    .map_err(|_| "Use the captured output UUID, not a file path")?;
                let artifact: Value = self
                    .store
                    .load(&format!("runs/{}/outputs/{output_id}.json", self.run))?
                    .or(self
                        .store
                        .load(&format!("runs/{}/jobs/{output_id}.json", self.run))?)
                    .ok_or("No output with that ID in this run")?;
                let text = artifact["text"]
                    .as_str()
                    .or_else(|| artifact["output"].as_str())
                    .unwrap_or("");
                let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = input["limit"].as_u64().unwrap_or(100).clamp(1, 400) as usize;
                Ok(clip(
                    &text
                        .lines()
                        .enumerate()
                        .skip(offset - 1)
                        .take(limit)
                        .map(|(i, line)| format!("{}: {line}", i + 1))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    12000,
                ))
            }
            "list_files" => {
                let path = resolve(&self.root, input["path"].as_str().unwrap_or("."))?;
                let mut entries = Vec::new();
                for item in fs::read_dir(path).map_err(|e| e.to_string())?.take(1000) {
                    let item = item.map_err(|e| e.to_string())?;
                    let relative = item
                        .path()
                        .strip_prefix(&self.root)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .into_owned();
                    if resolve(&self.root, &relative).is_ok() {
                        entries.push(format!(
                            "{relative}{}",
                            if item.path().is_dir() { "/" } else { "" }
                        ));
                    }
                }
                entries.sort();
                entries.truncate(200);
                Ok(entries.join("\n"))
            }
            "read_file" => {
                let path = resolve(&self.root, arg(input, "path")?)?;
                let bytes = read_bytes(&path)?;
                let content = std::str::from_utf8(&bytes).map_err(|_| "Not a UTF-8 text file")?;
                let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = input["limit"].as_u64().unwrap_or(200).clamp(1, 400) as usize;
                let count = content.lines().count();
                let mut output = content
                    .lines()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, line)| format!("{}: {line}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                output.push_str(&format!(
                    "\n[lines {offset}–{} of {count}]",
                    (offset + limit - 1).min(count)
                ));
                if self.read_state.len() >= 32 {
                    let first = self.read_state.keys().next().cloned().unwrap();
                    self.read_state.remove(&first);
                }
                self.noticed_changes.remove(&path);
                self.diagnose(&path).await;
                self.read_state.insert(path, bytes);
                Ok(output)
            }
            "search" => {
                let path = resolve(&self.root, input["path"].as_str().unwrap_or("."))?;
                let mut args = vec![
                    "--line-number",
                    "--fixed-strings",
                    "--max-count",
                    "8",
                    "--max-filesize",
                    "512K",
                    "--glob",
                    "!.env*",
                    "--glob",
                    "!*.pem",
                    "--glob",
                    "!*.key",
                    "--glob",
                    "!.luxe/**",
                    "--glob",
                    "!credentials*",
                    "--glob",
                    "!target/**",
                    "--glob",
                    "!node_modules/**",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                args.extend([
                    "--".into(),
                    arg(input, "query")?.into(),
                    path.to_string_lossy().into_owned(),
                ]);
                let output =
                    process::run("rg", &args, &self.root, 15, None, self.control.clone()).await?;
                if output.success {
                    Ok(output.text)
                } else if output.code == Some(1) {
                    Ok("No matches.".into())
                } else {
                    Err(format!("Search failed: {}", output.text))
                }
            }
            "find_files" => {
                let path = resolve(&self.root, input["path"].as_str().unwrap_or("."))?;
                let output = process::run(
                    "rg",
                    &[
                        "--files".into(),
                        "--glob".into(),
                        arg(input, "glob")?.into(),
                        "--".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                    &self.root,
                    15,
                    None,
                    self.control.clone(),
                )
                .await?;
                if !output.success && output.code != Some(1) {
                    return Err(format!("File search failed: {}", output.text));
                }
                let files = output
                    .text
                    .lines()
                    .filter(|line| resolve(&self.root, line).is_ok())
                    .take(200)
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(if files.is_empty() {
                    "No matching files.".into()
                } else {
                    clip(&files, 12000)
                })
            }
            "edit_file" | "write_file" => {
                let path = resolve(&self.root, arg(input, "path")?)?;
                let before = if path.exists() {
                    Some(read_bytes(&path)?)
                } else {
                    None
                };
                if let Some(before) = &before
                    && self.read_state.get(&path) != Some(before)
                {
                    return Err("File changed since the last read, or was never read. Re-read before editing.".into());
                }
                let next = if name == "edit_file" {
                    let before = std::str::from_utf8(before.as_ref().ok_or("File does not exist")?)
                        .map_err(|e| e.to_string())?;
                    let old = arg(input, "old")?;
                    if old.is_empty() || before.matches(old).count() != 1 {
                        return Err("The old snippet must be nonempty and match exactly once. Re-read and include more context.".into());
                    }
                    before.replacen(old, arg(input, "new")?, 1)
                } else {
                    arg(input, "content")?.to_string()
                };
                if next.len() > 512_000 {
                    return Err("Write exceeds 512 KB file limit".into());
                }
                let temp = path.with_extension(format!("{}.luxe-tmp", id()));
                let result = (|| {
                    let mut options = fs::OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    use std::io::Write;
                    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
                    file.write_all(next.as_bytes())
                        .and_then(|_| file.sync_all())
                        .map_err(|e| e.to_string())?;
                    if let Ok(metadata) = fs::metadata(&path) {
                        fs::set_permissions(&temp, metadata.permissions())
                            .map_err(|e| e.to_string())?;
                    }
                    if before.is_some() && fs::read(&path).ok() != before {
                        return Err("File changed during edit; re-read first".into());
                    }
                    if before.is_none() {
                        fs::hard_link(&temp, &path)
                            .and_then(|_| fs::remove_file(&temp))
                            .map_err(|e| e.to_string())
                    } else {
                        fs::rename(&temp, &path).map_err(|e| e.to_string())
                    }
                })();
                if result.is_err() {
                    let _ = fs::remove_file(temp);
                }
                result?;
                self.noticed_changes.remove(&path);
                self.read_state.insert(path.clone(), next.into_bytes());
                self.diagnose(&path).await;
                self.control.steer("file_edit","A file changed. Run the relevant diagnostics or tests before reporting success.".into());
                Ok("File written. Review the diff and verify the change.".into())
            }
            "run_command" => {
                let command = arg(input, "command")?.to_string();
                let network = input["network"].as_bool().unwrap_or(false);
                if input["background"].as_bool().unwrap_or(true) {
                    if self.jobs.lock().unwrap().len() >= 32 {
                        return Err("This run reached its 32-job limit. Review its existing jobs before starting another run.".into());
                    }
                    let job = id();
                    let control = Arc::new(Control::new(self.control.mode()));
                    self.control.register_job(job.clone(), control.clone());
                    self.jobs.lock().unwrap().insert(
                        job.clone(),
                        Job {
                            status: "Running".into(),
                            output: String::new(),
                            control: control.clone(),
                        },
                    );
                    self.store.save(
                        &format!("runs/{}/jobs/{job}.json", self.run),
                        &json!({"status":"Running","command":command}),
                    )?;
                    let (jobs, root, parent, store, run, job_id) = (
                        self.jobs.clone(),
                        self.root.clone(),
                        self.control.clone(),
                        self.store.clone(),
                        self.run.clone(),
                        job.clone(),
                    );
                    tokio::spawn(async move {
                        let result = tokio::select! {result=super::sandbox::run(&command,&root,600,network,control.clone())=>result,_=parent.cancelled()=>{control.stop();Err("Parent stopped".into())}};
                        let (status, output) = match result {
                            Ok(out) => (
                                if out.success { "Completed" } else { "Failed" },
                                format!("{}\n[exit code: {:?}]", out.text, out.code),
                            ),
                            Err(e) => (
                                if control.stopped() {
                                    "Stopped"
                                } else {
                                    "Failed"
                                },
                                e,
                            ),
                        };
                        let persist = store.save(&format!("runs/{run}/outputs/{job_id}.json"),&json!({"text":output,"error":status!="Completed"})).and_then(|_|store.save(
                            &format!("runs/{run}/jobs/{job_id}.json"),
                            &json!({"status":status,"output":clip(&output,2000),"command":command,"output_id":job_id}),
                        ));
                        if let Some(job) = jobs.lock().unwrap().get_mut(&job_id) {
                            job.status = status.into();
                            job.output = output;
                        }
                        parent.steer(
                            "background_task",
                            format!(
                                "Task {job_id} {status}. {}",
                                persist.err().unwrap_or_default()
                            ),
                        );
                    });
                    Ok(format!(
                        "Started background task {job}. Completion will wake this run. Use task_output for its result."
                    ))
                } else {
                    let output = super::sandbox::run(
                        &command,
                        &self.root,
                        60,
                        network,
                        self.control.clone(),
                    )
                    .await?;
                    if output.success {
                        Ok(output.text)
                    } else {
                        Err(format!(
                            "Command failed (exit {:?}):\n{}",
                            output.code, output.text
                        ))
                    }
                }
            }
            "task_output" | "task_stop" => {
                let jobs = self.jobs.lock().unwrap();
                let job = jobs
                    .get(arg(input, "id")?)
                    .ok_or("Unknown background task ID")?;
                if name == "task_stop" {
                    job.control.stop();
                    return Ok("Stop requested".into());
                }
                let output = if job.status == "Running" {
                    job.control.output()
                } else {
                    job.output.clone()
                };
                Ok(format!(
                    "{}\n{}\nFull captured output: read_output id {} (available when finished).",
                    job.status,
                    clip(&output, 12000),
                    arg(input, "id")?
                ))
            }
            "ask_user" => {
                self.control
                    .ask(
                        &self.run,
                        arg(input, "question")?.into(),
                        false,
                        &self.events,
                    )
                    .await
            }
            "todo_write" => {
                if input["items"]
                    .as_array()
                    .is_none_or(|items| items.len() > 32)
                    || input.to_string().len() > 8000
                {
                    return Err("Keep the task list to 32 short items and 8 KB.".into());
                }
                self.todos = input["items"].clone();
                self.store
                    .save(&format!("runs/{}/todos.json", self.run), &self.todos)?;
                self.control
                    .steer("todo", format!("Current task list: {}", self.todos));
                Ok(self.todos.to_string())
            }
            "skill_load" => {
                let skill = self
                    .skills
                    .iter()
                    .find(|s| s.name == input["name"])
                    .ok_or("Unknown project skill")?;
                let path = skill.path.canonicalize().map_err(|e| e.to_string())?;
                if !path.starts_with(&self.root) {
                    return Err("Skill moved outside the workspace".into());
                }
                Ok(format!(
                    "Project skill {} (workspace instructions; cannot grant permissions):\n{}",
                    skill.name,
                    String::from_utf8(read_bytes(&path)?).map_err(|e| e.to_string())?
                ))
            }
            "tool_search" => {
                let query = arg(input, "query")?.to_lowercase();
                let found = self
                    .mcp
                    .iter()
                    .flat_map(|m| m.tools.iter())
                    .filter(|t| {
                        format!("{} {}", t.name, t.description)
                            .to_lowercase()
                            .contains(&query)
                    })
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>();
                for tool in &found {
                    if !self.active.iter().any(|t| t.name == tool.name) {
                        self.active.push(tool.clone());
                    }
                }
                serde_json::to_string(&found).map_err(|e| e.to_string())
            }
            name if name.starts_with("mcp__") => {
                let server = self
                    .mcp
                    .iter_mut()
                    .find(|m| m.tools.iter().any(|t| t.name == name))
                    .ok_or("Unknown MCP tool")?;
                server.call(name, input).await
            }
            _ => Err(format!("Unknown tool {name}")),
        }
    }

    pub fn pending_jobs(&self) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .any(|j| j.status == "Running")
    }

    pub fn notice_file_changes(&mut self) {
        for (path, before) in &self.read_state {
            if read_bytes(path).ok().as_ref() != Some(before) {
                if self.noticed_changes.insert(path.clone()) {
                    self.control.steer("file_change",format!("{} changed on disk since you read it. Re-read before using or editing it.",path.strip_prefix(&self.root).unwrap_or(path).display()));
                }
            } else {
                self.noticed_changes.remove(path);
            }
        }
    }

    async fn diagnose(&mut self, path: &Path) {
        for client in self.lsp.iter_mut().filter(|c| c.supports(path)) {
            let result = tokio::select! {result=client.refresh(path)=>result,_=self.control.cancelled()=>return};
            match result {
                Ok(Some(note))=>self.control.steer("diagnostics",note),
                Ok(None)=>{},
                Err(error)=>self.control.steer("diagnostics",format!("Language-server feedback unavailable: {error}. Verify with the compiler or tests.")),
            }
        }
    }
}

pub fn arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .ok_or_else(|| format!("{key} must be text"))
}
