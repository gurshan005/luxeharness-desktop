use crate::harness::store::Store;
use crate::icons::{Icon, icon};
use crate::theme::{self, Palette};
use crate::workspace::{self, Inspection, Library};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_editor};
use iced::{Alignment, Border, Element, Fill, Font, Task};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Action {
    Open,
    Create,
    Opened(Option<PathBuf>),
    Created(Option<PathBuf>),
    Select(PathBuf),
    Refresh,
    Discard,
    Inspected(u64, PathBuf, Result<Box<Inspection>, String>),
    Tab(bool),
    Edit(text_editor::Action),
    Save,
    OpenCodex,
}

pub struct Panel {
    pub library: Library,
    pub notice: String,
    pub inspection: Option<Inspection>,
    store: Store,
    writable: bool,
    loading: bool,
    generation: u64,
    memory_tab: bool,
    agents: text_editor::Content,
    memory: text_editor::Content,
    agents_base: String,
    memory_base: String,
}

impl Panel {
    pub fn new(store: Store, previous: Option<PathBuf>, run_roots: &[PathBuf]) -> Self {
        let (mut library, writable, mut notice) = match store.load::<Library>("workspaces.json") {
            Ok(saved) => (saved.unwrap_or_default(), true, String::new()),
            Err(error) => (Library::default(), false, error),
        };
        if writable {
            if let Some(root) = previous {
                library.remember(&root);
                if library.active.is_none() {
                    library.active = Some(root.canonicalize().unwrap_or(root));
                }
            }
            for root in run_roots {
                library.remember(root);
            }
            if let Some(root) = library.active.clone() {
                library.remember(&root);
            }
            if let Err(error) = library.save(&store) {
                notice = error;
            }
        } else if let Some(root) = previous {
            library.remember(&root);
            library.active = Some(root.canonicalize().unwrap_or(root));
        }
        Self {
            library,
            store,
            writable,
            notice,
            inspection: None,
            loading: false,
            generation: 0,
            memory_tab: false,
            agents: text_editor::Content::new(),
            memory: text_editor::Content::new(),
            agents_base: String::new(),
            memory_base: String::new(),
        }
    }

    pub fn dirty(&self) -> bool {
        self.inspection.is_some()
            && (self.agents.text() != self.agents_base || self.memory.text() != self.memory_base)
    }

    pub fn ready(&self) -> bool {
        !self.loading
            && !self.dirty()
            && self.inspection.as_ref().is_some_and(|info| {
                info.agents.original.is_some() && info.memory.original.is_some()
            })
    }

    pub fn remember_preferences(&mut self, profile: String, draft: String) {
        if !self.writable {
            return;
        }
        if let Some(project) = self
            .library
            .projects
            .iter_mut()
            .find(|p| Some(&p.root) == self.library.active.as_ref())
            && (project.profile_id.as_ref() != Some(&profile) || project.draft != draft)
        {
            project.profile_id = Some(profile);
            project.draft = draft;
            if let Err(error) = self.library.save(&self.store) {
                self.notice = error;
            }
        }
    }

    fn can_switch(&mut self) -> bool {
        if self.dirty() {
            self.notice =
                "Save your workspace edits, or discard the draft, before switching folders.".into();
            false
        } else if !self.writable {
            self.notice =
                "The workspace list could not be loaded. Its original file has been preserved."
                    .into();
            false
        } else {
            true
        }
    }

    fn refresh(&mut self) -> Task<Action> {
        let Some(root) = self.library.active.clone() else {
            return Task::none();
        };
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.inspection = None;
        self.notice.clear();
        Task::perform(
            async move {
                let result = workspace::inspect(root.clone()).await.map(Box::new);
                (root, result)
            },
            move |(root, result)| Action::Inspected(generation, root, result),
        )
    }

