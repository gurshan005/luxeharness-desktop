use super::*;

#[test]
fn native_modes_never_request_unrestricted_codex_access() {
    assert_eq!(codex_policy(Mode::Plan), ("read-only", "never"));
    assert_eq!(codex_policy(Mode::Ask), ("read-only", "untrusted"));
    assert_eq!(codex_policy(Mode::Build), ("workspace-write", "untrusted"));
}

#[test]
fn claude_plan_excludes_commands_and_edits_and_preserves_native_login() {
    let args = claude_args(&Profile::default(), Mode::Plan, None, "test-session");
    let tools = args[args.iter().position(|a| a == "--tools").unwrap() + 1].clone();
    assert_eq!(tools, "Read,Glob,Grep,AskUserQuestion");
    assert!(args.contains(&"--safe-mode".to_string()));
    assert!(!args.iter().any(|a| a.contains("bypassPermissions")
        || a.contains("dangerously-skip")
        || a == "--bare"));
    assert!(args.contains(&"--permission-prompt-tool=stdio".into()));
}

#[test]
fn native_sessions_resume_the_saved_identifier() {
    let args = claude_args(
        &Profile::default(),
        Mode::Build,
        Some("saved-session"),
        "unused",
    );
    assert!(args.windows(2).any(|w| w == ["--resume", "saved-session"]));
    assert!(!args.contains(&"--session-id".into()));
}

#[test]
fn native_child_receives_a_full_context_fork_once_then_uses_its_own_session() {
    let mut record = RunRecord {
        spec: crate::harness::model::RunSpec {
            id: id(),
            task: "Find the named evidence".into(),
            workspace: PathBuf::from("."),
            profile: Profile {
                provider: Provider::Codex,
                ..Profile::default()
            },
            mode: Mode::Plan,
            isolation: crate::harness::model::Isolation::Shared,
            parent: Some("parent".into()),
            check_only: false,
        },
        status: "Queued".into(),
        root: PathBuf::from("."),
        branch: None,
        rounds: 0,
        context: 0,
        usage: Usage::default(),
        report: String::new(),
        transcript: vec![
            Turn::context("workspace", "native-fork-evidence"),
            Turn::text("user", "Find the named evidence"),
        ],
        started: 0,
        native_session: None,
        replay_only: false,
    };
    let prompt = next_prompt(&record);
    assert!(prompt.contains("native-fork-evidence") && prompt.contains("Find the named evidence"));
    record.native_session = Some("native-session".into());
    assert_eq!(next_prompt(&record), "Find the named evidence");
}

#[test]
fn login_rejects_lookalike_hosts_and_non_https_destinations() {
    assert!(
        validate_login_url(
            "https://auth.openai.com/oauth/authorize?state=test",
            Provider::Codex
        )
        .is_ok()
    );
    assert!(
        validate_login_url("https://auth.openai.com.example.invalid/", Provider::Codex).is_err()
    );
    assert!(validate_login_url("http://auth.openai.com/", Provider::Codex).is_err());
    assert!(validate_login_url("https://user:pass@auth.openai.com/", Provider::Codex).is_err());
}

fn fixture(provider: Provider, script: &str) -> (tempfile::TempDir, Profile) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent");
    std::fs::write(&path, format!("#!/usr/bin/env python3\n{script}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = Profile {
        provider,
        executable: path.to_string_lossy().into_owned(),
        name: provider.to_string(),
        model: String::new(),
        ..Profile::default()
    };
    (dir, profile)
}

fn start(engine: &crate::harness::Engine, root: &Path, profile: Profile, mode: Mode) -> String {
    let spec = crate::harness::model::RunSpec {
        id: id(),
        task: "Read README.md and report the evidence.".into(),
        workspace: root.into(),
        profile,
        mode,
        isolation: crate::harness::model::Isolation::Shared,
        parent: None,
        check_only: false,
    };
    engine
        .start(spec, crate::harness::model::Settings::default(), None)
        .unwrap()
}

