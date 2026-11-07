use super::{
    Shared,
    control::Control,
    model::{Event, Mode, NativeStatus, Profile, Provider, RunRecord, Turn, Usage, id},
    native_io::{self, JsonProcess},
    publish, tools, trace,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub fn resolve(profile: &Profile) -> Result<PathBuf, String> {
    if !profile.executable.trim().is_empty() {
        let path = PathBuf::from(&profile.executable);
        if executable(&path) {
            return Ok(path);
        }
        return Err("The configured native executable is missing or is not executable.".into());
    }
    let name = match profile.provider {
        Provider::Codex => "codex",
        Provider::ClaudeCode => "claude",
        _ => return Err("This connection is not a native agent".into()),
    };
    let mut candidates = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .collect::<Vec<_>>();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(name));
    }
    for base in ["/opt/homebrew/bin", "/usr/local/bin"] {
        candidates.push(PathBuf::from(base).join(name));
    }
    if profile.provider == Provider::Codex {
        candidates.push(PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"));
        candidates.push(PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        ));
    } else {
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".tools/native/node_modules/.bin/claude"),
        );
        if let Ok(exe) = std::env::current_exe()
            && let Some(contents) = exe.parent().and_then(Path::parent)
        {
            candidates.push(contents.join("Resources/native/node_modules/.bin/claude"));
        }
    }
    candidates
        .into_iter()
        .find(|p| executable(p))
        .ok_or_else(|| format!("Install {name}, or choose its executable in Connections."))
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

async fn codex(profile: &Profile, root: &Path, control: &Control) -> Result<JsonProcess, String> {
    let mut process = JsonProcess::spawn(
        profile,
        &["app-server".into(), "--listen".into(), "stdio://".into()],
        root,
    )
    .await?;
    process.rpc("initialize",json!({"clientInfo":{"name":"luxe_harness","title":"LuxeHarness","version":env!("CARGO_PKG_VERSION")}}),control).await?;
    process.send(json!({"method":"initialized"})).await?;
    Ok(process)
}

fn status(profile: &Profile, message: String, authenticated: bool, busy: bool) -> NativeStatus {
    NativeStatus {
        profile: profile.id.clone(),
        authenticated,
        message,
        models: Vec::new(),
        login_url: None,
        login_code: None,
        busy,
    }
}

