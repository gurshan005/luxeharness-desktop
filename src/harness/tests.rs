use super::*;
use model::{Profile, Provider};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn store(dir: &tempfile::TempDir) -> Store {
    Store {
        root: dir.path().join("data"),
    }
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# Example\nEvidence from disk\n",
    )
    .unwrap();
    dir
}
fn profile(endpoint: String) -> Profile {
    Profile {
        provider: Provider::Compatible,
        model: "test-model".into(),
        endpoint,
        key_env: String::new(),
        ..Profile::default()
    }
}
fn spec(root: PathBuf, profile: Profile) -> RunSpec {
    RunSpec {
        id: id(),
        task: "Read README.md and report evidence".into(),
        workspace: root,
        profile,
        mode: Mode::Plan,
        isolation: Isolation::Shared,
        parent: None,
        check_only: false,
    }
}

struct Fake {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Fake {
    fn new(reply: impl Fn(usize, &Value) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let headers = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (captured, stopped) = (requests.clone(), stop.clone());
        let captured_headers = headers.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 8192];
                let (header_end, length) = loop {
                    let n = socket.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(start) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..start]).to_lowercase();
                        let length: usize = header
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        break (start + 4, length);
                    }
                };
                while bytes.len() < header_end + length {
                    let n = socket.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let value: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                captured_headers
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes[..header_end]).into_owned());
                let index = {
                    let mut captured = captured.lock().unwrap();
                    captured.push(value.clone());
                    captured.len() - 1
                };
                let response = reply(index, &value);
                let _ = socket.write_all(response.as_bytes());
            }
        });
        Self {
            endpoint,
            requests,
            headers,
            stop,
            thread: Some(thread),
        }
    }
}