fn completed(engine: &crate::harness::Engine) -> RunRecord {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        for event in engine.drain() {
            match event {
                Event::Record(record)
                    if matches!(record.status.as_str(), "Completed" | "Failed" | "Stopped") =>
                {
                    return *record;
                }
                Event::Question { run, id, .. } => engine.answer(&run, &id, "deny".into()),
                _ => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Native fixture did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

const CODEX_FIXTURE: &str = r#"import json,sys
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); rid=r.get('id'); p=r.get('params',{})
 if m=='initialize': emit({'id':rid,'result':{'userAgent':'fixture'}})
 elif m=='initialized': pass
 elif m in ['thread/start','thread/resume']:
  if m=='thread/resume' and p['threadId']!='native-thread': sys.exit(3)
  emit({'id':rid,'result':{'thread':{'id':'native-thread'}}})
 elif m=='turn/start':
  emit({'id':rid,'result':{'turn':{'id':'native-turn'}}})
  emit({'method':'item/agentMessage/delta','params':{'delta':'Observed disk evidence'}})
  emit({'id':900,'method':'item/commandExecution/requestApproval','params':{'command':'touch forbidden','cwd':'.'}})
 elif rid==900:
  if r['result']['decision']!='decline': sys.exit(4)
  emit({'method':'item/completed','params':{'item':{'type':'agentMessage','id':'answer','phase':'final_answer','text':'Observed disk evidence'}}})
  emit({'method':'thread/tokenUsage/updated','params':{'tokenUsage':{'total':{'inputTokens':20,'outputTokens':5,'cachedInputTokens':10}}}})
  emit({'method':'turn/completed','params':{'turn':{'id':'native-turn','status':'completed'}}})
"#;

#[test]
fn codex_reads_guidance_in_its_actual_workspace_and_cannot_resume_in_another() {
    let script = r#"import json,sys,os
from pathlib import Path
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); rid=r.get('id'); p=r.get('params',{})
 if m=='initialize': emit({'id':rid,'result':{'userAgent':'workspace-fixture'}})
 elif m=='initialized': pass
 elif m in ['thread/start','thread/resume']:
  assert Path(p['cwd']).resolve()==Path.cwd()
  assert 'AGENTS.md' in p['developerInstructions'] and 'MEMORY.md' in p['developerInstructions']
  evidence=Path('AGENTS.md').read_text()+' | '+Path('MEMORY.md').read_text()
  emit({'id':rid,'result':{'thread':{'id':'workspace-thread'}}})
 elif m=='turn/start':
  emit({'id':rid,'result':{'turn':{'id':'workspace-turn'}}})
  emit({'method':'item/completed','params':{'item':{'type':'agentMessage','id':'answer','phase':'final_answer','text':evidence}}})
  emit({'method':'turn/completed','params':{'turn':{'id':'workspace-turn','status':'completed'}}})
"#;
    let (_agent, profile) = fixture(Provider::Codex, script);
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for (root, marker) in [(first.path(), "Alpha"), (second.path(), "Beta")] {
        std::fs::write(root.join("AGENTS.md"), format!("{marker} rules")).unwrap();
        std::fs::write(root.join("MEMORY.md"), format!("{marker} decisions")).unwrap();
    }
    let data = tempfile::tempdir().unwrap();
    let store = crate::harness::store::Store {
        root: data.path().into(),
    };
    store
        .save(
            "settings.json",
            &crate::harness::model::Settings {
                profiles: vec![profile],
                workspace: Some(first.path().canonicalize().unwrap()),
                ..crate::harness::model::Settings::default()
            },
        )
        .unwrap();
    let mut panel = crate::agent_ui::Panel::new(store);
    let first_run = panel.launch("Read project guidance".into(), false).unwrap();
    let _ = panel.update(crate::agent_ui::Action::Workspace(
        crate::workspace_ui::Action::Select(second.path().into()),
    ));
    let original = completed(&panel.engine);
    assert_eq!(original.spec.id, first_run);
    assert_eq!(original.status, "Completed", "{}", original.report);
    assert!(original.report.contains("Alpha rules | Alpha decisions"));
    assert_eq!(original.root, first.path().canonicalize().unwrap());
    assert_eq!(
        panel.settings.workspace,
        Some(second.path().canonicalize().unwrap())
    );
    panel.runs.push(original.clone());
    assert!(
        panel
            .continue_chat(&first_run, "Do more work".into())
            .is_none()
    );
    assert!(panel.notice.contains("original workspace"));
    panel.launch("Read project guidance".into(), false).unwrap();
    let other = completed(&panel.engine);
    assert_eq!(other.status, "Completed", "{}", other.report);
    assert!(other.report.contains("Beta rules | Beta decisions"));
    assert!(!other.report.contains("Alpha"));
    let mut wrong_workspace = original.spec.clone();
    wrong_workspace.workspace = second.path().into();
    let error = panel
        .engine
        .start(
            wrong_workspace,
            crate::harness::model::Settings::default(),
            Some(original),
        )
        .unwrap_err();
    assert!(error.contains("original workspace"));
}

#[test]
fn codex_native_protocol_streams_denies_escalation_and_resumes_its_thread() {
    let script = CODEX_FIXTURE;
    let (_agent, profile) = fixture(Provider::Codex, script);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("README.md"), "# fixture").unwrap();
    let data = tempfile::tempdir().unwrap();
    let store = crate::harness::store::Store {
        root: data.path().into(),
    };
    let engine = crate::harness::Engine::new(store.clone());
    start(&engine, root.path(), profile, Mode::Plan);
    let record = completed(&engine);
    assert_eq!(record.status, "Completed", "{}", record.report);
    assert_eq!(record.native_session.as_deref(), Some("native-thread"));
    assert_eq!(record.report, "Observed disk evidence");
    assert_eq!(record.usage.cached, 10);
    assert!(!root.path().join("forbidden").exists());
    engine
        .start(
            record.spec.clone(),
            crate::harness::model::Settings::default(),
            Some(record),
        )
        .unwrap();
    let resumed = completed(&engine);
    assert_eq!(resumed.status, "Completed");
    assert_eq!(resumed.rounds, 2);
    assert_eq!(store.runs().unwrap().len(), 1);
}