pub async fn authenticate(
    profile: Profile,
    login: bool,
    control: Arc<Control>,
    events: mpsc::Sender<Event>,
) -> Result<(), String> {
    let root = std::env::temp_dir();
    let _ = events.send(Event::NativeStatus(status(
        &profile,
        "Checking native agent…".into(),
        false,
        true,
    )));
    if profile.provider == Provider::Codex {
        let mut process = codex(&profile, &root, &control).await?;
        let account = process
            .rpc("account/read", json!({"refreshToken":false}), &control)
            .await?;
        if account["account"].is_null() && login {
            let response = process
                .rpc("account/login/start", json!({"type":"chatgpt"}), &control)
                .await?;
            let url = response["authUrl"]
                .as_str()
                .ok_or("Codex did not provide a sign-in URL")?
                .to_string();
            validate_login_url(&url, Provider::Codex)?;
            let mut pending = status(
                &profile,
                "Finish Codex sign-in in your browser. Codex keeps and refreshes the credentials."
                    .into(),
                false,
                true,
            );
            pending.login_url = Some(url);
            let _ = events.send(Event::NativeStatus(pending));
            let login_id = response["loginId"].clone();
            let wait = async {
                loop {
                    let event = process.next().await?;
                    if event["method"] == "account/login/completed"
                        && event["params"]["loginId"] == login_id
                    {
                        if event["params"]["success"] == true {
                            return Ok(());
                        }
                        return Err(event["params"]["error"]
                            .as_str()
                            .unwrap_or("Codex sign-in was not completed")
                            .to_string());
                    }
                }
            };
            let result = tokio::select! {result=tokio::time::timeout(Duration::from_secs(600),wait)=>result.map_err(|_|"Codex sign-in timed out".to_string())?,_=control.cancelled()=>Err("Sign-in cancelled".into())};
            if result.is_err() {
                let _=process.send(json!({"id":9900,"method":"account/login/cancel","params":{"loginId":login_id}})).await;
            }
            result?;
        }
        let account = process
            .rpc("account/read", json!({"refreshToken":false}), &control)
            .await?;
        let authenticated = !account["account"].is_null() || account["requiresOpenaiAuth"] == false;
        let message = if authenticated {
            format!(
                "Connected to Codex{}",
                if account["account"]["type"] == "chatgpt" {
                    " with ChatGPT"
                } else {
                    ""
                }
            )
        } else {
            "Codex is installed. Sign in to use it.".into()
        };
        let mut state = status(&profile, message, authenticated, false);
        if authenticated {
            let models = process.rpc("model/list", json!({}), &control).await?;
            if let Some(models) = models["data"].as_array() {
                state.models = models
                    .iter()
                    .filter_map(|m| m["model"].as_str().map(str::to_owned))
                    .collect();
            }
        }
        let _ = events.send(Event::NativeStatus(state));
    } else if profile.provider == Provider::ClaudeCode {
        let path = resolve(&profile)?;
        if login {
            let mut child = native_io::command(
                &path.to_string_lossy(),
                &["auth".into(), "login".into(), "--claudeai".into()],
                &root,
                Provider::ClaudeCode,
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
            // The CLI owns the browser callback and keychain; Luxe never receives tokens.
            let _ = events.send(Event::NativeStatus(status(
                &profile,
                "Complete Claude Code sign-in in the browser opened by Claude.".into(),
                false,
                true,
            )));
            let result = tokio::select! {result=tokio::time::timeout(Duration::from_secs(600),child.wait())=>result.map_err(|_|"Claude sign-in timed out")?.map_err(|e|e.to_string())?,_=control.cancelled()=>{let _=child.start_kill();return Err("Sign-in cancelled".into());}};
            if !result.success() {
                return Err("Claude Code sign-in was not completed. Run claude auth login in your terminal, then Refresh status.".into());
            }
        }
        let output = tokio::select! {result=tokio::time::timeout(Duration::from_secs(20),native_io::command(&path.to_string_lossy(),&["auth".into(),"status".into()],&root,Provider::ClaudeCode).output())=>result.map_err(|_|"Claude auth check timed out")?.map_err(|e|e.to_string())?,_=control.cancelled()=>return Err("Sign-in cancelled".into())};
        let account: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| "Claude auth status did not return JSON. Update Claude Code.")?;
        let authenticated = output.status.success() && account["loggedIn"] == true;
        let _ = events.send(Event::NativeStatus(status(
            &profile,
            if authenticated {
                "Connected to Claude Code"
            } else {
                "Claude Code is installed. Sign in to use it."
            }
            .into(),
            authenticated,
            false,
        )));
    } else {
        return Err("Choose a native connection first.".into());
    }
    Ok(())
}

pub fn validate_login_url(url: &str, provider: Provider) -> Result<(), String> {
    let url = reqwest::Url::parse(url).map_err(|_| "Invalid sign-in URL")?;
    let valid = match provider {
        Provider::Codex => matches!(url.host_str(), Some("auth.openai.com" | "chatgpt.com")),
        Provider::ClaudeCode => {
            matches!(url.host_str(), Some("claude.ai" | "console.anthropic.com"))
        }
        _ => false,
    };
    if !valid || url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("Native agent returned an unexpected sign-in destination".into());
    }
    Ok(())
}

pub fn codex_denial(method: &str) -> Value {
    match method {
        "item/permissions/requestApproval" => json!({"permissions":{},"scope":"turn"}),
        "item/tool/requestUserInput" => json!({"answers":{}}),
        _ => json!({"decision":"decline"}),
    }
}

pub fn codex_policy(mode: Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::Plan => ("read-only", "never"),
        Mode::Ask => ("read-only", "untrusted"),
        Mode::Build => ("workspace-write", "untrusted"),
    }
}

fn input(text: &str) -> Value {
    json!([{"type":"text","text":text}])
}