    pub fn update(&mut self, action: Action) -> Task<Action> {
        match action {
            Action::Open | Action::Create => {
                if !self.can_switch() {
                    return Task::none();
                }
                let create = matches!(action, Action::Create);
                return Task::perform(
                    async move {
                        let dialog = rfd::AsyncFileDialog::new();
                        if create {
                            dialog
                                .set_title("Create a workspace folder")
                                .set_file_name("New workspace")
                                .save_file()
                                .await
                                .map(|file| file.path().to_path_buf())
                        } else {
                            dialog
                                .set_title("Open a workspace folder")
                                .pick_folder()
                                .await
                                .map(|file| file.path().to_path_buf())
                        }
                    },
                    move |path| {
                        if create {
                            Action::Created(path)
                        } else {
                            Action::Opened(path)
                        }
                    },
                );
            }
            Action::Opened(Some(root)) | Action::Select(root) => {
                if !self.can_switch() {
                    return Task::none();
                }
                let mut library = self.library.clone();
                match library
                    .select(&root)
                    .and_then(|_| library.save(&self.store))
                {
                    Ok(()) => {
                        self.library = library;
                        self.memory_tab = false;
                        return self.refresh();
                    }
                    Err(error) => self.notice = error,
                }
            }
            Action::Created(Some(root)) => {
                if !self.can_switch() {
                    return Task::none();
                }
                match std::fs::create_dir(&root) {
                    Ok(()) => return self.update(Action::Opened(Some(root))),
                    Err(error) => {
                        self.notice =
                            format!("Cannot create this folder: {error}. Choose a new folder name.")
                    }
                }
            }
            Action::Opened(None) | Action::Created(None) => {}
            Action::Refresh => {
                if self.dirty() {
                    self.notice = "Save your edits or use Discard draft & reload first.".into();
                } else {
                    return self.refresh();
                }
            }
            Action::Discard => return self.refresh(),
            Action::Inspected(generation, root, result) => {
                if generation != self.generation || self.library.active.as_ref() != Some(&root) {
                    return Task::none();
                }
                self.loading = false;
                match result {
                    Ok(info) => {
                        let info = *info;
                        self.agents = text_editor::Content::with_text(&info.agents.text);
                        self.memory = text_editor::Content::with_text(&info.memory.text);
                        self.agents_base = self.agents.text();
                        self.memory_base = self.memory.text();
                        self.inspection = Some(info);
                    }
                    Err(error) => self.notice = error,
                }
            }
            Action::Tab(memory) => self.memory_tab = memory,
            Action::Edit(action) => {
                if self.memory_tab {
                    self.memory.perform(action)
                } else {
                    self.agents.perform(action)
                }
            }
            Action::Save => {
                let Some(info) = &mut self.inspection else {
                    return Task::none();
                };
                let agents_text = self.agents.text();
                let memory_text = self.memory.text();
                let save_agents = info.agents.original.is_none() || agents_text != self.agents_base;
                let save_memory = info.memory.original.is_none() || memory_text != self.memory_base;
                let result = (|| {
                    if save_agents {
                        info.agents.validate(&info.root, &agents_text)?;
                    }
                    if save_memory {
                        info.memory.validate(&info.root, &memory_text)?;
                    }
                    if save_agents {
                        info.agents.save(&info.root, &agents_text)?;
                        self.agents_base = agents_text;
                    }
                    if save_memory {
                        info.memory.save(&info.root, &memory_text)?;
                        self.memory_base = memory_text;
                    }
                    Ok::<_, String>(())
                })();
                match result {
                    Ok(()) => {
                        for name in ["AGENTS.md", "MEMORY.md"] {
                            if !info.entries.iter().any(|entry| entry == name) {
                                info.entries.push(name.into());
                            }
                        }
                        info.entries.sort();
                        if save_agents || save_memory {
                            info.git =
                                "Workspace files saved · refresh to update Git status".into();
                        }
                        self.notice = "Workspace files saved. New Codex chats will read this project's instructions and memory.".into();
                    }
                    Err(error) => self.notice = error,
                }
            }
            Action::OpenCodex => {}
        }
        Task::none()
    }