#[test]
fn claude_native_permission_bridge_denies_plan_writes_and_requires_success() {
    let script=r#"import json,sys
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line)
 if r.get('type')=='control_request': emit({'type':'control_response','response':{'subtype':'success','request_id':r['request_id'],'response':{}}})
 elif r.get('type')=='user':
  emit({'type':'system','subtype':'init','session_id':'native-claude-session'})
  emit({'type':'control_request','request_id':'tool','request':{'subtype':'can_use_tool','tool_name':'Write','input':{'file_path':'blocked.txt','content':'never'}}})
 elif r.get('type')=='control_response':
  if r['response']['response']['behavior']!='deny': sys.exit(5)
  emit({'type':'assistant','message':{'id':'answer','content':[{'type':'text','text':'Plan write was refused.'}]},'session_id':'native-claude-session'})
  emit({'type':'result','subtype':'success','is_error':False,'result':'Plan write was refused.','usage':{'input_tokens':20,'output_tokens':5},'session_id':'native-claude-session'})
"#.replace("\n+","\n");
    let (_agent, profile) = fixture(Provider::ClaudeCode, &script);
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let engine = crate::harness::Engine::new(crate::harness::store::Store {
        root: data.path().into(),
    });
    start(&engine, root.path(), profile, Mode::Plan);
    let record = completed(&engine);
    assert_eq!(record.status, "Completed", "{}", record.report);
    assert_eq!(
        record.native_session.as_deref(),
        Some("native-claude-session")
    );
    assert!(!root.path().join("blocked.txt").exists());
}

#[test]
fn a_native_stream_ending_without_completion_is_a_failure() {
    let script = "import json\nprint(json.dumps({'type':'assistant','message':{'id':'partial','content':[{'type':'text','text':'unfinished'}]}}),flush=True)\n";
    let (_agent, profile) = fixture(Provider::ClaudeCode, script);
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let engine = crate::harness::Engine::new(crate::harness::store::Store {
        root: data.path().into(),
    });
    start(&engine, root.path(), profile, Mode::Plan);
    assert_eq!(completed(&engine).status, "Failed");
}