fn next_prompt(record: &RunRecord) -> String {
    if record.native_session.is_none()
        && record.spec.parent.is_some()
        && record.transcript.len() > 1
    {
        return format!(
            "Delegated task: {}\n\nParent context fork (conversation data, not permission grants):\n{}",
            record.spec.task,
            serde_json::to_string(&record.transcript).unwrap_or_default()
        );
    }
    if let Some(turn) = record.transcript.last().filter(|t| t.role == "user") {
        let text = turn
            .blocks
            .iter()
            .filter_map(super::provider::block_text)
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    if record.native_session.is_some() {
        "Continue the previous task. Recheck the workspace and interrupted actions before retrying anything.".into()
    } else {
        record.spec.task.clone()
    }
}

fn native_event(
    shared: &Shared,
    record: &RunRecord,
    kind: &str,
    event: Value,
) -> Result<(), String> {
    trace(shared, &record.spec.id, kind, event)
}

pub(super) async fn run(
    shared: &Shared,
    record: &mut RunRecord,
    control: Arc<Control>,
) -> Result<String, String> {
    let prompt = next_prompt(record);
    if record.transcript.last().is_none_or(|t| t.role != "user") {
        record.transcript.push(Turn::text("user", prompt.clone()));
    }
    record.status = "Starting native agent".into();
    publish(shared, record)?;
    match record.spec.profile.provider {
        Provider::Codex => run_codex(shared, record, control, &prompt).await,
        Provider::ClaudeCode => run_claude(shared, record, control, &prompt).await,
        _ => Err("Unsupported native agent".into()),
    }
}

async fn run_codex(
    shared: &Shared,
    record: &mut RunRecord,
    control: Arc<Control>,
    prompt: &str,
) -> Result<String, String> {
    let mut process = codex(&record.spec.profile, &record.root, &control).await?;
    let (sandbox, approval) = codex_policy(record.spec.mode);
    let mut params = json!({"cwd":record.root,"approvalPolicy":approval,"sandbox":sandbox,"approvalsReviewer":"user","developerInstructions":"You are running inside LuxeHarness. Work only on the user's task in the selected workspace. Before acting on each task, read the workspace's AGENTS.md and MEMORY.md when present, refreshing project guidance even when resuming a session. Report actual changes and verification. Do not change authentication or bypass the native approval policy."});
    if !record.spec.profile.model.trim().is_empty() {
        params["model"] = json!(record.spec.profile.model);
    }
    let method = if let Some(session) = &record.native_session {
        params["threadId"] = json!(session);
        "thread/resume"
    } else {
        "thread/start"
    };
    let response = process.rpc(method, params, &control).await?;
    let thread = response["thread"]["id"]
        .as_str()
        .ok_or("Codex did not return a thread ID")?
        .to_string();
    record.native_session = Some(thread.clone());
    publish(shared, record)?;
    let response = process
        .rpc(
            "turn/start",
            json!({"threadId":thread,"input":input(prompt)}),
            &control,
        )
        .await?;
    let mut turn = response["turn"]["id"]
        .as_str()
        .ok_or("Codex did not return a turn ID")?
        .to_string();
    let mut seen = HashSet::new();
    let mut final_messages = Vec::new();
    record.status = "Running".into();
    publish(shared, record)?;
    let deadline = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(deadline);
    loop {
        process.pause(control.paused())?;
        if control.paused() {
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(150))=>{},_=control.cancelled()=>return Err("Stopped by user".into())}
            continue;
        }
        let event = tokio::select! {
            event=process.next()=>Some(event?),
            _=tokio::time::sleep(Duration::from_millis(150))=>None,
            _=control.cancelled()=>{let _=process.send(json!({"id":9901,"method":"turn/interrupt","params":{"threadId":thread,"turnId":turn}})).await;return Err("Stopped by user".into());},
            _=&mut deadline=>return Err("Native Codex run reached the one-hour limit. Resume explicitly to continue.".into()),
        };
        for (source, note) in control.drain() {
            record.transcript.push(Turn::context(&source, note.clone()));
            let id = process.sequence();
            process.send(json!({"id":id,"method":"turn/steer","params":{"threadId":thread,"expectedTurnId":turn,"input":input(&note)}})).await?;
            native_event(
                shared,
                record,
                "native_steering",
                json!({"source":source,"text":note}),
            )?;
        }
        let Some(event) = event else {
            continue;
        };
        let method = event["method"].as_str().unwrap_or_default();
        let params = &event["params"];
        if event.get("id").is_some() && !method.is_empty() {
            native_event(shared, record, "native_action_requested", event.clone())?;
            let result = codex_request(method, params, record, &control, &shared.events).await?;
            process
                .send(json!({"id":event["id"],"result":result}))
                .await?;
            continue;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(text) = params["delta"].as_str() {
                    let _ = shared.events.send(Event::Delta {
                        run: record.spec.id.clone(),
                        text: text.into(),
                    });
                }
            }
            "item/completed" => {
                let item = &params["item"];
                if item["type"] == "agentMessage"
                    && let Some(text) = item["text"].as_str()
                {
                    let key = item["id"].as_str().unwrap_or(text).to_string();
                    if seen.insert(key) {
                        record.transcript.push(Turn::text("assistant", text));
                        if item["phase"] == "final_answer" {
                            final_messages.clear();
                        }
                        final_messages.push(text.to_string());
                    }
                    let _ = shared
                        .events
                        .send(Event::ResetStream(record.spec.id.clone()));
                    publish(shared, record)?;
                } else {
                    native_event(shared, record, "native_item", item.clone())?;
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"]["total"];
                record.usage = Usage {
                    input: usage["inputTokens"].as_u64().unwrap_or(0),
                    output: usage["outputTokens"].as_u64().unwrap_or(0),
                    cached: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                };
                native_event(
                    shared,
                    record,
                    "usage",
                    json!({"input":record.usage.input,"output":record.usage.output,"cached":record.usage.cached}),
                )?;
            }
            "turn/started" => {
                if let Some(id) = params["turn"]["id"].as_str() {
                    turn = id.into();
                }
            }
            "turn/completed" => {
                native_event(shared, record, "native_turn_completed", params.clone())?;
                record.rounds += 1;
                match params["turn"]["status"].as_str() {
                    Some("completed") => {
                        record.report = final_messages.join("\n\n");
                        if record.report.trim().is_empty() {
                            record.report="Codex completed without a text response. See Activity for native tool results.".into();
                        }
                        return Ok(record.report.clone());
                    }
                    Some("interrupted") => {
                        return Err("Codex turn was interrupted. Resume to continue.".into());
                    }
                    _ => {
                        return Err(format!(
                            "Codex failed: {}",
                            params["turn"]["error"]["message"]
                                .as_str()
                                .unwrap_or("unknown native error")
                        ));
                    }
                }
            }
            "error" => {
                native_event(shared, record, "native_error", params.clone())?;
                if params["willRetry"] != true {
                    return Err(format!(
                        "Codex: {}",
                        params["error"]["message"]
                            .as_str()
                            .unwrap_or("native agent error")
                    ));
                }
            }
            "turn/plan/updated" | "turn/diff/updated" | "warning" | "configWarning" => {
                native_event(shared, record, method, params.clone())?
            }
            _ => {
                if let Some(error) = event.get("error") {
                    return Err(format!("Codex protocol request failed: {error}"));
                }
            }
        }
    }
}