    pub fn view(&self, p: Palette) -> Element<'_, Action> {
        let secondary = |label, action| {
            button(text(label).size(12))
                .on_press(action)
                .padding([9, 12])
                .style(move |_, status| theme::subtle(p, status))
        };
        let header = row![
            column![
                text("Workspace").size(28),
                text("A folder, shared instructions, and your agents.")
                    .size(13)
                    .color(p.muted)
            ]
            .spacing(7)
            .width(Fill),
            secondary("Open folder", Action::Open),
            secondary("New workspace", Action::Create),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut content = column![header].spacing(22);
        if !self.notice.is_empty() {
            content = content.push(text(&self.notice).size(12).color(p.accent));
        }
        let Some(root) = &self.library.active else {
            return content.push(container(column![
                icon(Icon::Folder, 32.0, p.accent),
                text("Choose a place to build").size(24),
                text("Open an existing project or create a folder. Then review its instructions and memory before starting Codex.").size(14).color(p.muted),
                button("Open a folder").padding([11, 18]).style(theme::primary).on_press(Action::Open),
            ].spacing(18).max_width(480)).padding([60, 16])).into();
        };
        let Some(info) = &self.inspection else {
            return content
                .push(
                    text(root.to_string_lossy().into_owned())
                        .size(13)
                        .color(p.muted),
                )
                .push(
                    text(if self.loading {
                        "Reading workspace files…"
                    } else {
                        "Workspace details are unavailable."
                    })
                    .size(16),
                )
                .push(secondary("Refresh workspace", Action::Refresh))
                .into();
        };
        content = content.push(
            column![
                text(&info.name).size(22).color(p.accent),
                text(root.to_string_lossy().into_owned())
                    .size(12)
                    .color(p.muted),
            ]
            .spacing(5),
        );
        let mut files = column![
            text("PROJECT").size(10).color(p.faint),
            text(&info.stack).size(13),
            Space::new().height(10)
        ]
        .spacing(7);
        for entry in &info.entries {
            files = files.push(
                row![
                    icon(
                        if entry.ends_with('/') {
                            Icon::Folder
                        } else {
                            Icon::File
                        },
                        13.0,
                        p.faint
                    ),
                    text(entry).size(12),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        let details = column![
            files,
            Space::new().height(12),
            text("GIT").size(10).color(p.faint),
            text(&info.git).size(11).color(p.muted),
            secondary("Refresh", Action::Refresh),
        ]
        .spacing(9)
        .width(210);
        let mut tabs = row![].spacing(4);
        for (name, memory, new) in [
            ("AGENTS.md", false, info.agents.original.is_none()),
            ("MEMORY.md", true, info.memory.original.is_none()),
        ] {
            let selected = self.memory_tab == memory;
            tabs = tabs.push(
                button(
                    row![
                        text(name).size(12),
                        text(if new { "New" } else { "" }).size(10).color(p.faint)
                    ]
                    .spacing(8),
                )
                .on_press(Action::Tab(memory))
                .padding([9, 12])
                .style(move |_, status| {
                    let mut style = theme::subtle(p, status);
                    if selected {
                        style.background = Some(p.accent_soft.into());
                    }
                    style
                }),
            );
        }
        let caption = if self.memory_tab {
            "Shared decisions, known issues and the next useful steps."
        } else {
            "Project guidance Codex discovers when it starts in this folder."
        };
        let editor = text_editor(if self.memory_tab {
            &self.memory
        } else {
            &self.agents
        })
        .on_action(Action::Edit)
        .font(Font::MONOSPACE)
        .size(12)
        .padding(16)
        .height(320)
        .style(move |_, _| text_editor::Style {
            background: p.surface.into(),
            border: Border {
                radius: 10.0.into(),
                width: 1.0,
                color: p.border,
            },
            placeholder: p.faint,
            value: p.text,
            selection: p.accent_soft,
        });
        let mut save = button(if self.dirty() {
            "Save changes"
        } else {
            "Save workspace files"
        })
        .padding([10, 15])
        .style(theme::primary);
        if self.dirty() || info.agents.original.is_none() || info.memory.original.is_none() {
            save = save.on_press(Action::Save);
        }
        let mut open = button("Open Codex chat")
            .padding([10, 15])
            .style(theme::primary);
        if self.ready() {
            open = open.on_press(Action::OpenCodex);
        }
        let mut actions = row![save, open].spacing(10);
        if self.dirty() {
            actions = actions.push(secondary("Discard draft & reload", Action::Discard));
        }
        let guidance = column![tabs, text(caption).size(12).color(p.muted), editor, actions.wrap(),
            text(if self.ready() { "Ready · Codex will run in this folder. Commit shared guidance before creating parallel worktrees." } else { "Review both files and save them to finish workspace setup. Existing files stay intact until you edit and save." }).size(11).color(p.muted),
        ].spacing(14).width(Fill);
        content = content.push(row![details, guidance].spacing(28));
        scrollable(content)
            .height(Fill)
            .style(move |_, status| theme::scroll(p, status))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspect(root: PathBuf) -> Inspection {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(workspace::inspect(root))
            .unwrap()
    }

    #[test]
    fn onboarding_saves_both_files_and_keeps_unsaved_edits_in_their_workspace() {
        let data = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let root = first.path().canonicalize().unwrap();
        std::fs::write(root.join("AGENTS.md"), "Existing team instructions\n").unwrap();
        let mut panel = Panel::new(
            Store {
                root: data.path().into(),
            },
            None,
            &[],
        );
        let _ = panel.update(Action::Select(root.clone()));
        let _ = panel.update(Action::Inspected(
            panel.generation,
            root.clone(),
            Ok(Box::new(inspect(root.clone()))),
        ));
        assert_eq!(panel.agents.text(), "Existing team instructions\n");
        assert!(!panel.ready());
        let _ = panel.update(Action::Save);
        assert!(panel.ready());
        assert_eq!(
            std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "Existing team instructions\n"
        );
        assert!(root.join("MEMORY.md").is_file());
        let _ = panel.update(Action::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('X'),
        )));
        let edited = panel.agents.text();
        assert!(panel.dirty());
        let _ = panel.update(Action::Select(second.path().into()));
        assert_eq!(panel.library.active, Some(root.clone()));
        assert_eq!(panel.agents.text(), edited);
        std::fs::write(root.join("AGENTS.md"), "A teammate's newer rules\n").unwrap();
        let _ = panel.update(Action::Save);
        assert!(panel.notice.contains("changed on disk"));
        assert_eq!(panel.agents.text(), edited);
        assert_eq!(
            std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "A teammate's newer rules\n"
        );
        let _ = panel.update(Action::Discard);
        let _ = panel.update(Action::Inspected(
            panel.generation,
            root.clone(),
            Ok(Box::new(inspect(root))),
        ));
        assert!(!panel.dirty());
        let _ = panel.update(Action::Select(second.path().into()));
        assert_eq!(
            panel.library.active,
            Some(second.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn late_folder_inspections_cannot_replace_the_new_workspace() {
        let data = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut panel = Panel::new(
            Store {
                root: data.path().into(),
            },
            None,
            &[],
        );
        let _ = panel.update(Action::Select(first.path().into()));
        let first_generation = panel.generation;
        let _ = panel.update(Action::Select(second.path().into()));
        let _ = panel.update(Action::Inspected(
            first_generation,
            first.path().canonicalize().unwrap(),
            Ok(Box::new(inspect(first.path().canonicalize().unwrap()))),
        ));
        assert!(panel.inspection.is_none());
        assert!(panel.loading);
        let root = second.path().canonicalize().unwrap();
        let _ = panel.update(Action::Inspected(
            panel.generation,
            root.clone(),
            Ok(Box::new(inspect(root.clone()))),
        ));
        assert_eq!(panel.inspection.as_ref().unwrap().root, root);
        assert!(!panel.loading);
    }

    #[test]
    fn creating_a_workspace_never_replaces_an_existing_folder() {
        let data = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("New project");
        let mut panel = Panel::new(
            Store {
                root: data.path().into(),
            },
            None,
            &[],
        );
        let _ = panel.update(Action::Created(Some(root.clone())));
        assert!(root.is_dir());
        assert_eq!(panel.library.active, Some(root.canonicalize().unwrap()));
        assert!(!root.join("AGENTS.md").exists());
        std::fs::write(root.join("keep.txt"), "Existing project work").unwrap();
        let _ = panel.update(Action::Created(Some(root.clone())));
        assert!(panel.notice.contains("Cannot create"));
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "Existing project work"
        );
    }

    #[test]
    fn corrupt_workspace_metadata_preserves_the_file_and_previous_folder() {
        let data = tempfile::tempdir().unwrap();
        let previous = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = data.path().join("workspaces.json");
        std::fs::write(&path, "corrupt workspace metadata").unwrap();
        let mut panel = Panel::new(
            Store {
                root: data.path().into(),
            },
            Some(previous.path().into()),
            &[],
        );
        let root = previous.path().canonicalize().unwrap();
        assert_eq!(panel.library.active, Some(root.clone()));
        let _ = panel.update(Action::Select(other.path().into()));
        panel.remember_preferences("new-profile".into(), "keep this draft".into());
        assert_eq!(panel.library.active, Some(root));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "corrupt workspace metadata"
        );
    }
}