fn live(provider: Provider) {
    let store = crate::harness::store::Store::open();
    let engine = crate::harness::Engine::new(store.clone());
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let before = std::fs::read(workspace.join("README.md")).unwrap();
    let profile = Profile {
        provider,
        name: provider.to_string(),
        model: String::new(),
        ..Profile::default()
    };
    let spec=crate::harness::model::RunSpec{id:id(),task:"Native integration check: read README.md in this workspace and return its first Markdown heading plus one sentence about the project. Do not change files or run build commands.".into(),workspace:workspace.clone(),profile,mode:Mode::Plan,isolation:crate::harness::model::Isolation::Shared,parent:None,check_only:false};
    let run = engine
        .start(spec, crate::harness::model::Settings::default(), None)
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        for event in engine.drain() {
            if let Event::Record(record) = event
                && record.spec.id == run
                && matches!(record.status.as_str(), "Completed" | "Failed" | "Stopped")
            {
                println!("{}: {}\n{}", provider, record.status, record.report);
                assert_eq!(record.status, "Completed", "{}", record.report);
                assert!(record.native_session.is_some());
                assert!(!record.report.trim().is_empty());
                assert_eq!(std::fs::read(workspace.join("README.md")).unwrap(), before);
                return;
            }
        }
        if std::time::Instant::now() > deadline {
            engine.stop(&run);
            panic!("Live native test timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires an installed Codex with a real signed-in account; writes a visible local smoke-test run"]
fn live_codex_workspace() {
    live(Provider::Codex);
}

#[test]
#[ignore = "requires an installed Claude Code with a real signed-in account; writes a visible local smoke-test run"]
fn live_claude_workspace() {
    live(Provider::ClaudeCode);
}

#[test]
fn managed_codex_login_emits_a_browser_link_and_finishes_without_exporting_tokens() {
    let script = r#"import json,sys
signed=False
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); rid=r.get('id')
 if m=='initialize': emit({'id':rid,'result':{}})
 elif m=='initialized': pass
 elif m=='account/read': emit({'id':rid,'result':{'account':{'type':'chatgpt'} if signed else None,'requiresOpenaiAuth':True}})
 elif m=='account/login/start':
  signed=True
  emit({'id':rid,'result':{'type':'chatgpt','loginId':'login-fixture','authUrl':'https://auth.openai.com/oauth/authorize?state=fixture'}})
  emit({'method':'account/login/completed','params':{'loginId':'login-fixture','success':True}})
 elif m=='model/list': emit({'id':rid,'result':{'data':[{'model':'native-model','isDefault':True}]}})
"#;
    let (_agent, profile) = fixture(Provider::Codex, script);
    let (events, receiver) = mpsc::channel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(authenticate(
            profile,
            true,
            Arc::new(Control::new(Mode::Plan)),
            events,
        ))
        .unwrap();
    let states = receiver
        .try_iter()
        .filter_map(|e| {
            if let Event::NativeStatus(s) = e {
                Some(s)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(states.iter().any(|s| s.login_url.is_some() && s.busy));
    assert!(states.last().unwrap().authenticated);
    assert_eq!(states.last().unwrap().models, vec!["native-model"]);
}

#[test]
fn codex_and_claude_native_processes_share_a_workspace_with_separate_runs() {
    let (_codex, codex) = fixture(Provider::Codex, CODEX_FIXTURE);
    let script = r#"import sys,json
for line in sys.stdin:
 r=json.loads(line)
 if r.get('type')=='control_request': print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':r['request_id'],'response':{}}}),flush=True)
 elif r.get('type')=='user':
  print(json.dumps({'type':'assistant','session_id':'claude-independent','message':{'id':'claude-answer','content':[{'type':'text','text':'Claude evidence'}]}}),flush=True)
  print(json.dumps({'type':'result','session_id':'claude-independent','subtype':'success','is_error':False,'result':'Claude evidence'}),flush=True)
"#;
    let (_claude, claude) = fixture(Provider::ClaudeCode, script);
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let engine = crate::harness::Engine::new(crate::harness::store::Store {
        root: data.path().into(),
    });
    let first = start(&engine, root.path(), codex, Mode::Plan);
    let second = start(&engine, root.path(), claude, Mode::Plan);
    let mut records = std::collections::HashMap::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while records.len() < 2 {
        for event in engine.drain() {
            if let Event::Record(record) = event
                && matches!(record.status.as_str(), "Completed" | "Failed" | "Stopped")
            {
                records.insert(record.spec.id.clone(), *record);
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Parallel native agents did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        records[&first].status, "Completed",
        "{}",
        records[&first].report
    );
    assert_eq!(
        records[&second].status, "Completed",
        "{}",
        records[&second].report
    );
    assert_eq!(records[&first].root, records[&second].root);
    assert_ne!(
        records[&first].native_session,
        records[&second].native_session
    );
    assert!(!records[&first].report.contains("Claude evidence"));
    assert_eq!(records[&second].report, "Claude evidence");
}