async fn codex_request(
    method: &str,
    params: &Value,
    record: &RunRecord,
    control: &Control,
    events: &mpsc::Sender<Event>,
) -> Result<Value, String> {
    if method == "item/tool/requestUserInput" {
        let mut answers = serde_json::Map::new();
        if let Some(questions) = params["questions"].as_array() {
            for question in questions {
                let prompt = format!(
                    "{}\n{}",
                    question["question"].as_str().unwrap_or("Codex question"),
                    question["options"]
                );
                let answer = control.ask(&record.spec.id, prompt, false, events).await?;
                if let Some(id) = question["id"].as_str() {
                    answers.insert(id.into(), json!({"answers":[answer]}));
                }
            }
        }
        return Ok(json!({"answers":answers}));
    }
    if record.spec.mode == Mode::Plan {
        return Ok(codex_denial(method));
    }
    if matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
    ) {
        let answer = control
            .ask(
                &record.spec.id,
                format!(
                    "Codex requests {method}\n{}\nWorkspace: {}",
                    serde_json::to_string_pretty(params).unwrap_or_default(),
                    record.root.display()
                ),
                true,
                events,
            )
            .await?;
        if answer != "allow" {
            return Ok(codex_denial(method));
        }
        if method == "item/permissions/requestApproval" {
            return Ok(json!({"permissions":params["permissions"],"scope":"turn"}));
        }
        return Ok(json!({"decision":"accept"}));
    }
    Ok(codex_denial(method))
}

