use std::path::PathBuf;

use iced::widget::{
    Space, button, canvas, column, container, mouse_area, operation, pick_list, row, scrollable,
    stack, text, text_editor, text_input, tooltip,
};
use iced::{
    Alignment, Border, Color, Element, Event, Fill, Font, Length, Padding, Shadow, Subscription,
    Task, Theme, Vector, keyboard,
};

use crate::agent_ui::{self, Panel};
use crate::harness::store::Store;
use crate::icons::{Icon, icon};
use crate::session::Session;
use crate::theme::{self, Palette};

const CHAT_WIDTH: f32 = 780.0;
const SIDEBAR_WIDTH: f32 = 232.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workspace {
    Setup,
    Vault,
    AgentControl,
    Plugins,
}

impl Workspace {
    fn label(self) -> &'static str {
        match self {
            Self::Setup => "Workspace setup",
            Self::Vault => "Vault",
            Self::AgentControl => "Agent control",
            Self::Plugins => "Connections",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Edit(text_editor::Action),
    Send,
    Agents(agent_ui::Action),
    Pulse,
    Attach,
    FilePicked(usize, Option<PathBuf>),
    ToggleSidebar,
    OpenWorkspace(Workspace),
    ViewRun(String),
    NewChat,
    SelectChat(usize),
    ToggleSearch,
    Search(String),
    RemoveAttachment(usize),
    DragWindow,
    CycleMode,
}

pub struct App {
    sessions: Vec<Session>,
    active: usize,
    sidebar_open: bool,
    workspace: Option<Workspace>,
    search_open: bool,
    search: String,
    composer: text_editor::Content,
    agents: Panel,
    dirty: bool,
    chats_ok: bool,
    backdrop: crate::backdrop::Backdrop,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedChats {
    sessions: Vec<Session>,
    active: usize,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let mut app = Self::with_store(Store::open());
        let task = app
            .agents
            .update(agent_ui::Action::Workspace(
                crate::workspace_ui::Action::Refresh,
            ))
            .map(Message::Agents);
        (app, task)
    }

    fn with_store(store: Store) -> Self {
        let mut agents = Panel::new(store.clone());
        let (mut sessions, active, chats_ok) = match store.load::<SavedChats>("chats.json") {
            Ok(Some(chats)) if !chats.sessions.is_empty() => {
                let active = chats.active.min(chats.sessions.len() - 1);
                (chats.sessions, active, true)
            }
            Ok(_) => (vec![Session::new()], 0, true),
            Err(error) => {
                agents.notice = error;
                (vec![Session::new()], 0, false)
            }
        };
        for session in &mut sessions {
            if session.workspace.is_none() {
                session.workspace = session
                    .run_id
                    .as_ref()
                    .and_then(|id| agents.runs.iter().find(|run| &run.spec.id == id))
                    .map(|run| run.spec.workspace.clone())
                    .or_else(|| agents.settings.workspace.clone());
            }
        }
        let composer = text_editor::Content::with_text(&sessions[active].draft);
        let mut app = Self {
            sessions,
            active,
            sidebar_open: true,
            workspace: if agents.settings.workspace.is_none() {
                Some(Workspace::Setup)
            } else {
                None
            },
            search_open: false,
            search: String::new(),
            composer,
            agents,
            dirty: false,
            chats_ok,
            backdrop: crate::backdrop::Backdrop::default(),
        };
        app.restore_workspace_chat();
        app
    }

    fn restore_workspace_chat(&mut self) {
        let root = self.agents.settings.workspace.clone();
        if self.sessions[self.active].workspace != root {
            self.active = self
                .sessions
                .iter()
                .rposition(|session| session.workspace == root)
                .unwrap_or_else(|| {
                    let mut session = Session::new();
                    session.workspace = root;
                    self.sessions.push(session);
                    self.sessions.len() - 1
                });
        }
        self.composer = text_editor::Content::with_text(&self.sessions[self.active].draft);
        self.agents.selected = self.sessions[self.active].run_id.clone();
    }

