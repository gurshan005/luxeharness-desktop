use crate::harness::{
    Engine, extensions,
    model::{
        Block, Event, Isolation, Mode, NativeStatus, Profile, Provider, RunRecord, RunSpec,
        Settings, Trace, id,
    },
    store::Store,
    tools,
};
use crate::icons::{Icon, icon};
use crate::theme::{self, Palette, action as button};
use iced::widget::{
    column, container, pick_list, row, scrollable, text, text_editor, text_input, tooltip,
};
use iced::{Element, Fill, Task};
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub index: usize,
    pub label: String,
}
impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Workspace(crate::workspace_ui::Action),
    ChooseWorkspace,
    ChooseProfile(Choice),
    AddProfile,
    Provider(Provider),
    SummaryProfile(Choice),
    SafetyProfile(Choice),
    ProfileField(&'static str, String),
    SaveProfiles,
    EditTask(text_editor::Action),
    Mode(Mode),
    RunMode(Mode),
    Isolation(Isolation),
    Launch,
    Check,
    Select(String),
    Pause,
    Stop,
    Resume,
    Steer(String),
    SendSteering,
    Approve(String, bool),
    RememberApproval(String),
    RevokeApprovals,
    LoadCapture,
    EditReplay(text_editor::Action),
    Replay,
    ReplayAgain,
    ApprovePlan,
    StopJob(String),
    JobOutput(String),
    CancelWakeup(String),
    Answer(String),
    SubmitAnswer(String),
    Tab(&'static str),
    CopyPath,
    NativeAuth(bool),
    CancelLogin,
    OpenLogin,
    NativeModel(String),
}

pub struct Question {
    pub id: String,
    pub prompt: String,
    pub approval: bool,
}

pub struct Panel {
    pub workspaces: crate::workspace_ui::Panel,
    pub store: Store,
    pub settings: Settings,
    pub engine: Engine,
    pub runs: Vec<RunRecord>,
    pub selected: Option<String>,
    pub profile: usize,
    pub mode: Mode,
    pub isolation: Isolation,
    pub notice: String,
    pub task: text_editor::Content,
    steering: String,
    answer: String,
    context_field: String,
    streams: HashMap<String, String>,
    questions: HashMap<String, Vec<Question>>,
    traces: Vec<Trace>,
    paused: HashSet<String>,
    tab: &'static str,
    settings_ok: bool,
    native_status: HashMap<String, NativeStatus>,
    replay_editor: text_editor::Content,
    job_output: Option<(String, String)>,
}

impl Panel {
    pub fn new(store: Store) -> Self {
        let mut notice = String::new();
        let (mut settings, settings_ok) = match store.load::<Settings>("settings.json") {
            Ok(settings) => (settings.unwrap_or_default(), true),
            Err(e) => {
                notice = e;
                (Settings::default(), false)
            }
        };
        if settings.profiles.is_empty() {
            settings.profiles = Provider::ALL
                .into_iter()
                .map(|provider| {
                    let (endpoint, key) = provider.defaults();
                    Profile {
                        provider,
                        name: provider.to_string(),
                        endpoint: endpoint.into(),
                        key_env: key.into(),
                        ..Profile::default()
                    }
                })
                .collect();
        }
        for provider in [Provider::Codex, Provider::ClaudeCode] {
            if !settings.profiles.iter().any(|p| p.provider == provider) {
                settings.profiles.push(Profile {
                    provider,
                    name: provider.to_string(),
                    endpoint: String::new(),
                    key_env: String::new(),
                    ..Profile::default()
                });
            }
        }
        let engine = Engine::new(store.clone());
        #[cfg(not(test))]
        for profile in settings.profiles.iter().filter(|p| p.provider.native()) {
            engine.native_auth(profile.clone(), false);
        }
        let mut profile = settings
            .profiles
            .iter()
            .position(|p| p.provider == Provider::Codex)
            .unwrap_or(0);
        let runs = match store.runs() {
            Ok(runs) => runs,
            Err(e) => {
                notice = e;
                Vec::new()
            }
        };
        let roots = runs
            .iter()
            .filter(|run| run.spec.parent.is_none())
            .map(|run| run.spec.workspace.clone())
            .collect::<Vec<_>>();
        let workspaces =
            crate::workspace_ui::Panel::new(store.clone(), settings.workspace.clone(), &roots);
        settings.workspace = workspaces.library.active.clone();
        let project = workspaces
            .library
            .projects
            .iter()
            .find(|p| Some(&p.root) == settings.workspace.as_ref());
        if let Some(index) = project
            .and_then(|p| p.profile_id.as_ref())
            .and_then(|id| settings.profiles.iter().position(|p| &p.id == id))
        {
            profile = index;
        }
        let task =
            text_editor::Content::with_text(project.map(|p| p.draft.as_str()).unwrap_or_default());
        let context_field = settings.profiles[profile].context_tokens.to_string();
        Self {
            workspaces,
            engine,
            store,
            settings,
            runs,
            selected: None,
            profile,
            mode: Mode::Plan,
            isolation: Isolation::Shared,
            notice,
            task,
            steering: String::new(),
            answer: String::new(),
            context_field,
            streams: HashMap::new(),
            questions: HashMap::new(),
            traces: Vec::new(),
            paused: HashSet::new(),
            tab: "Conversation",
            settings_ok,
            native_status: HashMap::new(),
            replay_editor: text_editor::Content::new(),
            job_output: None,
        }
    }

    pub fn choices(&self) -> Vec<Choice> {
        self.settings
            .profiles
            .iter()
            .enumerate()
            .map(|(index, p)| Choice {
                index,
                label: p.name.clone(),
            })
            .collect()
    }
    pub fn choice(&self) -> Choice {
        Choice {
            index: self.profile,
            label: self.settings.profiles[self.profile].name.clone(),
        }
    }
    pub fn current(&self) -> Option<&RunRecord> {
        self.runs
            .iter()
            .find(|r| Some(&r.spec.id) == self.selected.as_ref())
    }

    pub fn launch(&mut self, task: String, check: bool) -> Option<String> {
        if !check && self.workspaces.dirty() {
            self.notice =
                "Save or discard your workspace instruction edits before starting an agent.".into();
            return None;
        }
        let Some(workspace) = self.settings.workspace.clone() else {
            self.notice = "Choose a workspace folder first.".into();
            return None;
        };
        let spec = RunSpec {
            id: id(),
            task,
            workspace,
            profile: self.settings.profiles[self.profile].clone(),
            mode: if check { Mode::Plan } else { self.mode },
            isolation: if check {
                Isolation::Shared
            } else {
                self.isolation
            },
            parent: None,
            check_only: check,
        };
        match self.engine.start(spec, self.settings.clone(), None) {
            Ok(id) => {
                self.selected = Some(id.clone());
                self.notice = if check {
                    "Running a local workspace check…"
                } else {
                    "Agent launched. You can launch another agent while this one works."
                }
                .into();
                self.tab = "Conversation";
                Some(id)
            }
            Err(e) => {
                self.notice = e;
                None
            }
        }
    }

    pub fn pulse(&mut self) {
        self.workspaces.remember_preferences(
            self.settings.profiles[self.profile].id.clone(),
            self.task.text(),
        );
        for event in self.engine.drain() {
            match event {
                Event::NativeStatus(status) => {
                    if let Some(url) = &status.login_url
                        && self
                            .native_status
                            .get(&status.profile)
                            .and_then(|s| s.login_url.as_ref())
                            != Some(url)
                    {
                        let _ = open_url(url);
                    }
                    self.native_status.insert(status.profile.clone(), status);
                }
                Event::Record(record) => {
                    let mut record = *record;
                    if self.selected.as_ref() == Some(&record.spec.id) && finished(&record.status) {
                        self.notice = if record.status == "Completed" {
                            "Run completed.".into()
                        } else {
                            record.report.clone()
                        };
                    }
                    if self.paused.contains(&record.spec.id) && record.status == "Running" {
                        record.status = "Paused after current request".into();
                    }
                    if let Some(existing) =
                        self.runs.iter_mut().find(|r| r.spec.id == record.spec.id)
                    {
                        *existing = record;
                    } else {
                        self.runs.insert(0, record);
                    }
                }
                Event::Delta { run, text } => {
                    let stream = self.streams.entry(run).or_default();
                    if stream.len() < 200_000 {
                        stream.push_str(&text);
                    }
                }
                Event::ResetStream(run) => {
                    self.streams.remove(&run);
                }
                Event::Trace(trace) => {
                    if trace.kind == "storage_error" {
                        self.notice = format!("Session could not be saved: {}", trace.detail);
                    }
                    self.traces.push(trace);
                    if self.traces.len() > 500 {
                        self.traces.drain(..100);
                    }
                }
                Event::Question {
                    run,
                    id,
                    prompt,
                    approval,
                } => {
                    self.questions.entry(run).or_default().push(Question {
                        id,
                        prompt,
                        approval,
                    });
                }
                Event::Answered { run, id } => {
                    if let Some(items) = self.questions.get_mut(&run) {
                        items.retain(|q| q.id != id);
                    }
                }
            }
        }
    }

    pub fn continue_chat(&mut self, run: &str, prompt: String) -> Option<String> {
        let Some(mut record) = self.runs.iter().find(|r| r.spec.id == run).cloned() else {
            self.notice =
                "This chat's saved run is unavailable. Start a new chat in the selected workspace."
                    .into();
            return None;
        };
        if self.settings.workspace.as_ref() != Some(&record.spec.workspace) {
            self.notice = "Open this chat's original workspace before continuing its agent.".into();
            return None;
        }
        if self.workspaces.dirty() {
            self.notice =
                "Save or discard your workspace instruction edits before continuing an agent."
                    .into();
            return None;
        }
        if !finished(&record.status) {
            self.engine.steer(run, prompt);
            return Some(run.into());
        }
        record
            .transcript
            .push(crate::harness::model::Turn::text("user", prompt));
        match self
            .engine
            .start(record.spec.clone(), self.settings.clone(), Some(record))
        {
            Ok(id) => {
                self.selected = Some(id.clone());
                Some(id)
            }
            Err(e) => {
                self.notice = e;
                None
            }
        }
    }

    pub fn update(&mut self, action: Action) -> Task<Action> {
        match action {
            Action::Workspace(action) => {
                let previous = self.settings.workspace.clone();
                self.workspaces.remember_preferences(
                    self.settings.profiles[self.profile].id.clone(),
                    self.task.text(),
                );
                if matches!(action, crate::workspace_ui::Action::OpenCodex)
                    && self.workspaces.ready()
                    && let Some(index) = self
                        .settings
                        .profiles
                        .iter()
                        .position(|p| p.provider == Provider::Codex)
                {
                    self.profile = index;
                    self.context_field = self.settings.profiles[index].context_tokens.to_string();
                }
                let task = self.workspaces.update(action).map(Action::Workspace);
                self.settings.workspace = self.workspaces.library.active.clone();
                if previous != self.settings.workspace {
                    self.selected = None;
                    self.steering.clear();
                    self.answer.clear();
                    self.job_output = None;
                    self.replay_editor = text_editor::Content::new();
                    let project = self
                        .workspaces
                        .library
                        .projects
                        .iter()
                        .find(|p| Some(&p.root) == self.settings.workspace.as_ref());
                    self.profile = project
                        .and_then(|p| p.profile_id.as_ref())
                        .and_then(|id| self.settings.profiles.iter().position(|p| &p.id == id))
                        .unwrap_or_else(|| {
                            self.settings
                                .profiles
                                .iter()
                                .position(|p| p.provider == Provider::Codex)
                                .unwrap_or(0)
                        });
                    self.task = text_editor::Content::with_text(
                        project.map(|p| p.draft.as_str()).unwrap_or_default(),
                    );
                    self.context_field = self.settings.profiles[self.profile]
                        .context_tokens
                        .to_string();
                    self.save_settings();
                }
                return task;
            }
            Action::NativeAuth(login) => {
                self.save_settings();
                let profile = self.settings.profiles[self.profile].clone();
                self.engine.native_auth(profile, login);
            }
            Action::CancelLogin => {
                let profile = &self.settings.profiles[self.profile];
                self.engine.cancel_login(&profile.id);
                self.native_status.remove(&profile.id);
                self.notice = "Sign-in cancelled.".into();
            }
            Action::OpenLogin => {
                if let Some(url) = self
                    .native_status
                    .get(&self.settings.profiles[self.profile].id)
                    .and_then(|s| s.login_url.as_ref())
                    && let Err(error) = open_url(url)
                {
                    self.notice = error;
                }
            }
            Action::NativeModel(model) => self.settings.profiles[self.profile].model = model,
            Action::ChooseWorkspace => {
                return self.update(Action::Workspace(crate::workspace_ui::Action::Open));
            }
            Action::ChooseProfile(choice) => {
                if choice.index < self.settings.profiles.len() {
                    self.profile = choice.index;
                    self.context_field = self.settings.profiles[self.profile]
                        .context_tokens
                        .to_string();
                }
            }
            Action::AddProfile => {
                self.settings.profiles.push(Profile {
                    name: format!("Connection {}", self.settings.profiles.len() + 1),
                    ..Profile::default()
                });
                self.profile = self.settings.profiles.len() - 1;
                self.context_field = "32000".into();
            }
            Action::Provider(provider) => {
                let profile = &mut self.settings.profiles[self.profile];
                let (endpoint, key) = provider.defaults();
                profile.provider = provider;
                profile.endpoint = endpoint.into();
                profile.key_env = key.into();
                profile.name = provider.to_string();
                profile.model.clear();
            }
            Action::SummaryProfile(choice) => {
                self.settings.summary_profile_id = self
                    .settings
                    .profiles
                    .get(choice.index)
                    .map(|p| p.id.clone());
            }
            Action::SafetyProfile(choice) => {
                self.settings.safety_profile_id = self
                    .settings
                    .profiles
                    .get(choice.index)
                    .map(|p| p.id.clone())
            }
            Action::ProfileField(field, value) => {
                let profile = &mut self.settings.profiles[self.profile];
                match field {
                    "name" => profile.name = value,
                    "model" => profile.model = value,
                    "endpoint" => profile.endpoint = value,
                    "key_env" => profile.key_env = value,
                    "guided" => profile.guided = value == "true",
                    "executable" => profile.executable = value,
                    "context" => {
                        self.context_field = value.clone();
                        profile.context_tokens = value.parse().unwrap_or(0);
                    }
                    _ => {}
                }
            }
            Action::SaveProfiles => {
                self.save_settings();
            }
            Action::EditTask(action) => self.task.perform(action),
            Action::Mode(mode) => {
                self.mode = mode;
                if mode != Mode::Plan {
                    self.isolation = Isolation::Worktree;
                }
            }
            Action::RunMode(mode) => {
                if let Some(record) = self.current().cloned() {
                    if record.replay_only {
                        self.notice = "Replay experiments remain read only.".into();
                        return Task::none();
                    }
                    if !finished(&record.status)
                        && let Err(e) = self.engine.mode(&record.spec.id, mode)
                    {
                        self.notice = e;
                    } else {
                        if let Some(run) =
                            self.runs.iter_mut().find(|r| r.spec.id == record.spec.id)
                        {
                            run.spec.mode = mode;
                        }
                        self.notice = format!("Run switched to {mode}");
                    }
                }
            }
            Action::Isolation(isolation) => self.isolation = isolation,
            Action::Launch => {
                if self.launch(self.task.text(), false).is_some() {
                    self.task = text_editor::Content::new();
                }
            }
            Action::Check => {
                self.launch("Check the selected workspace".into(), true);
            }
            Action::Select(run) => {
                self.job_output = None;
                self.replay_editor = text_editor::Content::new();
                self.selected = Some(run.clone());
                self.answer.clear();
                self.steering.clear();
                self.load_trace(&run);
            }
            Action::Pause => {
                if let Some(run) = self.selected.clone() {
                    let pause = self.paused.insert(run.clone());
                    if !pause {
                        self.paused.remove(&run);
                    }
                    self.engine.pause(&run, pause);
                    self.notice=if pause{"Pause requested. The current request may finish; the next action will wait."}else{"Agent continued."}.into();
                }
            }
            Action::Stop => {
                if let Some(run) = &self.selected {
                    self.engine.stop(run);
                    self.notice = "Stop requested for the run and its children.".into();
                }
            }
            Action::Resume => {
                if let Some(record) = self.current().cloned() {
                    match self.engine.start(
                        record.spec.clone(),
                        self.settings.clone(),
                        Some(record),
                    ) {
                        Ok(_) => self.notice = "Run resumed from its saved transcript.".into(),
                        Err(e) => self.notice = e,
                    }
                }
            }
            Action::Steer(text) => self.steering = text,
            Action::SendSteering => {
                if let Some(run) = &self.selected
                    && !self.steering.trim().is_empty()
                {
                    self.engine.steer(run, std::mem::take(&mut self.steering));
                    self.notice = "Direction queued for the next model request.".into();
                }
            }
            Action::Approve(question, allow) => {
                if let Some(run) = &self.selected {
                    self.engine
                        .answer(run, &question, if allow { "allow" } else { "deny" }.into());
                }
            }
            Action::RememberApproval(question) => {
                if let Some(run) = self.current()
                    && !run.spec.profile.provider.native()
                {
                    self.engine
                        .answer(&run.spec.id, &question, "allow_always".into());
                }
            }
            Action::RevokeApprovals => {
                self.notice = match crate::harness::permissions::revoke(&self.store) {
                    Ok(()) => "Remembered approvals revoked for future actions.".into(),
                    Err(e) => e,
                };
            }
            Action::LoadCapture => {
                if let Some(run) = self.current() {
                    let directory = self.store.run_path(&run.spec.id).join("requests");
                    let latest = std::fs::read_dir(directory)
                        .ok()
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
                    self.notice = match latest {
                        Some(entry) => match std::fs::read(entry.path())
                            .map_err(|e| e.to_string())
                            .and_then(|bytes| {
                                if bytes.len() > 8_000_000 {
                                    Err("Capture exceeds 8 MB".into())
                                } else {
                                    String::from_utf8(bytes).map_err(|e| e.to_string())
                                }
                            }) {
                            Ok(body) => {
                                self.replay_editor = text_editor::Content::with_text(&body);
                                "Capture loaded. Edit one variable, then replay.".into()
                            }
                            Err(error) => error,
                        },
                        None => "This run has no captured API requests.".into(),
                    };
                }
            }
            Action::StopJob(job) => {
                if let Some(run) = &self.selected {
                    self.notice = if self.engine.stop_job(run, &job) {
                        "Background job stop requested."
                    } else {
                        "That job is no longer active."
                    }
                    .into();
                }
            }
            Action::JobOutput(job) => {
                if let Some(run) = &self.selected {
                    let output = self
                        .engine
                        .job_output(run, &job)
                        .or_else(|| {
                            self.store
                                .load::<serde_json::Value>(&format!(
                                    "runs/{run}/outputs/{job}.json"
                                ))
                                .ok()
                                .flatten()
                                .and_then(|v| v["text"].as_str().map(str::to_owned))
                        })
                        .unwrap_or_else(|| "No output yet.".into());
                    self.job_output = Some((run.clone(), output));
                }
            }
            Action::CancelWakeup(alarm) => {
                if let Some(run) = &self.selected {
                    self.engine.cancel_wakeup(run, &alarm);
                    self.notice = "Wakeup cancellation queued.".into();
                }
            }
            Action::ReplayAgain => {
                let _ = self.update(Action::LoadCapture);
                return self.update(Action::Replay);
            }
            Action::EditReplay(edit) => self.replay_editor.perform(edit),
            Action::Replay => {
                if let Some(source) = self.current() {
                    let result = serde_json::from_str(&self.replay_editor.text())
                        .map_err(|e| e.to_string())
                        .and_then(|body| self.engine.replay(source, body));
                    match result {
                        Ok(run) => {
                            self.selected = Some(run);
                            self.tab = "Conversation";
                            self.notice = "Replay started. Tool proposals will not execute.".into();
                        }
                        Err(error) => self.notice = error,
                    }
                }
            }
            Action::ApprovePlan => {
                if let Some(mut record) = self
                    .current()
                    .filter(|r| {
                        !r.replay_only
                            && r.spec.mode == Mode::Plan
                            && r.status == "Completed"
                            && !r.report.trim().is_empty()
                    })
                    .cloned()
                {
                    let plan = record.report.clone();
                    record.spec.mode = Mode::Build;
                    record.transcript.push(crate::harness::model::Turn::context("approved_plan",format!("The user approved this plan for execution. Re-read current files and re-verify assumptions before editing. Plan:\n{plan}")));
                    match self.store.save(&format!("runs/{}/plan.json",record.spec.id),&serde_json::json!({"text":plan,"approved_at":crate::harness::model::now()})).and_then(|_|self.engine.start(record.spec.clone(),self.settings.clone(),Some(record))) {
                        Ok(_)=>self.notice="Approved plan queued for execution with Build permissions.".into(),Err(error)=>self.notice=error,
                    }
                }
            }
            Action::Answer(answer) => self.answer = answer,
            Action::SubmitAnswer(question) => {
                if let Some(run) = &self.selected {
                    self.engine
                        .answer(run, &question, std::mem::take(&mut self.answer));
                }
            }
            Action::Tab(tab) => self.tab = tab,
            Action::CopyPath => {
                let path = self
                    .current()
                    .map(|r| self.store.run_path(&r.spec.id))
                    .unwrap_or_else(|| self.store.root.clone());
                return iced::clipboard::write(path.to_string_lossy().into_owned());
            }
        }
        Task::none()
    }

    fn save_settings(&mut self) {
        if !self.settings_ok {
            self.notice="The saved settings could not be read. Recover or move that file before saving replacements; the original was kept.".into();
            return;
        }
        self.notice = match self.store.save("settings.json", &self.settings) {
            Ok(()) => {
                "Workspace and connections saved. API key values are never stored here.".into()
            }
            Err(e) => e,
        };
    }

    fn load_trace(&mut self, run: &str) {
        let path = self.store.run_path(run).join("events.jsonl");
        use std::io::Read;
        if let Ok(file) = std::fs::File::open(path) {
            let mut bytes = Vec::new();
            if file.take(2_000_000).read_to_end(&mut bytes).is_ok() {
                self.traces.retain(|t| t.run != run);
                for line in String::from_utf8_lossy(&bytes).lines() {
                    if let Ok(trace) = serde_json::from_str(line) {
                        self.traces.push(trace);
                    }
                }
            }
        }
    }

    pub fn workspace_bar(&self, p: Palette) -> Element<'_, Action> {
        let path = self
            .settings
            .workspace
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Choose a folder to work in".into());
        let name = self
            .settings
            .workspace
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        row![
            tooltip(
                container(
                    row![
                        icon(Icon::Folder, 17.0, p.muted),
                        container(
                            text(name)
                                .size(13)
                                .color(p.muted)
                                .wrapping(text::Wrapping::None)
                        )
                        .width(Fill)
                        .clip(true)
                    ]
                    .spacing(9)
                    .align_y(iced::Alignment::Center)
                )
                .width(Fill),
                container(text(path).size(12))
                    .padding([7, 10])
                    .style(move |_| theme::panel(p)),
                tooltip::Position::Bottom
            ),
            button(text("Change folder").size(12))
                .padding([8, 12])
                .on_press(Action::ChooseWorkspace)
                .style(move |_, s| theme::subtle(p, s))
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub fn control_view(&self, p: Palette) -> Element<'_, Action> {
        let mut content = column![
            text("Agent control").size(28),
            text("One workspace. Your choice of agents.")
                .size(14)
                .color(p.muted),
            self.workspace_bar(p)
        ]
        .spacing(14);
        if !self.notice.is_empty() {
            content = content.push(text(&self.notice).size(12).color(p.muted));
        }
        let composer = text_editor(&self.task)
            .placeholder("Give an agent a task…")
            .on_action(Action::EditTask)
            .height(92)
            .padding(12)
            .size(14);
        let controls = row![
            pick_list(self.choices(), Some(self.choice()), Action::ChooseProfile).text_size(12),
            pick_list(Mode::ALL, Some(self.mode), Action::Mode).text_size(12),
            pick_list(Isolation::ALL, Some(self.isolation), Action::Isolation).text_size(12)
        ]
        .spacing(10)
        .wrap();
        content = content.push(
            container(
                column![
                    composer,
                    controls,
                    text("Cycle permission modes · ⌘⇧M").size(11).color(p.muted),
                    row![
                        button("Run agent")
                            .on_press(Action::Launch)
                            .padding([9, 14]),
                        button("Check workspace")
                            .on_press(Action::Check)
                            .padding([9, 14])
                    ]
                    .spacing(10)
                ]
                .spacing(12),
            )
            .padding(16)
            .style(move |_| theme::panel(p)),
        );
        if !self
            .runs
            .iter()
            .any(|run| Some(&run.spec.workspace) == self.settings.workspace.as_ref())
        {
            content=content.push(container(column![text("Ready for your first agent").size(18),text("Choose a workspace, add a model ID in Connections, then run a task. Launch another task to work with a second agent. A local workspace check runs without an API key.").size(13).color(p.muted)].spacing(10)).padding(24));
        } else {
            let mut runs = column![text("Runs").size(14)].spacing(5);
            for run in self
                .runs
                .iter()
                .filter(|run| Some(&run.spec.workspace) == self.settings.workspace.as_ref())
                .take(30)
            {
                let title = run.spec.task.lines().next().unwrap_or("Agent");
                let label = if run.spec.check_only {
                    "Local workspace check".into()
                } else {
                    format!(
                        "{} · {}",
                        run.spec.profile.name,
                        if run.spec.profile.model.is_empty() {
                            "Agent default"
                        } else {
                            &run.spec.profile.model
                        }
                    )
                };
                let waiting = self
                    .questions
                    .get(&run.spec.id)
                    .is_some_and(|q| !q.is_empty());
                let state = if waiting {
                    "Needs your answer"
                } else {
                    &run.status
                };
                let selected = self.selected.as_ref() == Some(&run.spec.id);
                runs = runs.push(
                    button(
                        row![
                            column![
                                text(tools::clip(title, 80)).size(13),
                                text(label).size(11).color(p.muted)
                            ]
                            .spacing(3)
                            .width(Fill),
                            text(state).size(11).color(p.muted)
                        ]
                        .spacing(12)
                        .align_y(iced::Alignment::Center),
                    )
                    .width(Fill)
                    .padding([10, 12])
                    .on_press(Action::Select(run.spec.id.clone()))
                    .style(move |_, status| {
                        let mut style = theme::subtle(p, status);
                        if selected {
                            style.background = Some(p.accent_soft.into());
                        }
                        style
                    }),
                );
            }
            content = content.push(runs);
        }
        if let Some(run) = self.current() {
            content = content.push(self.run_view(run, p));
        }
        scrollable(content).height(Fill).into()
    }

    fn run_view(&self, run: &RunRecord, p: Palette) -> Element<'_, Action> {
        let mut detail = column![
            row![
                text("Run details").size(18).width(Fill),
                button("Copy trace folder").on_press(Action::CopyPath)
            ]
            .spacing(10),
            text(format!(
                "{} · {} {} · {} input / {} output / {} cached tokens",
                run.status,
                run.rounds,
                if run.spec.profile.provider.native() {
                    "native turns"
                } else {
                    "rounds"
                },
                run.usage.input,
                run.usage.output,
                run.usage.cached
            ))
            .size(12)
            .color(p.muted),
            text(if run.spec.profile.provider.native() {
                format!("Context managed by the native agent · {}", run.spec.mode)
            } else {
                format!(
                    "Context ≈ {} / {} tokens · {}",
                    run.context, run.spec.profile.context_tokens, run.spec.mode
                )
            })
            .size(12)
            .color(p.muted),
            text(run.root.to_string_lossy().into_owned())
                .size(11)
                .color(p.muted)
        ]
        .spacing(10);
        if let Some(parent) = &run.spec.parent {
            detail = detail.push(text(format!("Child of {parent}")).size(11).color(p.muted));
        }
        if let Some(branch) = &run.branch {
            detail = detail.push(
                text(format!(
                    "Branch: {branch} · kept for review; merge separately"
                ))
                .size(11)
                .color(p.muted),
            );
        }
        let actions = if run.replay_only && finished(&run.status) {
            row![button("Replay request again").on_press(Action::ReplayAgain)]
        } else if finished(&run.status) {
            row![
                button(if run.spec.check_only {
                    "Run check again"
                } else {
                    "Resume from transcript"
                })
                .on_press(Action::Resume)
            ]
        } else {
            row![
                button(if self.paused.contains(&run.spec.id) {
                    "Continue"
                } else {
                    "Pause"
                })
                .on_press(Action::Pause),
                button("Stop run").on_press(Action::Stop)
            ]
        };
        detail = detail.push(actions.spacing(10));
        if !run.replay_only
            && run.spec.mode == Mode::Plan
            && run.status == "Completed"
            && !run.spec.check_only
            && !run.report.trim().is_empty()
        {
            detail = detail.push(button("Approve plan & build").on_press(Action::ApprovePlan));
        }
        if let Ok(Some(items)) = self
            .store
            .load::<serde_json::Value>(&format!("runs/{}/todos.json", run.spec.id))
            && let Some(items) = items.as_array()
        {
            let mut todos = column![text("Task progress").size(14)].spacing(4);
            for item in items.iter().take(32) {
                todos = todos.push(
                    text(format!(
                        "{} {}",
                        match item["status"].as_str() {
                            Some("completed") => "[x]",
                            Some("in_progress") => "[>]",
                            _ => "[ ]",
                        },
                        item["task"].as_str().unwrap_or_default()
                    ))
                    .size(12)
                    .color(p.muted),
                );
            }
            detail = detail.push(todos);
        }
        if !run.spec.check_only && !run.replay_only {
            detail = detail
                .push(pick_list(Mode::ALL, Some(run.spec.mode), Action::RunMode).text_size(12));
        }
        if let Some(questions) = self.questions.get(&run.spec.id) {
            for question in questions {
                let controls: Element<'_, Action> = if question.approval {
                    let mut controls = row![
                        button("Allow once").on_press(Action::Approve(question.id.clone(), true)),
                        button("Deny").on_press(Action::Approve(question.id.clone(), false))
                    ];
                    if !run.spec.profile.provider.native() {
                        controls = controls.push(
                            button("Remember exact action")
                                .on_press(Action::RememberApproval(question.id.clone())),
                        );
                    }
                    controls.spacing(10).wrap().into()
                } else {
                    row![
                        text_input("Your answer", &self.answer)
                            .on_input(Action::Answer)
                            .on_submit(Action::SubmitAnswer(question.id.clone())),
                        button("Reply").on_press(Action::SubmitAnswer(question.id.clone()))
                    ]
                    .spacing(10)
                    .into()
                };
                detail = detail.push(
                    container(
                        column![
                            text(if question.approval {
                                "Action requested"
                            } else {
                                "Agent question"
                            })
                            .size(14),
                            text(&question.prompt).size(12),
                            controls
                        ]
                        .spacing(10),
                    )
                    .padding(14)
                    .style(move |_| theme::panel(p)),
                );
            }
        }
        if !finished(&run.status) && !run.replay_only {
            detail = detail.push(
                row![
                    text_input("Send a correction or new direction…", &self.steering)
                        .on_input(Action::Steer)
                        .on_submit(Action::SendSteering),
                    button("Send direction").on_press(Action::SendSteering)
                ]
                .spacing(10),
            );
        }
        detail = detail.push(
            row![
                button("Conversation").on_press(Action::Tab("Conversation")),
                button("Activity").on_press(Action::Tab("Activity")),
                button("Changes").on_press(Action::Tab("Changes")),
                button("Replay").on_press(Action::Tab("Replay")),
                button("Background").on_press(Action::Tab("Background"))
            ]
            .spacing(8)
            .wrap(),
        );
        match self.tab {
            "Background" => {
                let jobs = self.store.run_path(&run.spec.id).join("jobs");
                if let Ok(entries) = std::fs::read_dir(jobs) {
                    for entry in entries.filter_map(Result::ok).take(32) {
                        let Some(id) = entry
                            .path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(str::to_owned)
                        else {
                            continue;
                        };
                        if uuid::Uuid::parse_str(&id).is_err() {
                            continue;
                        }
                        if let Ok(Some(job)) = self.store.load::<serde_json::Value>(&format!(
                            "runs/{}/jobs/{id}.json",
                            run.spec.id
                        )) {
                            let tracked = job["status"] == "Running"
                                && !finished(&run.status)
                                && !run.status.starts_with("Interrupted");
                            let status = if job["status"] == "Running" && !tracked {
                                "Untracked after interruption"
                            } else {
                                job["status"].as_str().unwrap_or("Unknown")
                            };
                            let mut row = row![
                                text(format!(
                                    "{} · {}",
                                    status,
                                    tools::clip(job["command"].as_str().unwrap_or(&id), 70)
                                ))
                                .size(12)
                                .width(Fill),
                                button("View output").on_press(Action::JobOutput(id.clone()))
                            ]
                            .spacing(8);
                            if tracked {
                                row = row.push(button("Stop job").on_press(Action::StopJob(id)));
                            }
                            detail = detail.push(row.wrap());
                        }
                    }
                }
                if let Ok(Some(alarms)) = self
                    .store
                    .load::<serde_json::Value>(&format!("runs/{}/alarms.json", run.spec.id))
                    && let Some(alarms) = alarms.as_array()
                {
                    for alarm in alarms.iter().filter(|a| a["delivered"] != true).take(8) {
                        if let Some(id) = alarm["id"].as_str() {
                            detail = detail.push(
                                row![
                                    text(format!(
                                        "{} · {}",
                                        if alarm["path"].is_null() {
                                            "Timer"
                                        } else {
                                            "File monitor"
                                        },
                                        tools::clip(
                                            alarm["prompt"].as_str().unwrap_or_default(),
                                            100
                                        )
                                    ))
                                    .size(12)
                                    .width(Fill),
                                    button("Cancel wakeup")
                                        .on_press(Action::CancelWakeup(id.into()))
                                ]
                                .spacing(8)
                                .wrap(),
                            );
                        }
                    }
                }
                let output=self.job_output.as_ref().filter(|(id,_)|id==&run.spec.id).map(|(_,text)|text.as_str()).unwrap_or("Select a job to inspect its output. After an interruption, check for leftover processes before retrying.");
                detail = detail.push(text_widget(tools::clip(output, 16000), p));
            }
            "Replay" => {
                if run.spec.profile.provider.native() {
                    detail=detail.push(text("Native agents own their internal API traffic. Their protocol activity remains available under Activity.").size(13).color(p.muted));
                } else {
                    detail=detail.push(text("Load the latest captured request, edit it, and compare a fresh reply. Tool proposals are recorded without executing them.").size(13).color(p.muted))
                        .push(row![button("Load latest capture").on_press(Action::LoadCapture),button("Replay edited request").on_press(Action::Replay)].spacing(8))
                        .push(text_editor(&self.replay_editor).on_action(Action::EditReplay).height(300).size(12).font(iced::Font::MONOSPACE).padding(12));
                }
            }
            "Activity" => {
                for trace in self
                    .traces
                    .iter()
                    .filter(|t| t.run == run.spec.id)
                    .rev()
                    .take(40)
                {
                    detail = detail.push(
                        column![
                            text(format!("{} · {}", trace.time, trace.kind))
                                .size(11)
                                .color(p.muted),
                            text(tools::clip(
                                &serde_json::to_string_pretty(&trace.detail).unwrap_or_default(),
                                3000
                            ))
                            .size(12)
                        ]
                        .spacing(4),
                    );
                }
            }
            "Changes" => {
                let review = self
                    .store
                    .load::<serde_json::Value>(&format!("runs/{}/review.json", run.spec.id));
                let text=match review{Ok(Some(value))=>format!("{}\n\n{}",value["status"].as_str().unwrap_or_default(),value["diff"].as_str().filter(|s|!s.is_empty()).unwrap_or("No tracked-file diff was recorded.")),Ok(None)=>"A diff will be captured when this run finishes. Worktrees stay available for review.".into(),Err(e)=>e};
                detail = detail.push(text_widget(tools::clip(&text, 20000), p));
            }
            _ => {
                for turn in run
                    .transcript
                    .iter()
                    .rev()
                    .take(20)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    for block in &turn.blocks {
                        match block {
                            Block::Text { text: message } => {
                                detail = detail.push(
                                    column![
                                        text(if turn.role == "assistant" {
                                            if run.spec.check_only {
                                                "Workspace check".into()
                                            } else {
                                                run.spec.profile.name.clone()
                                            }
                                        } else {
                                            "You".into()
                                        })
                                        .size(11)
                                        .color(p.muted),
                                        text_widget(tools::clip(message, 16000), p)
                                    ]
                                    .spacing(5),
                                )
                            }
                            Block::ToolCall { name, .. } => {
                                detail = detail
                                    .push(text(format!("Using {name}")).size(11).color(p.muted))
                            }
                            Block::ToolResult {
                                error: true,
                                text: message,
                                ..
                            } => {
                                detail = detail.push(
                                    text(format!("Tool error: {}", tools::clip(message, 1000)))
                                        .size(12)
                                        .color(p.muted),
                                )
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(stream) = self.streams.get(&run.spec.id) {
                    detail = detail.push(text_widget(tools::clip(stream, 16000), p));
                }
                if run.status == "Failed" || run.status == "Stopped" {
                    detail = detail.push(text_widget(run.report.clone(), p));
                }
            }
        }
        container(detail)
            .padding(18)
            .width(Fill)
            .style(move |_| theme::panel(p))
            .into()
    }

    pub fn connections_view(&self, p: Palette) -> Element<'_, Action> {
        let profile = &self.settings.profiles[self.profile];
        let field = |label: &'static str, key: &'static str, value: &str| {
            column![
                text(label).size(12).color(p.muted),
                text_input(label, value)
                    .on_input(move |value| Action::ProfileField(key, value))
                    .padding(10)
                    .size(14)
            ]
            .spacing(5)
        };
        let readiness = profile
            .validate()
            .err()
            .unwrap_or_else(|| "Configured. Model access is verified when you run a task.".into());
        if profile.provider.native() {
            let state = self.native_status.get(&profile.id);
            let busy = state.is_some_and(|s| s.busy);
            let connected = state.is_some_and(|s| s.authenticated);
            let resolved = crate::harness::native::resolve(profile)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|e| e);
            let mut content = column![
                text("Connections").size(28),
                text("Run Codex and Claude Code from the same Luxe workspace.")
                    .size(14)
                    .color(p.muted),
                row![
                    pick_list(self.choices(), Some(self.choice()), Action::ChooseProfile),
                    button("Add connection").on_press(Action::AddProfile)
                ]
                .spacing(10),
                pick_list(Provider::ALL, Some(profile.provider), Action::Provider),
                field("Connection name", "name", &profile.name),
                field(
                    "Executable path (empty = auto detect)",
                    "executable",
                    &profile.executable
                ),
                text(resolved).size(11).color(p.muted),
                text(
                    state
                        .map(|s| s.message.as_str())
                        .unwrap_or("Refresh status to check this connection.")
                )
                .size(14),
                row![
                    button(if connected {
                        "Use existing sign-in"
                    } else if profile.provider == Provider::Codex {
                        "Sign in with Codex"
                    } else {
                        "Sign in to Claude Code"
                    })
                    .on_press_maybe((!busy).then_some(Action::NativeAuth(true))),
                    button("Refresh status")
                        .on_press_maybe((!busy).then_some(Action::NativeAuth(false)))
                ]
                .spacing(10),
                field("Model ID (empty = agent default)", "model", &profile.model)
            ]
            .spacing(14);
            if let Some(state) = state {
                if !state.models.is_empty() {
                    content = content.push(
                        pick_list(
                            state.models.clone(),
                            (!profile.model.is_empty()).then_some(profile.model.clone()),
                            Action::NativeModel,
                        )
                        .placeholder("Available native models"),
                    );
                }
                if state.login_url.is_some() {
                    content = content.push(button("Open sign-in page").on_press(Action::OpenLogin));
                }
                if let Some(code) = &state.login_code {
                    content = content.push(text(format!("Sign-in code: {code}")).size(18));
                }
                if state.busy {
                    content = content.push(button("Cancel sign-in").on_press(Action::CancelLogin));
                }
            }
            content=content.push(button("Save connection").on_press(Action::SaveProfiles).padding([10,16])).push(text(&self.notice).size(12).color(p.muted)).push(text("Your native agent keeps its own account credentials. Luxe uses its supported local process protocol and saves the session ID for resume. Codex enforces its sandbox and approval policy; Claude Code sends permission requests back to Luxe. Mode changes take effect after stopping and resuming a native run.").size(12).color(p.muted));
            return scrollable(content).height(Fill).into();
        }
        let mut summary_choices = self
            .choices()
            .into_iter()
            .filter(|c| !self.settings.profiles[c.index].provider.native())
            .collect::<Vec<_>>();
        summary_choices.push(Choice {
            index: usize::MAX,
            label: "Same as each run".into(),
        });
        let summary_selected = summary_choices
            .iter()
            .find(|c| {
                self.settings.profiles.get(c.index).map(|p| &p.id)
                    == self.settings.summary_profile_id.as_ref()
            })
            .cloned();
        let mut safety_choices = summary_choices.clone();
        if let Some(none) = safety_choices.iter_mut().find(|c| c.index == usize::MAX) {
            none.label = "Disabled · exact grants only".into();
        }
        let safety_selected = safety_choices
            .iter()
            .find(|c| {
                self.settings.profiles.get(c.index).map(|p| &p.id)
                    == self.settings.safety_profile_id.as_ref()
            })
            .cloned();
        let content=column![text("Connections").size(28),text("Choose a provider for each agent. Add multiple profiles for different models or endpoints.").size(14).color(p.muted),row![pick_list(self.choices(),Some(self.choice()),Action::ChooseProfile),button("Add connection").on_press(Action::AddProfile)].spacing(10),pick_list(Provider::ALL,Some(profile.provider),Action::Provider),field("Connection name","name",&profile.name),field("Model ID","model",&profile.model),field("API base URL","endpoint",&profile.endpoint),field("API key environment variable","key_env",&profile.key_env),field("Context budget (tokens)","context",&self.context_field),text(readiness).size(12).color(p.muted),button(if profile.guided{"Instructions: guided"}else{"Instructions: lean"}).on_press(Action::ProfileField("guided",(!profile.guided).to_string())),text("Compaction connection").size(12).color(p.muted),pick_list(summary_choices,summary_selected,Action::SummaryProfile),text("Optional safety screening for remembered actions").size(12).color(p.muted),pick_list(safety_choices,safety_selected,Action::SafetyProfile),button("Save connections").on_press(Action::SaveProfiles).padding([10,16]),text(&self.notice).size(12).color(p.muted),text("Local models can use a compatible endpoint without a key. For hosted providers, set the named environment variable before starting Luxe. Key values are read only for provider requests and are excluded from tool-process environments and captures.").size(12).color(p.muted),text("Extensions").size(18),text(format!("{} enabled MCP servers · {} hooks · {} language servers",self.settings.mcp_servers.iter().filter(|s|s.enabled).count(),self.settings.hooks.iter().filter(|s|s.enabled).count(),self.settings.language_servers.iter().filter(|s|s.enabled).count())).size(12),text(format!("Configure trusted stdio/HTTP MCP servers, lifecycle hooks and language analyzers in {}. Each is disabled until explicitly enabled there. Project skills are discovered from .luxe/skills/*/SKILL.md.",self.store.root.join("settings.json").display())).size(12).color(p.muted)].spacing(14);
        scrollable(content).height(Fill).into()
    }

    pub fn vault_view(&self, p: Palette) -> Element<'_, Action> {
        let mut content = column![
            text("Workspace vault").size(28),
            text("Durable runs, source context, and procedures for this workspace.")
                .size(14)
                .color(p.muted),
            self.workspace_bar(p),
            text(format!(
                "{} saved runs · storage: {}",
                self.runs
                    .iter()
                    .filter(|run| Some(&run.spec.workspace) == self.settings.workspace.as_ref())
                    .count(),
                self.store.root.display()
            ))
            .size(12)
            .color(p.muted),
            button("Copy storage path").on_press(Action::CopyPath),
            button("Revoke remembered approvals").on_press(Action::RevokeApprovals),
            text("Project procedures").size(18)
        ]
        .spacing(14);
        if let Some(root) = self
            .settings
            .workspace
            .as_ref()
            .and_then(|p| p.canonicalize().ok())
        {
            let skills = extensions::skills(&root);
            if skills.is_empty() {
                content=content.push(text("Add a procedure at .luxe/skills/<name>/SKILL.md. Agents receive a short index and load the full instructions only when needed.").size(13).color(p.muted));
            }
            for skill in skills {
                content = content.push(
                    column![
                        text(skill.name).size(14),
                        text(skill.description).size(12).color(p.muted)
                    ]
                    .spacing(4),
                );
            }
        }
        content=content.push(text("Runs keep transcripts, activity and review diffs. API runs also capture provider payloads; native runs keep their agent session IDs and protocol events. Active processes are marked interrupted after restart; resume is explicit.").size(13).color(p.muted));
        scrollable(content).height(Fill).into()
    }
}

pub fn finished(status: &str) -> bool {
    matches!(status, "Completed" | "Failed" | "Stopped") || status.starts_with("Interrupted")
}
fn text_widget(message: String, p: Palette) -> Element<'static, Action> {
    container(text(message).size(13).line_height(1.5))
        .width(Fill)
        .padding([6, 0])
        .style(move |_| theme::flat(p.surface))
        .into()
}

fn open_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err("Sign-in requires HTTPS".into());
    }
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}