pub fn claude_args(
    profile: &Profile,
    mode: Mode,
    session: Option<&str>,
    new_session: &str,
) -> Vec<String> {
    let mut args = [
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool=stdio",
        "--permission-prompts",
        "host",
        "--safe-mode",
        "--restricted",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--max-turns",
        "50",
        "--permission-mode",
        match mode {
            Mode::Plan => "plan",
            Mode::Ask => "manual",
            Mode::Build => "acceptEdits",
        },
        "--tools",
        if mode == Mode::Plan {
            "Read,Glob,Grep,AskUserQuestion"
        } else {
            "Read,Glob,Grep,Edit,Write,Bash,AskUserQuestion"
        },
        "--settings",
        "{\"permissions\":{\"ask\":[\"Bash\"]},\"disableAllHooks\":true}",
    ]
    .map(str::to_owned)
    .to_vec();
    if !profile.model.trim().is_empty() {
        args.extend(["--model".into(), profile.model.clone()]);
    }
    if let Some(session) = session {
        args.extend(["--resume".into(), session.into()]);
    } else {
        args.extend(["--session-id".into(), new_session.into()]);
    }
    args
}

async fn run_claude(
    shared: &Shared,
    record: &mut RunRecord,
    control: Arc<Control>,
    prompt: &str,
) -> Result<String, String> {
    let fresh = id();
    let args = claude_args(
        &record.spec.profile,
        record.spec.mode,
        record.native_session.as_deref(),
        &fresh,
    );
    let mut process = JsonProcess::spawn(&record.spec.profile, &args, &record.root).await?;
    let init_id = id();
    process.send(json!({"type":"control_request","request_id":init_id,"request":{"subtype":"initialize"}})).await?;
    process.send(claude_input(prompt)).await?;
    let mut seen = HashSet::new();
    let mut queued = 1_usize;
    record.status = "Running".into();
    publish(shared, record)?;
    let deadline = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(deadline);
    loop {
        process.pause(control.paused())?;
        if control.paused() {
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(150))=>{},_=control.cancelled()=>return Err("Stopped by user".into())}
            continue;
        }
        let event = tokio::select! {event=process.next()=>Some(event?),_=tokio::time::sleep(Duration::from_millis(150))=>None,_=control.cancelled()=>{let _=process.send(json!({"type":"control_request","request_id":id(),"request":{"subtype":"interrupt"}})).await;return Err("Stopped by user".into());},_=&mut deadline=>return Err("Claude Code reached the one-hour limit. Resume explicitly to continue.".into())};
        for (source, note) in control.drain() {
            record.transcript.push(Turn::context(&source, note.clone()));
            process.send(claude_input(&note)).await?;
            queued += 1;
            native_event(
                shared,
                record,
                "native_steering",
                json!({"source":source,"text":note}),
            )?;
        }
        let Some(event) = event else {
            continue;
        };
        if let Some(session) = event["session_id"].as_str() {
            record.native_session = Some(session.into());
        }
        match event["type"].as_str().unwrap_or_default() {
            "control_request" => {
                native_event(
                    shared,
                    record,
                    "native_action_requested",
                    event["request"].clone(),
                )?;
                let response =
                    claude_request(&event["request"], record, &control, &shared.events).await?;
                process.send(json!({"type":"control_response","response":{"subtype":"success","request_id":event["request_id"],"response":response}})).await?;
            }
            "control_response" => {
                if event["response"]["subtype"] == "error" {
                    return Err(format!(
                        "Claude control protocol: {}",
                        event["response"]["error"]
                    ));
                }
            }
            "stream_event" => {
                let part = &event["event"];
                if part["type"] == "content_block_delta"
                    && part["delta"]["type"] == "text_delta"
                    && let Some(text) = part["delta"]["text"].as_str()
                {
                    let _ = shared.events.send(Event::Delta {
                        run: record.spec.id.clone(),
                        text: text.into(),
                    });
                }
            }
            "assistant" => {
                if let Some(blocks) = event["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"] == "text"
                            && let Some(text) = block["text"].as_str()
                        {
                            let key = format!("{}:{text}", event["message"]["id"]);
                            if seen.insert(key) {
                                record.transcript.push(Turn::text("assistant", text));
                            }
                        } else if block["type"] == "tool_use" {
                            native_event(shared, record, "native_tool", block.clone())?;
                        }
                    }
                }
                let _ = shared
                    .events
                    .send(Event::ResetStream(record.spec.id.clone()));
                publish(shared, record)?;
            }
            "user" => {
                native_event(
                    shared,
                    record,
                    "native_tool_output",
                    event["message"].clone(),
                )?;
            }
            "result" => {
                native_event(shared, record, "native_result", event.clone())?;
                record.rounds += 1;
                let usage = &event["usage"];
                record.usage.add(&Usage {
                    input: usage["input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                    output: usage["output_tokens"].as_u64().unwrap_or(0),
                    cached: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                });
                if event["is_error"] == true || event["subtype"] != "success" {
                    return Err(format!(
                        "Claude Code: {}",
                        event["result"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| event["errors"].to_string())
                    ));
                }
                record.report = event["result"].as_str().unwrap_or_default().to_string();
                if record.report.is_empty() {
                    record.report = record
                        .transcript
                        .iter()
                        .rev()
                        .find(|t| t.role == "assistant")
                        .map(|t| {
                            t.blocks
                                .iter()
                                .filter_map(super::provider::block_text)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                }
                queued = queued.saturating_sub(1);
                if queued == 0 {
                    if record.report.trim().is_empty() {
                        record.report="Claude Code completed without a text response. See Activity for native tool results.".into();
                    }
                    let last = record
                        .transcript
                        .iter()
                        .rev()
                        .find(|t| t.role == "assistant")
                        .map(|t| {
                            t.blocks
                                .iter()
                                .filter_map(super::provider::block_text)
                                .collect::<Vec<_>>()
                                .join("\n")
                        });
                    if last.as_deref() != Some(&record.report) {
                        record
                            .transcript
                            .push(Turn::text("assistant", record.report.clone()));
                    }
                    process.exit().await?;
                    return Ok(record.report.clone());
                }
            }
            "system" => {
                native_event(shared, record, "native_system", event.clone())?;
                publish(shared, record)?;
            }
            _ => {}
        }
    }
}