    pub fn theme(&self) -> Theme {
        theme::app_theme(false)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, _, _| {
            let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            match key.as_ref() {
                keyboard::Key::Character("b") if modifiers.command() => {
                    Some(Message::ToggleSidebar)
                }
                keyboard::Key::Character("n") if modifiers.command() => Some(Message::NewChat),
                keyboard::Key::Character("k") if modifiers.command() => Some(Message::ToggleSearch),
                keyboard::Key::Character("m" | "M") if modifiers.command() && modifiers.shift() => {
                    Some(Message::CycleMode)
                }
                _ => None,
            }
        });
        Subscription::batch([
            keyboard,
            iced::time::every(std::time::Duration::from_millis(200)).map(|_| Message::Pulse),
        ])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if !matches!(message, Message::Pulse) {
            self.dirty = true;
        }
        match message {
            Message::Agents(action) => {
                use crate::workspace_ui::Action as WorkspaceAction;
                let previous = self.agents.settings.workspace.clone();
                let onboarding = matches!(
                    &action,
                    agent_ui::Action::ChooseWorkspace
                        | agent_ui::Action::Workspace(
                            WorkspaceAction::Open
                                | WorkspaceAction::Create
                                | WorkspaceAction::Select(_)
                                | WorkspaceAction::Opened(Some(_))
                                | WorkspaceAction::Created(Some(_))
                        )
                );
                let codex_chat = matches!(
                    &action,
                    agent_ui::Action::Workspace(WorkspaceAction::OpenCodex)
                ) && self.agents.workspaces.ready();
                let task = self.agents.update(action).map(Message::Agents);
                if self.agents.settings.workspace != previous {
                    if previous.is_none() {
                        for session in &mut self.sessions {
                            if session.workspace.is_none() {
                                session.workspace = self.agents.settings.workspace.clone();
                            }
                        }
                    }
                    self.restore_workspace_chat();
                    self.search.clear();
                }
                if onboarding {
                    self.workspace = Some(Workspace::Setup);
                }
                if codex_chat {
                    return Task::batch([task, self.update(Message::NewChat)]);
                }
                return task;
            }
            Message::Pulse => {
                self.agents.pulse();
                for session in &mut self.sessions {
                    if let Some(run) = session
                        .run_id
                        .as_ref()
                        .and_then(|id| self.agents.runs.iter().find(|r| &r.spec.id == id))
                        && agent_ui::finished(&run.status)
                        && !run.report.is_empty()
                    {
                        if let Some(entry) = session.messages.iter_mut().find(|e| {
                            e.role == "assistant" && e.run_id.as_ref() == Some(&run.spec.id)
                        }) {
                            if entry.text != run.report {
                                entry.text = run.report.clone();
                                self.dirty = true;
                            }
                        } else {
                            session.messages.push(crate::session::Entry {
                                text: run.report.clone(),
                                attachments: Vec::new(),
                                role: "assistant".into(),
                                run_id: Some(run.spec.id.clone()),
                            });
                            self.dirty = true;
                        }
                    }
                }
                if self.dirty && self.chats_ok {
                    if let Err(error) = self.agents.store.save(
                        "chats.json",
                        &SavedChats {
                            sessions: self.sessions.clone(),
                            active: self.active,
                        },
                    ) {
                        self.agents.notice = error;
                    }
                    self.dirty = false;
                }
            }
            Message::Edit(action) => {
                self.composer.perform(action);
                self.sessions[self.active].draft = self.composer.text();
            }
            Message::Send => {
                let prompt = self.composer.text();
                if !prompt.trim().is_empty() {
                    let paths = self.sessions[self.active]
                        .attachments
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    let task = if paths.is_empty() {
                        prompt.clone()
                    } else {
                        format!(
                            "{prompt}\n\nAttached file references (not contents):\n{}",
                            paths.join("\n")
                        )
                    };
                    let run = if let Some(id) = self.sessions[self.active].run_id.clone() {
                        self.agents.continue_chat(&id, task)
                    } else {
                        self.agents.launch(task, false)
                    };
                    if let Some(run) = run {
                        self.sessions[self.active].run_id = Some(run);
                    } else {
                        return Task::none();
                    }
                }
                if self.sessions[self.active].submit(&prompt) {
                    self.composer = text_editor::Content::new();
                    return Task::batch([
                        operation::snap_to_end("transcript"),
                        operation::focus("composer"),
                    ]);
                }
            }
            Message::Attach => {
                let origin = self.active;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    move |path| Message::FilePicked(origin, path),
                );
            }
            Message::FilePicked(origin, Some(path)) => {
                if let Some(session) = self.sessions.get_mut(origin)
                    && !session.attachments.contains(&path)
                {
                    session.attachments.push(path);
                }
                if origin == self.active && self.workspace.is_none() {
                    return operation::focus("composer");
                }
            }
            Message::FilePicked(_, None) => {}
            Message::ToggleSidebar => {
                self.sidebar_open = !self.sidebar_open;
                if !self.sidebar_open {
                    self.search_open = false;
                    self.search.clear();
                    if self.workspace.is_none() {
                        return operation::focus("composer");
                    }
                }
            }
            Message::OpenWorkspace(workspace) => {
                self.workspace = Some(workspace);
                self.search_open = false;
                self.search.clear();
            }
            Message::ViewRun(run) => {
                self.workspace = Some(Workspace::AgentControl);
                return self
                    .agents
                    .update(agent_ui::Action::Select(run))
                    .map(Message::Agents);
            }
            Message::NewChat => {
                self.workspace = None;
                self.active = self
                    .sessions
                    .iter()
                    .position(|session| {
                        session.messages.is_empty()
                            && session.workspace == self.agents.settings.workspace
                            && session.draft.trim().is_empty()
                            && session.attachments.is_empty()
                    })
                    .unwrap_or_else(|| {
                        let mut session = Session::new();
                        session.workspace = self.agents.settings.workspace.clone();
                        self.sessions.push(session);
                        self.sessions.len() - 1
                    });
                self.composer = text_editor::Content::with_text(&self.sessions[self.active].draft);
                self.agents.selected = self.sessions[self.active].run_id.clone();
                self.search.clear();
                self.search_open = false;
                return operation::focus("composer");
            }
            Message::SelectChat(index) => {
                if index < self.sessions.len() {
                    if self.sessions[index].workspace != self.agents.settings.workspace {
                        self.agents.notice =
                            "Select this chat's workspace before opening its conversation.".into();
                        return Task::none();
                    }
                    self.workspace = None;
                    self.active = index;
                    self.composer = text_editor::Content::with_text(&self.sessions[index].draft);
                    self.agents.selected = self.sessions[index].run_id.clone();
                    return Task::batch([
                        operation::snap_to_end("transcript"),
                        operation::focus("composer"),
                    ]);
                }
            }
            Message::ToggleSearch => {
                self.sidebar_open = true;
                self.search_open = !self.search_open;
                self.search.clear();
                if self.search_open {
                    return operation::focus("chat-search");
                }
            }
            Message::Search(query) => self.search = query,
            Message::RemoveAttachment(index) => {
                if index < self.sessions[self.active].attachments.len() {
                    self.sessions[self.active].attachments.remove(index);
                }
            }
            Message::DragWindow => return iced::window::latest().and_then(iced::window::drag),
            Message::CycleMode => {
                if self.workspace.is_none() {
                    self.agents.selected = self.sessions[self.active].run_id.clone();
                }
                if let Some(run) = self.agents.current() {
                    return self
                        .agents
                        .update(agent_ui::Action::RunMode(run.spec.mode.next()))
                        .map(Message::Agents);
                }
                return self
                    .agents
                    .update(agent_ui::Action::Mode(self.agents.mode.next()))
                    .map(Message::Agents);
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let p = theme::palette(false);
        let content = if let Some(workspace) = self.workspace {
            let view = match workspace {
                Workspace::Setup => self
                    .agents
                    .workspaces
                    .view(p)
                    .map(agent_ui::Action::Workspace),
                Workspace::AgentControl => self.agents.control_view(p),
                Workspace::Vault => self.agents.vault_view(p),
                Workspace::Plugins => self.agents.connections_view(p),
            };
            container(view.map(Message::Agents))
                .padding([24, 32])
                .width(Fill)
                .height(Fill)
                .into()
        } else {
            self.chat_view(p)
        };
        let mut body = row![].height(Fill);
        if self.sidebar_open {
            body = body.push(self.sidebar(p));
        }
        body = body.push(content);
        container(column![self.titlebar(p), body].height(Fill))
            .width(Fill)
            .height(Fill)
            .style(move |_| theme::flat(p.bg))
            .into()
    }