fn sse(parts: &[Value]) -> String {
    let mut body = parts
        .iter()
        .map(|part| format!("data: {part}\n\n"))
        .collect::<String>();
    body.push_str("data: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn answer(text: &str) -> String {
    sse(&[
        json!({"choices":[{"delta":{"content":text},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":80}}}),
    ])
}
fn tool(name: &str, input: Value) -> String {
    sse(&[
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":name,"arguments":input.to_string()}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ])
}

fn wait(engine: &Engine, count: usize) -> Vec<RunRecord> {
    let start = std::time::Instant::now();
    let mut records = HashMap::new();
    while start.elapsed() < Duration::from_secs(10) {
        for event in engine.drain() {
            if let Event::Record(record) = event
                && matches!(record.status.as_str(), "Completed" | "Failed" | "Stopped")
            {
                records.insert(record.spec.id.clone(), *record);
            }
        }
        if records.len() >= count {
            return records.into_values().collect();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("Runs did not finish; got {}", records.len());
}

#[test]
fn parallel_agents_keep_separate_transcripts_and_stable_prefixes() {
    let fake = Fake::new(|_, body| {
        if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
        {
            answer("README.md:2 confirms the evidence.")
        } else {
            tool(
                "read_file",
                json!({"path":"README.md","offset":1,"limit":2}),
            )
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let store = store(&data);
    let engine = Engine::new(store.clone());
    let profile = profile(fake.endpoint.clone());
    let settings = Settings {
        profiles: vec![profile.clone()],
        ..Settings::default()
    };
    for _ in 0..2 {
        engine
            .start(
                spec(root.path().into(), profile.clone()),
                settings.clone(),
                None,
            )
            .unwrap();
    }
    let records = wait(&engine, 2);
    assert_ne!(records[0].spec.id, records[1].spec.id);
    for record in &records {
        assert_eq!(record.status, "Completed", "{}", record.report);
        assert_eq!(record.rounds, 2);
        assert_eq!(record.usage.input, 100);
        assert!(record.transcript.iter().flat_map(|t| &t.blocks).any(
            |b| matches!(b,Block::ToolResult{text,..} if text.contains("Evidence from disk"))
        ));
    }
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let first = requests
        .iter()
        .find(|r| r["messages"].as_array().unwrap().len() == 3)
        .unwrap();
    let second = requests
        .iter()
        .find(|r| r["messages"].as_array().unwrap().len() > 3)
        .unwrap();
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(
        &second["messages"].as_array().unwrap()[..3],
        first["messages"].as_array().unwrap()
    );
    assert_eq!(store.runs().unwrap().len(), 2);
}

#[test]
fn plan_mode_denies_model_writes_and_returns_the_error_to_the_model() {
    let fake = Fake::new(|_, body| {
        if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
        {
            answer("The write was denied in Plan mode.")
        } else {
            tool("write_file", json!({"path":"unwanted.txt","content":"no"}))
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    engine
        .start(
            spec(root.path().into(), profile(fake.endpoint.clone())),
            Settings::default(),
            None,
        )
        .unwrap();
    let records = wait(&engine, 1);
    assert_eq!(records[0].status, "Completed");
    assert!(!root.path().join("unwanted.txt").exists());
    let requests = fake.requests.lock().unwrap();
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("Plan mode"))
    );
}

#[test]
fn stale_edits_ambiguous_snippets_symlinks_and_path_escapes_are_refused() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let control = Arc::new(Control::new(Mode::Build));
    let (tx, _) = mpsc::channel();
    let mut tools = Toolbox::new(root.clone(), id(), control, tx, store(&data), false);
    runtime().block_on(async {
        tools
            .execute("read_file", &json!({"path":"README.md"}))
            .await
            .unwrap();
        std::fs::write(root.join("README.md"), "human edit").unwrap();
        assert!(
            tools
                .execute(
                    "edit_file",
                    &json!({"path":"README.md","old":"Example","new":"Overwrite"})
                )
                .await
                .unwrap_err()
                .contains("changed")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "human edit"
        );
        tools
            .execute("read_file", &json!({"path":"README.md"}))
            .await
            .unwrap();
        tools
            .execute(
                "edit_file",
                &json!({"path":"README.md","old":"human edit","new":"verified edit"}),
            )
            .await
            .unwrap();
        tools
            .execute(
                "edit_file",
                &json!({"path":"README.md","old":"verified edit","new":"next edit"}),
            )
            .await
            .unwrap();
        assert!(
            tools
                .execute("edit_file", &json!({"path":"README.md","old":"","new":"x"}))
                .await
                .is_err()
        );
    });
    assert!(tools::resolve(&root, "../outside.txt").is_err());
    assert!(tools::resolve(&root, ".env").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(data.path(), root.join("escape")).unwrap();
        assert!(tools::resolve(&root, "escape/secret.txt").is_err());
    }
}

#[test]
fn stopped_approval_never_executes_and_queued_steering_survives_restart() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let store = store(&data);
    let run = id();
    let (tx, rx) = mpsc::channel();
    let control = Arc::new(Control::new(Mode::Ask));
    control
        .attach(store.clone(), run.clone(), tx.clone())
        .unwrap();
    control.steer("user", "preserve the public API".into());
    let restored = Control::new(Mode::Ask);
    restored.attach(store, run.clone(), tx.clone()).unwrap();
    assert_eq!(
        restored.drain(),
        vec![("user".into(), "preserve the public API".into())]
    );
    let tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        run,
        control.clone(),
        tx,
        Store {
            root: data.path().join("tools"),
        },
        false,
    );
    runtime().block_on(async{
        let input=json!({"path":"blocked.txt","content":"blocked"});
        let gate=tools.gate("write_file",&input);
        tokio::pin!(gate);
        tokio::select!{result=&mut gate=>panic!("Gate returned without an answer: {result:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
        assert!(rx.try_iter().any(|e|matches!(e,Event::Question{..})));control.stop();assert!(gate.await.is_err());
    });
    assert!(!root.path().join("blocked.txt").exists());
}

#[test]
fn competing_shared_writers_are_rejected() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    let mut first = spec(
        root.path().canonicalize().unwrap(),
        profile("http://127.0.0.1:1/v1".into()),
    );
    first.mode = Mode::Build;
    register(&engine.shared, &first, Arc::new(Control::new(Mode::Build))).unwrap();
    let mut second = first.clone();
    second.id = id();
    assert!(register(&engine.shared, &second, Arc::new(Control::new(Mode::Build))).is_err());
    second.isolation = Isolation::Worktree;
    register(&engine.shared, &second, Arc::new(Control::new(Mode::Build))).unwrap();
}

#[test]
fn separate_worktrees_have_distinct_branches_and_leave_the_source_untouched() {
    let root = fixture();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "user.name", "Luxe test"],
        vec!["add", "README.md"],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    for _ in 0..2 {
        let mut spec = spec(root.path().into(), Profile::default());
        spec.check_only = true;
        spec.isolation = Isolation::Worktree;
        engine.start(spec, Settings::default(), None).unwrap();
    }
    let records = wait(&engine, 2);
    for record in &records {
        assert_eq!(record.status, "Completed", "{}", record.report);
        assert!(record.root.join("README.md").exists());
        assert_ne!(record.root, root.path());
    }
    assert_ne!(records[0].root, records[1].root);
    assert_ne!(records[0].branch, records[1].branch);
    assert_eq!(
        std::fs::read_to_string(root.path().join("README.md")).unwrap(),
        "# Example\nEvidence from disk\n"
    );
}

#[test]
fn compaction_and_recovery_never_leave_orphaned_tool_results() {
    let mut turns = vec![
        Turn::text("user", "task"),
        Turn {
            role: "assistant".into(),
            blocks: vec![Block::ToolCall {
                id: "pending".into(),
                name: "read_file".into(),
                input: json!({"path":"x"}),
            }],
            wire: None,
        },
    ];
    repair_interrupted(&mut turns);
    assert!(matches!(
        turns.last().unwrap().blocks[0],
        Block::ToolResult { error: true, .. }
    ));
    let compacted = compact(
        &turns,
        "Task, constraints, decisions, files, verification, next steps".into(),
    );
    assert!(matches!(compacted[0].blocks[0], Block::Context { .. }));
    assert!(
        !compacted.iter().skip(1).any(|t| t
            .blocks
            .iter()
            .any(|b| matches!(b, Block::ToolResult { .. })))
            || compacted.len() == 4
    );
}

#[test]
fn compaction_keeps_recent_verbatim_tool_work_in_one_long_user_turn() {
    let mut turns = vec![Turn::text("user", "long task")];
    for round in 0..10 {
        let id = format!("call-{round}");
        turns.push(Turn {
            role: "assistant".into(),
            blocks: vec![Block::ToolCall {
                id: id.clone(),
                name: "read_file".into(),
                input: json!({"path":"code.rs"}),
            }],
            wire: None,
        });
        turns.push(Turn {
            role: "user".into(),
            blocks: vec![Block::ToolResult {
                id,
                text: format!("exact evidence {round}"),
                error: false,
            }],
            wire: None,
        });
    }
    turns.push(Turn::text("assistant", "recent conclusion"));
    let compacted = compact(&turns, "summary".into());
    assert!(compacted.len() >= 7);
    let mut calls = std::collections::HashSet::new();
    for turn in &compacted {
        for block in &turn.blocks {
            match block {
                Block::ToolCall { id, .. } => {
                    calls.insert(id);
                }
                Block::ToolResult { id, .. } => assert!(calls.contains(id)),
                _ => {}
            }
        }
    }
    assert!(
        compacted
            .iter()
            .flat_map(|t| &t.blocks)
            .any(|b| matches!(b,Block::ToolResult{text,..}if text=="exact evidence 9"))
    );
    assert_eq!(compacted.last(), turns.last());
}

#[test]
fn schema_and_child_capability_boundaries_are_enforced() {
    let tools = tools::definitions(true);
    assert!(!tools.iter().any(|t| matches!(
        t.name.as_str(),
        "delegate" | "write_file" | "edit_file" | "run_command"
    )));
    let read = tools.iter().find(|t| t.name == "read_file").unwrap();
    assert!(tools::validate(&read.parameters, &json!({"path":15})).is_err());
    assert!(tools::validate(&read.parameters, &json!({"path":"x","unexpected":true})).is_err());
    let profile = profile("http://example.com/v1".into());
    assert!(profile.validate().is_err());
}

#[test]
fn corrupt_storage_is_not_silently_replaced() {
    let data = tempfile::tempdir().unwrap();
    let store = store(&data);
    std::fs::create_dir_all(&store.root).unwrap();
    std::fs::write(store.root.join("settings.json"), "not json").unwrap();
    assert!(store.load::<Settings>("settings.json").is_err());
    assert_eq!(
        std::fs::read_to_string(store.root.join("settings.json")).unwrap(),
        "not json"
    );
}

#[test]
fn transient_failures_retry_the_request_without_repeating_tools() {
    let fake = Fake::new(|index, _| {
        if index == 0 {
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
        } else {
            answer("Recovered")
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    engine
        .start(
            spec(root.path().into(), profile(fake.endpoint.clone())),
            Settings::default(),
            None,
        )
        .unwrap();
    let records = wait(&engine, 1);
    assert_eq!(records[0].status, "Completed");
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
}

#[test]
fn delegation_routes_to_a_second_profile_and_returns_only_its_report() {
    let child_id = id();
    let selected_child = child_id.clone();
    let fake = Fake::new(move |_, body| {
        if body["model"] == "child-model" {
            return answer("Child evidence: README.md:2");
        }
        let messages = body["messages"].to_string();
        if messages.contains("Child evidence") {
            answer("Parent verified the child report.")
        } else if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
        {
            answer("Waiting for the exploration report.")
        } else {
            tool(
                "delegate",
                json!({"task":"Read README.md; return one cited fact.","profile_id":selected_child,"context":"fresh"}),
            )
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    let parent = profile(fake.endpoint.clone());
    let mut child = parent.clone();
    child.id = child_id;
    child.model = "child-model".into();
    let settings = Settings {
        profiles: vec![parent.clone(), child],
        ..Settings::default()
    };
    let mut spec = spec(root.path().into(), parent);
    spec.task = "parent-only-private-context: delegate a file lookup".into();
    let parent_id = spec.id.clone();
    engine.start(spec, settings, None).unwrap();
    let records = wait(&engine, 2);
    assert!(
        records.iter().all(|r| r.status == "Completed"),
        "{:?}",
        records.iter().map(|r| &r.report).collect::<Vec<_>>()
    );
    let child = records.iter().find(|r| r.spec.parent.is_some()).unwrap();
    assert_eq!(child.spec.parent.as_ref(), Some(&parent_id));
    assert!(
        !serde_json::to_string(&child.transcript)
            .unwrap()
            .contains("parent-only-private-context")
    );
    assert!(records.iter().find(|r|r.spec.parent.is_none()).unwrap().transcript.iter().flat_map(|t|&t.blocks).any(|b|matches!(b,Block::Context{source,text} if source=="subagent"&&text.contains("Child evidence"))));
}

#[test]
fn a_timer_wakes_an_idle_agent_without_model_polling() {
    let fake = Fake::new(|_, body| {
        let messages = body["messages"].to_string();
        if messages.contains("timer-evidence-ready") && messages.contains("source=\\\"timer\\\"") {
            answer("Timer completed.")
        } else if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
        {
            answer("Waiting until the scheduled wakeup.")
        } else {
            tool(
                "schedule_wakeup",
                json!({"seconds":1,"prompt":"timer-evidence-ready"}),
            )
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    engine
        .start(
            spec(root.path().into(), profile(fake.endpoint.clone())),
            Settings::default(),
            None,
        )
        .unwrap();
    let records = wait(&engine, 1);
    assert_eq!(records[0].status, "Completed", "{}", records[0].report);
    assert_eq!(fake.requests.lock().unwrap().len(), 3);
}

#[test]
fn a_broken_stream_is_discarded_before_any_tool_executes() {
    let fake = Fake::new(|index, _| {
        if index == 0 {
            let body = format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"partial","function":{"name":"write_file","arguments":"{\"path\":\"never.txt\",\"content\":\"bad\"}"}}]},"finish_reason":"tool_calls"}]})
            );
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        } else {
            answer("The retry returned a complete response.")
        }
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let engine = Engine::new(store(&data));
    let mut spec = spec(root.path().into(), profile(fake.endpoint.clone()));
    spec.mode = Mode::Build;
    engine.start(spec, Settings::default(), None).unwrap();
    let records = wait(&engine, 1);
    assert_eq!(records[0].status, "Completed");
    assert!(!root.path().join("never.txt").exists());
    assert_eq!(records[0].rounds, 1);
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
}

#[test]
fn mcp_handshake_discovery_deferred_activation_and_calls_work() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let (tx, _) = mpsc::channel();
    let mut tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        Arc::new(Control::new(Mode::Build)),
        tx,
        store(&data),
        false,
    );
    let program = r#"import sys,json
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 m=r['method']
 result={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}} if m=='initialize' else {'tools':[{'name':'echo','description':'Echo evidence','inputSchema':{'type':'object','properties':{'text':{'type':'string'}},'required':['text']}}]} if m=='tools/list' else {'content':[{'type':'text','text':r['params']['arguments']['text']}]}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let settings = Settings {
        mcp_servers: vec![model::McpServer {
            name: "fixture".into(),
            program: "python3".into(),
            args: vec!["-u".into(), "-c".into(), program.into()],
            enabled: true,
            ..Default::default()
        }],
        ..Settings::default()
    };
    runtime().block_on(async {
        tools.connect(&settings).await.unwrap();
        assert!(!tools.active.iter().any(|t| t.name == "mcp__fixture__echo"));
        tools
            .execute("tool_search", &json!({"query":"echo"}))
            .await
            .unwrap();
        let count = tools.active.len();
        tools
            .execute("tool_search", &json!({"query":"echo"}))
            .await
            .unwrap();
        assert_eq!(tools.active.len(), count);
        assert!(
            tools
                .execute("mcp__fixture__echo", &json!({"text":"actual MCP result"}))
                .await
                .unwrap()
                .contains("actual MCP result")
        );
    });
}

#[test]
fn pre_tool_hooks_veto_and_diagnostics_hooks_return_failures_as_evidence() {
    let root = fixture();
    let hooks = vec![
        model::Hook {
            event: "before_tool".into(),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'blocked by project policy'; exit 2".into(),
            ],
            enabled: true,
        },
        model::Hook {
            event: "after_edit".into(),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'compiler: unresolved symbol'; exit 1".into(),
            ],
            enabled: true,
        },
    ];
    runtime().block_on(async {
        let control = Arc::new(Control::new(Mode::Build));
        assert!(
            extensions::hooks(
                &hooks,
                "before_tool",
                &json!({"tool":"edit_file"}),
                root.path(),
                control.clone()
            )
            .await
            .unwrap_err()
            .contains("blocked by project policy")
        );
        let notes = extensions::hooks(
            &hooks,
            "after_edit",
            &json!({"tool":"edit_file"}),
            root.path(),
            control,
        )
        .await
        .unwrap();
        assert!(notes[0].contains("failed"));
        assert!(notes[0].contains("unresolved symbol"));
    });
}

#[test]
fn stopping_a_process_cancels_its_wait_and_keeps_partial_output() {
    let root = fixture();
    let control = Arc::new(Control::new(Mode::Build));
    runtime().block_on(async{
        let args=vec!["-c".into(),"printf started; sleep 30".into()];
        let run=process::run("/bin/sh",&args,root.path(),60,None,control.clone());tokio::pin!(run);
        let deadline=tokio::time::Instant::now()+Duration::from_secs(2);
        while !control.output().contains("started") {
            tokio::select!{result=&mut run=>panic!("Ended too early: {:?}",result.err()),_=tokio::time::sleep(Duration::from_millis(10))=>{}}
            assert!(tokio::time::Instant::now()<deadline,"Child never emitted its start marker");
        }
        assert!(control.output().contains("started"));control.stop();assert!(run.await.unwrap_err().contains("Stopped"));
    });
}

#[test]
fn remembered_approvals_are_exact_workspace_scoped_revocable_and_never_override_plan() {
    let root = fixture();
    let other = fixture();
    let data = tempfile::tempdir().unwrap();
    let store = store(&data);
    let action = json!({"command":"cargo test","network":false});
    super::permissions::remember(&store, root.path(), "run_command", &action).unwrap();
    assert!(super::permissions::granted(&store, root.path(), "run_command", &action).unwrap());
    assert!(!super::permissions::granted(&store, other.path(), "run_command", &action).unwrap());
    assert!(
        !super::permissions::granted(
            &store,
            root.path(),
            "run_command",
            &json!({"command":"cargo test; rm -rf src","network":false})
        )
        .unwrap()
    );
    assert!(
        !super::permissions::granted(
            &store,
            root.path(),
            "run_command",
            &json!({"command":"cargo test","network":true})
        )
        .unwrap()
    );
    let (tx, _) = mpsc::channel();
    let tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        Arc::new(Control::new(Mode::Plan)),
        tx,
        store.clone(),
        false,
    );
    assert!(
        runtime()
            .block_on(tools.gate("run_command", &action))
            .unwrap_err()
            .contains("Plan")
    );
    super::permissions::revoke(&store).unwrap();
    assert!(!super::permissions::granted(&store, root.path(), "run_command", &action).unwrap());
}

#[test]
fn file_change_notices_preserve_the_stale_write_guard_and_outputs_are_retrievable() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let (tx, _) = mpsc::channel();
    let control = Arc::new(Control::new(Mode::Build));
    let mut tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        control.clone(),
        tx,
        store(&data),
        false,
    );
    runtime().block_on(async {
        tools
            .execute("read_file", &json!({"path":"README.md"}))
            .await
            .unwrap();
        fs::write(root.path().join("README.md"), "Human changed this").unwrap();
        tools.notice_file_changes();
        tools.notice_file_changes();
        assert_eq!(
            control
                .drain()
                .iter()
                .filter(|(source, _)| source == "file_change")
                .count(),
            1
        );
        assert!(
            tools
                .execute(
                    "write_file",
                    &json!({"path":"README.md","content":"clobber"})
                )
                .await
                .is_err()
        );
        let artifact = id();
        tools
            .store
            .save(
                &format!("runs/{}/outputs/{artifact}.json", tools.run),
                &json!({"text":"one\ntwo\nthree\nfour"}),
            )
            .unwrap();
        assert_eq!(
            tools
                .execute("read_output", &json!({"id":artifact,"offset":2,"limit":2}))
                .await
                .unwrap(),
            "2: two\n3: three"
        );
        assert!(
            tools
                .execute("read_output", &json!({"id":"../../settings"}))
                .await
                .is_err()
        );
    });
}

#[test]
#[cfg(target_os = "macos")]
fn sandbox_allows_workspace_work_and_blocks_outside_writes_and_secret_reads() {
    let root = fixture();
    let outside = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::new(Mode::Build));
    let outside_path = outside.path().join("must-not-exist");
    fs::write(root.path().join(".env"), "SECRET_FIXTURE_ONLY").unwrap();
    runtime().block_on(async {
        let output = super::sandbox::run(
            "printf sandbox-ok > proof.txt; cat proof.txt",
            root.path(),
            10,
            false,
            control.clone(),
        )
        .await
        .unwrap();
        assert!(output.success, "{output:?}");
        assert_eq!(
            fs::read_to_string(root.path().join("proof.txt")).unwrap(),
            "sandbox-ok"
        );
        let output = super::sandbox::run(
            &format!("printf escaped > '{}'", outside_path.display()),
            root.path(),
            10,
            false,
            control.clone(),
        )
        .await
        .unwrap();
        assert!(!output.success, "{}", output.text);
        assert!(!outside_path.exists());
        let output = super::sandbox::run("cat .env", root.path(), 10, false, control.clone())
            .await
            .unwrap();
        assert!(!output.success, "{}", output.text);
        assert!(!output.text.contains("SECRET_FIXTURE_ONLY"));
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/config"), "git fixture").unwrap();
        let output = super::sandbox::run(
            "printf clobber > .git/config",
            root.path(),
            10,
            false,
            control.clone(),
        )
        .await
        .unwrap();
        assert!(!output.success);
        assert_eq!(
            fs::read_to_string(root.path().join(".git/config")).unwrap(),
            "git fixture"
        );
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let output = super::sandbox::run(
            "printf escaped > escape/must-not-exist",
            root.path(),
            10,
            false,
            control.clone(),
        )
        .await
        .unwrap();
        assert!(!output.success);
        assert!(!outside_path.exists());
        let (tx, _) = mpsc::channel();
        let data = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(
            root.path().canonicalize().unwrap(),
            id(),
            control,
            tx,
            store(&data),
            false,
        );
        let error = tools
            .execute(
                "run_command",
                &json!({"command":"printf compiler-error >&2; exit 7","background":false}),
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("compiler-error") && error.contains('7'),
            "{error}"
        );
    });
}

#[test]
#[cfg(target_os = "macos")]
fn sandbox_network_access_requires_the_explicit_network_flag() {
    let root = fixture();
    let control = Arc::new(Control::new(Mode::Build));
    let server = Fake::new(|_, _| {
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nnetwork-ok".into()
    });
    let command = format!(
        "/usr/bin/curl -sS --max-time 2 --data '{{}}' '{}'",
        server.endpoint
    );
    runtime().block_on(async {
        let denied = super::sandbox::run(&command, root.path(), 5, false, control.clone())
            .await
            .unwrap();
        assert!(!denied.success);
        assert!(server.requests.lock().unwrap().is_empty());
        let allowed = super::sandbox::run(&command, root.path(), 5, true, control)
            .await
            .unwrap();
        assert!(allowed.success, "{allowed:?}");
        assert_eq!(allowed.text, "network-ok");
    });
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn language_server_diagnostics_deltas_definitions_and_references_use_real_stdio() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.rs"), "original").unwrap();
    let server = r#"import sys,json
docs={}
def send(v):
 b=json.dumps(v).encode();sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(b)).encode()+b);sys.stdout.buffer.flush()
while True:
 headers={}
 while True:
  line=sys.stdin.buffer.readline()
  if not line: sys.exit(0)
  if not line.strip(): break
  k,v=line.decode().split(':',1);headers[k.lower()]=v.strip()
 r=json.loads(sys.stdin.buffer.read(int(headers['content-length'])));m=r.get('method','');p=r.get('params',{})
 if m=='textDocument/didOpen': docs[p['textDocument']['uri']]=p['textDocument']['text']
 if m=='textDocument/didChange': docs[p['textDocument']['uri']]=p['contentChanges'][0]['text']
 if 'id' not in r: continue
 if m=='initialize': result={'capabilities':{'diagnosticProvider':{},'positionEncoding':'utf-16'}}
 elif m=='textDocument/diagnostic':
  uri=p['textDocument']['uri'];items=[{'message':'existing warning','severity':2,'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}}}]
  if 'broken' in docs[uri]: items.append({'message':'new type error','severity':1,'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}}})
  result={'kind':'full','items':items}
 elif m in ('textDocument/definition','textDocument/references'):
  assert p['position']=={'line':0,'character':2};result=[{'uri':p['textDocument']['uri'],'range':{'start':p['position'],'end':p['position']}}]
 else: result={}
 send({'jsonrpc':'2.0','id':r['id'],'result':result})
"#;
    let settings = Settings {
        language_servers: vec![model::LanguageServer {
            name: "fixture".into(),
            program: "python3".into(),
            args: vec!["-u".into(), "-c".into(), server.into()],
            language: "rust".into(),
            extensions: vec!["rs".into()],
            enabled: true,
        }],
        ..Settings::default()
    };
    let (tx, _) = mpsc::channel();
    let control = Arc::new(Control::new(Mode::Build));
    let mut tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        control.clone(),
        tx,
        store(&data),
        false,
    );
    runtime().block_on(async {
        tools.connect(&settings).await.unwrap();
        tools
            .execute("read_file", &json!({"path":"main.rs"}))
            .await
            .unwrap();
        control.drain();
        tools
            .execute(
                "edit_file",
                &json!({"path":"main.rs","old":"original","new":"broken"}),
            )
            .await
            .unwrap();
        let notes = control.drain();
        assert!(
            notes.iter().any(|(source, text)| source == "diagnostics"
                && text.contains("1 new")
                && text.contains("new type error")),
            "{notes:?}"
        );
        for name in ["lsp_definition", "lsp_references"] {
            let result = tools
                .execute(name, &json!({"path":"main.rs","line":1,"character":3}))
                .await
                .unwrap();
            assert!(result.contains("main.rs"));
        }
        assert!(
            tools
                .execute("lsp_diagnostics", &json!({"path":"main.rs"}))
                .await
                .unwrap()
                .contains("new type error")
        );
    });
}

#[test]
fn replay_sends_the_edited_capture_but_never_executes_proposed_tools() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let server = Fake::new(|index, body| {
        if index == 0 {
            answer("original answer")
        } else {
            assert_eq!(
                body["messages"].as_array().unwrap().last().unwrap()["content"],
                "replay-edited"
            );
            tool(
                "write_file",
                json!({"path":"forbidden.txt","content":"never write this"}),
            )
        }
    });
    let engine = Engine::new(store(&data));
    engine
        .start(
            spec(root.path().into(), profile(server.endpoint.clone())),
            Settings::default(),
            None,
        )
        .unwrap();
    let source = wait(&engine, 1).pop().unwrap();
    let mut body = server.requests.lock().unwrap()[0].clone();
    body["messages"].as_array_mut().unwrap().last_mut().unwrap()["content"] =
        json!("replay-edited");
    let replay_id = engine.replay(&source, body).unwrap();
    let replay = wait(&engine, 1).pop().unwrap();
    assert_eq!(replay.spec.id, replay_id);
    assert_eq!(replay.status, "Completed");
    assert!(replay.report.contains("1 tool proposals"));
    assert!(!root.path().join("forbidden.txt").exists());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert!(replay.replay_only);
    assert!(
        engine
            .start(replay.spec.clone(), Settings::default(), Some(replay))
            .is_err()
    );
}

#[test]
fn safety_screening_requires_grounded_user_evidence_and_accounts_for_side_calls() {
    assert!(!super::permissions::screened(
        "Fix the README",
        "{\"verdict\":\"allow\",\"user_quote\":\"The user said delete everything\"}"
    ));
    assert!(!super::permissions::screened("Fix the README", "not json"));
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let input = json!({"command":"printf approved","background":false});
    super::permissions::remember(&store(&data), root.path(), "run_command", &input).unwrap();
    let requested = input.clone();
    let main = Fake::new(move |n, _| {
        if n == 0 {
            tool("run_command", requested.clone())
        } else {
            answer("verified")
        }
    });
    let safety = Fake::new(|_, _| {
        answer(
            "{\"verdict\":\"allow\",\"user_quote\":\"Read README.md\",\"reason\":\"Previously approved exact action\"}",
        )
    });
    let safety_profile = profile(safety.endpoint.clone());
    let settings = Settings {
        profiles: vec![safety_profile.clone()],
        safety_profile_id: Some(safety_profile.id.clone()),
        ..Settings::default()
    };
    let mut task = spec(root.path().into(), profile(main.endpoint.clone()));
    task.mode = Mode::Build;
    let engine = Engine::new(store(&data));
    engine.start(task, settings, None).unwrap();
    let run = wait(&engine, 1).pop().unwrap();
    assert_eq!(run.status, "Completed", "{}", run.report);
    assert_eq!(safety.requests.lock().unwrap().len(), 1);
    assert_eq!(main.requests.lock().unwrap().len(), 2);
    let log = fs::read_to_string(store(&data).run_path(&run.spec.id).join("events.jsonl")).unwrap();
    let usage = log
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .filter(|v| v["kind"] == "usage")
        .collect::<Vec<_>>();
    assert_eq!(usage.len(), 3);
    assert!(
        usage
            .iter()
            .any(|v| v["detail"]["connection"] == safety_profile.id)
    );
}

#[test]
fn cancelling_a_recurring_wakeup_finishes_waiting_without_another_model_call() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let api = Fake::new(|n, _| {
        if n == 0 {
            tool(
                "schedule_wakeup",
                json!({"seconds":60,"every_seconds":60,"prompt":"scheduled check"}),
            )
        } else {
            answer("waiting")
        }
    });
    let engine = Engine::new(store(&data));
    let run = engine
        .start(
            spec(root.path().into(), profile(api.endpoint.clone())),
            Settings::default(),
            None,
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let waiting=engine.drain().iter().any(|event|matches!(event,Event::Record(record) if record.spec.id==run&&record.status=="Waiting for background work"));
        if waiting {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let alarms: Vec<Alarm> = store(&data)
        .load(&format!("runs/{run}/alarms.json"))
        .unwrap()
        .unwrap();
    assert_eq!(alarms[0].repeat, Some(60));
    engine.cancel_wakeup(&run, &alarms[0].id);
    let finished = wait(&engine, 1).pop().unwrap();
    assert_eq!(finished.status, "Completed");
    assert_eq!(api.requests.lock().unwrap().len(), 2);
    let alarms: Vec<Alarm> = store(&data)
        .load(&format!("runs/{run}/alarms.json"))
        .unwrap()
        .unwrap();
    assert!(alarms[0].delivered);
}

#[test]
#[cfg(target_os = "macos")]
fn human_job_controls_stop_one_job_and_task_lists_are_durable() {
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::new(Mode::Build));
    let (tx, _) = mpsc::channel();
    let mut tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        control.clone(),
        tx,
        store(&data),
        false,
    );
    runtime().block_on(async {
        tools
            .execute(
                "todo_write",
                &json!({"items":[{"task":"Verify changes","status":"in_progress"}]}),
            )
            .await
            .unwrap();
        let todos: Value = store(&data)
            .load(&format!("runs/{}/todos.json", tools.run))
            .unwrap()
            .unwrap();
        assert_eq!(todos[0]["task"], "Verify changes");
        tools
            .execute(
                "run_command",
                &json!({"command":"printf started; /bin/sleep 30","background":true}),
            )
            .await
            .unwrap();
        let job = tools.jobs.lock().unwrap().keys().next().unwrap().clone();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !control
                .job_output(&job)
                .unwrap_or_default()
                .contains("started")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(control.stop_job(&job));
        tokio::time::timeout(Duration::from_secs(3), async {
            while tools.jobs.lock().unwrap()[&job].status == "Running" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(tools.jobs.lock().unwrap()[&job].status, "Stopped");
    });
}

#[test]
fn remote_mcp_negotiates_sessions_and_supports_json_sse_resources_and_prompts() {
    let server = Fake::new(|_, request| {
        if request.get("id").is_none() {
            return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        let method = request["method"].as_str().unwrap();
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{},"resources":{},"prompts":{}}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]})
            }
            "tools/call" => {
                json!({"content":[{"type":"text","text":request["params"]["arguments"]["text"]}]})
            }
            "resources/list" => json!({"resources":[{"uri":"fixture://guide","name":"guide"}]}),
            "resources/read" => {
                json!({"contents":[{"uri":"fixture://guide","text":"resource evidence"}]})
            }
            "prompts/list" => json!({"prompts":[{"name":"review"}]}),
            "prompts/get" => {
                json!({"messages":[{"role":"user","content":{"type":"text","text":"Review with evidence"}}]})
            }
            _ => panic!("Unexpected MCP method {method}"),
        };
        let value = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
        let streaming = method == "resources/read" || method == "tools/call";
        let body = if streaming {
            format!("event: message\ndata: {value}\n\n")
        } else {
            value.to_string()
        };
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nMcp-Session-Id: fixture-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
            body.len()
        )
    });
    let root = fixture();
    let data = tempfile::tempdir().unwrap();
    let (tx, _) = mpsc::channel();
    let control = Arc::new(Control::new(Mode::Build));
    let mut tools = Toolbox::new(
        root.path().canonicalize().unwrap(),
        id(),
        control.clone(),
        tx,
        store(&data),
        false,
    );
    let settings = Settings {
        mcp_servers: vec![model::McpServer {
            name: "remote".into(),
            url: server.endpoint.clone(),
            enabled: true,
            ..Default::default()
        }],
        ..Settings::default()
    };
    runtime().block_on(async {
        tools.connect(&settings).await.unwrap();
        for query in ["resources", "prompts", "echo"] {
            tools
                .execute("tool_search", &json!({"query":query}))
                .await
                .unwrap();
        }
        for (name, input, expected) in [
            (
                "mcp__remote__echo",
                json!({"text":"wire evidence"}),
                "wire evidence",
            ),
            (
                "mcp__remote__luxe_resources_list",
                json!({}),
                "fixture://guide",
            ),
            (
                "mcp__remote__luxe_resources_read",
                json!({"uri":"fixture://guide"}),
                "resource evidence",
            ),
            ("mcp__remote__luxe_prompts_list", json!({}), "review"),
            (
                "mcp__remote__luxe_prompts_get",
                json!({"name":"review"}),
                "Review with evidence",
            ),
        ] {
            assert!(
                tools
                    .execute(name, &input)
                    .await
                    .unwrap()
                    .contains(expected)
            );
        }
        control.set_mode(Mode::Plan);
        assert!(
            tools
                .gate(
                    "mcp__remote__luxe_resources_read",
                    &json!({"uri":"fixture://guide"})
                )
                .await
                .is_err()
        );
    });
    let headers = server.headers.lock().unwrap();
    assert!(
        headers
            .iter()
            .skip(1)
            .all(|h| h.to_lowercase().contains("mcp-session-id: fixture-session"))
    );
    assert!(
        super::mcp_http::Http::new(&model::McpServer {
            url: "http://example.com/mcp".into(),
            ..Default::default()
        })
        .is_err()
    );
}
use std::fs;