fn claude_input(text: &str) -> Value {
    json!({"type":"user","uuid":id(),"message":{"role":"user","content":text},"parent_tool_use_id":null})
}

async fn claude_request(
    request: &Value,
    record: &RunRecord,
    control: &Control,
    events: &mpsc::Sender<Event>,
) -> Result<Value, String> {
    if request["subtype"] != "can_use_tool" {
        return Ok(json!({"behavior":"deny","message":"Unsupported native permission request"}));
    }
    let tool = request["tool_name"].as_str().unwrap_or_default();
    let input = &request["input"];
    if tool == "AskUserQuestion" {
        let mut updated = input.clone();
        let mut answers = serde_json::Map::new();
        if let Some(questions) = input["questions"].as_array() {
            for question in questions {
                let text = question["question"].as_str().unwrap_or("Claude question");
                let answer = control
                    .ask(
                        &record.spec.id,
                        format!("{text}\n{}", question["options"]),
                        false,
                        events,
                    )
                    .await?;
                answers.insert(text.into(), json!(answer));
            }
        }
        updated["answers"] = json!(answers);
        return Ok(json!({"behavior":"allow","updatedInput":updated}));
    }
    let read = matches!(tool, "Read" | "Glob" | "Grep");
    if record.spec.mode == Mode::Plan && !read {
        return Ok(
            json!({"behavior":"deny","message":"Luxe Plan mode permits only reading and search."}),
        );
    }
    for key in ["file_path", "path"] {
        if let Some(path) = input[key].as_str()
            && let Err(error) = tools::resolve(&record.root, path)
        {
            return Ok(json!({"behavior":"deny","message":error}));
        }
    }
    if read {
        return Ok(json!({"behavior":"allow","updatedInput":input}));
    }
    let answer = control
        .ask(
            &record.spec.id,
            format!(
                "Claude Code requests {tool}\n{}\nWorkspace: {}",
                serde_json::to_string_pretty(input).unwrap_or_default(),
                record.root.display()
            ),
            true,
            events,
        )
        .await?;
    if answer == "allow" {
        Ok(json!({"behavior":"allow","updatedInput":input}))
    } else {
        Ok(json!({"behavior":"deny","message":"The user denied this action in Luxe."}))
    }
}

#[cfg(test)]
mod tests;