    fn chat_view(&self, p: Palette) -> Element<'_, Message> {
        let is_new = self.sessions[self.active].messages.is_empty();
        let mut messages = column![].spacing(24).width(Fill).max_width(CHAT_WIDTH);
        for entry in &self.sessions[self.active].messages {
            let mut contents = column![text(&entry.text).size(15).line_height(1.55)].spacing(10);
            for path in &entry.attachments {
                contents = contents.push(
                    row![
                        icon(Icon::File, 14.0, p.muted),
                        text(
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        )
                        .size(12)
                        .color(p.muted),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            let bubble = container(contents)
                .padding([13, 18])
                .max_width(580)
                .style(move |_| container::Style {
                    background: Some(p.raised.into()),
                    text_color: Some(p.text),
                    border: Border {
                        radius: 20.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            messages = messages.push(if entry.role == "assistant" {
                container(bubble).align_left(Fill)
            } else {
                container(bubble).align_right(Fill)
            });
        }
        let transcript: Element<'_, Message> = if is_new {
            container(
                column![
                    text("What will you build?")
                        .font(Font::with_name("Geist"))
                        .shaping(text::Shaping::Advanced)
                        .size(32)
                        .line_height(1.2)
                        .color(p.text)
                        .align_x(Alignment::Center),
                    text("Bring an idea. Choose your agent.")
                        .size(13)
                        .color(p.muted)
                        .align_x(Alignment::Center),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .padding([0, 24])
            .center_x(Fill)
            .into()
        } else {
            scrollable(container(messages).center_x(Fill).padding([24, 32]))
                .id("transcript")
                .height(Fill)
                .direction(narrow_scrollbar())
                .style(move |_, status| theme::scroll(p, status))
                .into()
        };
        let composer = container(container(self.composer_view(p)).max_width(if is_new {
            640.0
        } else {
            CHAT_WIDTH
        }))
        .center_x(Fill)
        .padding(Padding {
            top: if is_new { 20.0 } else { 12.0 },
            right: 32.0,
            bottom: if is_new { 0.0 } else { 24.0 },
            left: 32.0,
        });
        let workspace_bar =
            container(self.agents.workspace_bar(p).map(Message::Agents)).padding([14, 28]);
        let mut chat = column![workspace_bar];
        if !self.agents.notice.is_empty() {
            chat = chat.push(
                container(text(&self.agents.notice).size(12).color(p.muted)).padding([4, 32]),
            );
        }
        if let Some(run) = self.sessions[self.active]
            .run_id
            .as_ref()
            .and_then(|id| self.agents.runs.iter().find(|r| &r.spec.id == id))
            && !agent_ui::finished(&run.status)
        {
            chat = chat.push(
                container(
                    row![
                        text(format!("{} · {}", run.spec.profile.name, run.status))
                            .size(12)
                            .color(p.muted),
                        theme::action(text("View run").size(12))
                            .on_press(Message::ViewRun(run.spec.id.clone()))
                    ]
                    .spacing(10),
                )
                .padding([4, 32]),
            );
        }
        if is_new {
            let welcome = container(column![transcript, composer])
                .padding(Padding {
                    bottom: 88.0,
                    ..Padding::ZERO
                })
                .center(Fill);
            stack![
                canvas(&self.backdrop).width(Fill).height(Fill),
                chat.push(welcome).width(Fill).height(Fill)
            ]
            .clip(true)
            .into()
        } else {
            chat.push(transcript)
                .push(composer)
                .width(Fill)
                .height(Fill)
                .into()
        }
    }

    fn titlebar(&self, p: Palette) -> Element<'_, Message> {
        let toggle = tooltip(
            button(icon(Icon::Panel, 17.0, p.muted))
                .width(28)
                .height(28)
                .padding(5)
                .on_press(Message::ToggleSidebar)
                .style(move |_, status| theme::subtle(p, status)),
            hint(
                if self.sidebar_open {
                    "Hide sidebar · ⌘B"
                } else {
                    "Show sidebar · ⌘B"
                },
                p,
            ),
            tooltip::Position::Bottom,
        )
        .gap(8);
        let open = self.sidebar_open;
        // The first 96 px belong to the native close, minimize, and zoom controls.
        let left = container(toggle)
            .width(if open { SIDEBAR_WIDTH } else { 144.0 })
            .height(44)
            .padding(Padding {
                left: 96.0,
                ..Padding::ZERO
            })
            .style(move |_| theme::flat(if open { p.raised } else { p.bg }));
        mouse_area(row![left, Space::new().width(Fill)].height(44))
            .on_press(Message::DragWindow)
            .into()
    }

    fn sidebar(&self, p: Palette) -> Element<'_, Message> {
        let search_button = tooltip(
            button(icon(Icon::Search, 17.0, p.muted))
                .width(30)
                .height(30)
                .padding(6)
                .on_press(Message::ToggleSearch)
                .style(move |_, status| theme::subtle(p, status)),
            hint("Search chats · ⌘K", p),
            tooltip::Position::Right,
        )
        .gap(8);
        let brand = row![
            text("Luxe").size(22).font(display_font()).color(p.accent),
            Space::new().width(Fill),
            search_button,
        ]
        .align_y(Alignment::Center);
        let new_chat = button(
            row![
                icon(Icon::Compose, 17.0, p.on_accent),
                text("New chat").size(14)
            ]
            .spacing(11)
            .align_y(Alignment::Center),
        )
        .on_press(Message::NewChat)
        .width(Fill)
        .padding([11, 12])
        .style(theme::primary);
        let mut navigation = column![new_chat, Space::new().height(8)].spacing(2);
        for (workspace, glyph) in [
            (Workspace::Setup, Icon::Folder),
            (Workspace::Vault, Icon::Vault),
            (Workspace::AgentControl, Icon::AgentControl),
            (Workspace::Plugins, Icon::Plugins),
        ] {
            let selected = self.workspace == Some(workspace);
            navigation = navigation.push(
                button(
                    row![icon(glyph, 17.0, p.muted), text(workspace.label()).size(14)]
                        .spacing(11)
                        .align_y(Alignment::Center),
                )
                .on_press(Message::OpenWorkspace(workspace))
                .width(Fill)
                .padding([11, 10])
                .style(move |_, status| {
                    let mut style = theme::subtle(p, status);
                    if selected {
                        style.background = Some(p.accent_soft.into());
                    }
                    style
                }),
            );
        }
        let mut contents = column![
            brand,
            Space::new().height(16),
            navigation,
            Space::new().height(26)
        ]
        .width(Fill);
        if self.search_open {
            contents = contents
                .push(
                    text_input("Search chats", &self.search)
                        .id("chat-search")
                        .on_input(Message::Search)
                        .size(13)
                        .padding([9, 10])
                        .style(move |_, status| text_input::Style {
                            background: p.surface.into(),
                            border: Border {
                                color: if matches!(status, text_input::Status::Focused { .. }) {
                                    p.faint
                                } else {
                                    p.border
                                },
                                width: 1.0,
                                radius: 8.0.into(),
                            },
                            icon: p.muted,
                            placeholder: p.faint,
                            value: p.text,
                            selection: p.accent_soft,
                        }),
                )
                .push(Space::new().height(14));
        }
        let project_heading = row![
            text("Workspaces").size(12).color(p.faint).width(Fill),
            button(icon(Icon::Plus, 13.0, p.muted))
                .padding(4)
                .on_press(Message::OpenWorkspace(Workspace::Setup))
                .style(move |_, status| theme::subtle(p, status)),
        ]
        .align_y(Alignment::Center);
        contents = contents
            .push(container(project_heading).padding([0, 10]))
            .push(Space::new().height(6));
        if self.agents.workspaces.library.projects.is_empty() {
            contents = contents.push(
                button(text("Open a folder").size(13))
                    .on_press(Message::Agents(agent_ui::Action::ChooseWorkspace))
                    .padding([10, 12])
                    .width(Fill)
                    .style(move |_, status| theme::subtle(p, status)),
            );
        }
        for project in &self.agents.workspaces.library.projects {
            let selected = Some(&project.root) == self.agents.settings.workspace.as_ref();
            let name = project.name();
            let button = button(
                row![
                    icon(
                        Icon::Folder,
                        14.0,
                        if selected { p.accent } else { p.faint }
                    ),
                    container(text(name).size(13).wrapping(text::Wrapping::None))
                        .width(Fill)
                        .clip(true),
                ]
                .spacing(9)
                .align_y(Alignment::Center),
            )
            .on_press(Message::Agents(agent_ui::Action::Workspace(
                crate::workspace_ui::Action::Select(project.root.clone()),
            )))
            .width(Fill)
            .padding([9, 10])
            .style(move |_, status| {
                let mut style = theme::subtle(p, status);
                if selected {
                    style.background = Some(p.accent_soft.into());
                }
                style
            });
            contents = contents.push(tooltip(
                button,
                hint(project.root.to_string_lossy().into_owned(), p),
                tooltip::Position::Right,
            ));
        }
        contents = contents
            .push(Space::new().height(24))
            .push(container(text("Project chats").size(12).color(p.faint)).padding([0, 10]))
            .push(Space::new().height(10));
        let mut chats = column![].spacing(3).width(Fill);
        let query = self.search.to_lowercase();
        for (index, session) in self.sessions.iter().enumerate().rev() {
            if session.workspace != self.agents.settings.workspace {
                continue;
            }
            let title = session.title();
            if !title.to_lowercase().contains(&query) {
                continue;
            }
            let selected = self.workspace.is_none() && self.active == index;
            let label = text(title.clone()).size(13).wrapping(text::Wrapping::None);
            let chat = button(container(label).width(Fill).clip(true))
                .on_press(Message::SelectChat(index))
                .padding([10, 12])
                .width(Fill)
                .style(move |_, status| {
                    let mut style = theme::subtle(p, status);
                    if selected {
                        style.background = Some(p.accent_soft.into());
                    }
                    style
                });
            chats = chats.push(tooltip(chat, hint(title, p), tooltip::Position::Right).gap(8));
        }
        let contents = scrollable(contents.push(chats))
            .height(Fill)
            .direction(narrow_scrollbar())
            .style(move |_, status| theme::scroll(p, status));
        container(contents)
            .width(SIDEBAR_WIDTH)
            .height(Fill)
            .padding(Padding {
                top: 15.0,
                right: 14.0,
                bottom: 20.0,
                left: 14.0,
            })
            .style(move |_| theme::flat(p.raised))
            .into()
    }

    fn composer_view(&self, p: Palette) -> Element<'_, Message> {
        let editor = text_editor(&self.composer)
            .id("composer")
            .placeholder("Ask Luxe…")
            .on_action(Message::Edit)
            .size(16)
            .line_height(1.5)
            .height(Length::Shrink)
            .min_height(42)
            .max_height(136)
            .padding([0, 4])
            .key_binding(|press| match press.key.as_ref() {
                keyboard::Key::Character("b" | "n" | "k") if press.modifiers.command() => None,
                keyboard::Key::Character("m" | "M")
                    if press.modifiers.command() && press.modifiers.shift() =>
                {
                    None
                }
                keyboard::Key::Named(keyboard::key::Named::Enter)
                    if !press.modifiers.shift()
                        && matches!(press.status, text_editor::Status::Focused { .. }) =>
                {
                    Some(text_editor::Binding::Custom(Message::Send))
                }
                _ => text_editor::Binding::from_key_press(press),
            })
            .style(move |_, _| text_editor::Style {
                background: p.surface.into(),
                border: Border::default(),
                placeholder: p.faint,
                value: p.text,
                selection: p.accent_soft,
            });
        let can_send = !self.composer.text().trim().is_empty();
        let send = tooltip(
            button(icon(
                Icon::ArrowUp,
                19.0,
                if can_send { p.on_accent } else { p.muted },
            ))
            .width(36)
            .height(36)
            .padding(8)
            .on_press_maybe(can_send.then_some(Message::Send))
            .style(|theme, status| button::Style {
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..theme::primary(theme, status)
            }),
            hint("Send · Enter", p),
            tooltip::Position::Top,
        )
        .gap(8);
        let picker = pick_list(
            self.agents.choices(),
            Some(self.agents.choice()),
            |choice| Message::Agents(agent_ui::Action::ChooseProfile(choice)),
        )
        .text_size(13)
        .padding([8, 10])
        .style(move |_, status| pick_list::Style {
            text_color: p.muted,
            placeholder_color: p.muted,
            handle_color: p.muted,
            background: if matches!(status, pick_list::Status::Active) {
                p.surface
            } else {
                p.raised
            }
            .into(),
            border: Border {
                color: p.border,
                width: 1.0,
                radius: 8.0.into(),
            },
        });
        let attach = tooltip(
            button(icon(Icon::Plus, 20.0, p.muted))
                .width(36)
                .height(36)
                .padding(8)
                .on_press(Message::Attach)
                .style(move |_, status| button::Style {
                    border: Border {
                        color: p.border,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..theme::subtle(p, status)
                }),
            hint("Attach file", p),
            tooltip::Position::Top,
        )
        .gap(8);
        let mut composer = column![editor].spacing(8);
        if !self.sessions[self.active].attachments.is_empty() {
            let mut files = column![].spacing(4);
            for (index, path) in self.sessions[self.active].attachments.iter().enumerate() {
                files = files.push(
                    row![
                        icon(Icon::File, 14.0, p.muted),
                        text(
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        )
                        .size(12)
                        .color(p.muted)
                        .width(Fill),
                        tooltip(
                            button(icon(Icon::Close, 12.0, p.muted))
                                .padding(6)
                                .on_press(Message::RemoveAttachment(index))
                                .style(move |_, status| theme::subtle(p, status)),
                            hint("Remove attachment", p),
                            tooltip::Position::Top,
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            composer = composer.push(
                scrollable(files)
                    .height((self.sessions[self.active].attachments.len() as f32 * 28.0).min(84.0))
                    .direction(narrow_scrollbar())
                    .style(move |_, status| theme::scroll(p, status)),
            );
        }
        composer = composer.push(
            row![attach, Space::new().width(Fill), picker, send]
                .spacing(8)
                .align_y(Alignment::Center),
        );
        container(composer)
            .padding([16, 16])
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(p.surface.into()),
                border: Border {
                    color: p.border,
                    width: 0.75,
                    radius: 12.0.into(),
                },
                shadow: Shadow {
                    color: Color {
                        a: 0.08,
                        ..p.accent
                    },
                    offset: Vector::new(0.0, 6.0),
                    blur_radius: 20.0,
                },
                ..Default::default()
            })
            .into()
    }
}

fn display_font() -> Font {
    Font {
        weight: iced::font::Weight::Medium,
        ..Font::with_name("Geist")
    }
}

fn narrow_scrollbar() -> scrollable::Direction {
    scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4))
}

fn hint<'a>(label: impl text::IntoFragment<'a>, p: Palette) -> Element<'a, Message> {
    container(text(label).size(12))
        .padding([7, 10])
        .style(move |_| theme::panel(p))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_navigation_keeps_the_chat_draft_and_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::with_store(Store {
            root: dir.path().to_path_buf(),
        });
        let _ = app.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        let file = PathBuf::from("notes.txt");
        let _ = app.update(Message::FilePicked(0, Some(file.clone())));

        for workspace in [
            Workspace::Setup,
            Workspace::Vault,
            Workspace::AgentControl,
            Workspace::Plugins,
        ] {
            let _ = app.update(Message::OpenWorkspace(workspace));
            assert_eq!(app.workspace, Some(workspace));
            assert_eq!(app.composer.text().trim_end(), "x");
            assert_eq!(app.sessions[0].attachments, vec![file.clone()]);
        }

        let _ = app.update(Message::SelectChat(0));
        assert_eq!(app.workspace, None);
        assert_eq!(app.composer.text().trim_end(), "x");
        let _ = app.update(Message::OpenWorkspace(Workspace::Vault));
        let _ = app.update(Message::NewChat);
        assert_eq!(app.workspace, None);
        assert!(app.composer.text().trim().is_empty());
        assert_eq!(app.sessions[0].draft.trim_end(), "x");
    }

    #[test]
    fn switching_chats_preserves_drafts_and_attachment_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::with_store(Store {
            root: dir.path().to_path_buf(),
        });
        let _ = app.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('α'),
        )));
        let _ = app.update(Message::NewChat);
        let second = app.active;
        assert_ne!(second, 0);
        assert!(app.composer.text().trim().is_empty());

        let file = PathBuf::from("design-notes.txt");
        let _ = app.update(Message::FilePicked(0, Some(file.clone())));
        let _ = app.update(Message::FilePicked(0, Some(file.clone())));
        assert!(app.sessions[second].attachments.is_empty());
        assert_eq!(app.sessions[0].attachments, vec![file]);

        let _ = app.update(Message::SelectChat(0));
        assert_eq!(app.composer.text().trim_end(), "α");
        let _ = app.update(Message::Send);
        assert_eq!(app.sessions[0].draft.trim_end(), "α");
        assert!(app.sessions[0].messages.is_empty());
        assert!(app.agents.notice.contains("workspace folder"));
        assert!(app.sessions[second].messages.is_empty());
    }

    #[test]
    fn new_chat_reuses_an_empty_chat_without_erasing_another_draft() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::with_store(Store {
            root: dir.path().to_path_buf(),
        });
        let _ = app.update(Message::NewChat);
        assert_eq!(app.sessions.len(), 1);
        let _ = app.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        let _ = app.update(Message::NewChat);
        assert_eq!(app.sessions.len(), 2);
        let _ = app.update(Message::SelectChat(0));
        let _ = app.update(Message::NewChat);
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.active, 1);
        assert_eq!(app.sessions[0].draft.trim_end(), "x");
    }

    #[test]
    fn workspace_switches_restore_only_their_chats_drafts_and_connections_after_restart() {
        use crate::workspace_ui::Action as W;
        let data = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let store = Store {
            root: data.path().to_path_buf(),
        };
        let mut app = App::with_store(store.clone());
        let _ = app.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste("First project draft".to_owned().into()),
        )));
        let _ = app.update(Message::Agents(agent_ui::Action::Workspace(W::Select(
            first.path().into(),
        ))));
        let first_chat = app.active;
        assert_eq!(app.composer.text(), "First project draft");
        let native_run = crate::harness::model::id();
        app.sessions[first_chat].run_id = Some(native_run.clone());
        let _ = app.update(Message::Agents(agent_ui::Action::EditTask(
            text_editor::Action::Edit(text_editor::Edit::Paste(
                "First agent task".to_owned().into(),
            )),
        )));
        let profile = app
            .agents
            .settings
            .profiles
            .iter()
            .position(|p| p.provider == crate::harness::model::Provider::ClaudeCode)
            .unwrap();
        let _ = app.update(Message::Agents(agent_ui::Action::ChooseProfile(
            agent_ui::Choice {
                index: profile,
                label: "Claude".into(),
            },
        )));
        let _ = app.update(Message::Agents(agent_ui::Action::Workspace(W::Select(
            second.path().into(),
        ))));
        assert_ne!(app.active, first_chat);
        assert!(app.agents.selected.is_none());
        assert!(app.composer.text().is_empty());
        assert!(app.agents.task.text().is_empty());
        assert_eq!(
            app.agents.settings.profiles[app.agents.profile].provider,
            crate::harness::model::Provider::Codex
        );
        let _ = app.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('B'),
        )));
        let _ = app.update(Message::Agents(agent_ui::Action::Workspace(W::Select(
            first.path().into(),
        ))));
        assert_eq!(app.active, first_chat);
        assert_eq!(app.agents.selected.as_ref(), Some(&native_run));
        assert_eq!(app.composer.text(), "First project draft");
        assert_eq!(app.agents.task.text(), "First agent task");
        assert_eq!(app.agents.profile, profile);
        let _ = app.update(Message::Pulse);
        drop(app);
        let restored = App::with_store(store);
        assert_eq!(
            restored.agents.settings.workspace,
            Some(first.path().canonicalize().unwrap())
        );
        assert_eq!(restored.composer.text(), "First project draft");
        assert_eq!(restored.agents.task.text(), "First agent task");
        assert_eq!(restored.agents.profile, profile);
        assert_eq!(restored.agents.selected.as_ref(), Some(&native_run));
        assert_eq!(restored.agents.workspaces.library.projects.len(), 2);
    }
}
