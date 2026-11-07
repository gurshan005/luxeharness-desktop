use crate::harness::{control::Control, model::Mode, process, store::Store, tools};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DOCUMENT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub root: PathBuf,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub draft: String,
}

impl Project {
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .unwrap_or(self.root.as_os_str())
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub projects: Vec<Project>,
    pub active: Option<PathBuf>,
}

impl Library {
    pub fn select(&mut self, path: &Path) -> Result<(), String> {
        let root = path
            .canonicalize()
            .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
        if !root.is_dir() {
            return Err("Choose a workspace folder, not a file.".into());
        }
        let project = self
            .projects
            .iter()
            .position(|p| p.root == root)
            .map(|index| self.projects.remove(index))
            .unwrap_or_else(|| Project {
                root: root.clone(),
                profile_id: None,
                draft: String::new(),
            });
        self.projects.insert(0, project);
        self.active = Some(root);
        Ok(())
    }

    pub fn remember(&mut self, path: &Path) {
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.projects.iter().any(|p| p.root == root) {
            self.projects.push(Project {
                root,
                profile_id: None,
                draft: String::new(),
            });
        }
    }

    pub fn save(&self, store: &Store) -> Result<(), String> {
        store.save("workspaces.json", self)
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub name: &'static str,
    pub original: Option<String>,
    pub text: String,
}

fn read_document(root: &Path, name: &str) -> Result<Option<String>, String> {
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(format!(
                "{name} must be a regular file inside this workspace."
            ));
        }
        Ok(meta) if meta.len() > DOCUMENT_LIMIT as u64 => {
            return Err(format!("{name} exceeds the 64 KB workspace-editor limit."));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read {name}: {e}")),
    }
    let bytes = tools::read_bytes(&path)?;
    if bytes.len() > DOCUMENT_LIMIT {
        return Err(format!("{name} exceeds the 64 KB workspace-editor limit."));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{name} is not UTF-8 text."))
}

impl Document {
    fn load(root: &Path, name: &'static str, template: String) -> Result<Self, String> {
        let original = read_document(root, name)?;
        let text = original.clone().unwrap_or(template);
        Ok(Self {
            name,
            original,
            text,
        })
    }

    pub fn validate(&self, root: &Path, text: &str) -> Result<(), String> {
        if text.len() > DOCUMENT_LIMIT {
            return Err(format!("Keep {} below 64 KB.", self.name));
        }
        if root.canonicalize().map_err(|e| e.to_string())? != root {
            return Err(
                "The workspace location changed. Open the folder again before saving.".into(),
            );
        }
        if read_document(root, self.name)? != self.original {
            return Err(format!(
                "{} changed on disk. Reload it before saving; your draft has been kept.",
                self.name
            ));
        }
        Ok(())
    }

    pub fn save(&mut self, root: &Path, text: &str) -> Result<(), String> {
        self.validate(root, text)?;
        if self.original.as_deref() == Some(text) {
            return Ok(());
        }
        let path = root.join(self.name);
        if self.original.is_none() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("Cannot create {}: {e}", self.name))?;
            if let Err(error) = file
                .write_all(text.as_bytes())
                .and_then(|_| file.sync_all())
            {
                let _ = fs::remove_file(&path);
                return Err(format!("Cannot save {}: {error}", self.name));
            }
        } else {
            let temporary = root.join(format!(".{}.luxe-tmp", uuid::Uuid::new_v4()));
            let result = (|| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                fs::set_permissions(&temporary, fs::metadata(&path)?.permissions())?;
                self.validate(root, text).map_err(std::io::Error::other)?;
                fs::rename(&temporary, &path)
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(&temporary);
                return Err(format!("Cannot save {}: {error}", self.name));
            }
        }
        self.original = Some(text.to_owned());
        self.text = text.to_owned();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub root: PathBuf,
    pub name: String,
    pub stack: String,
    pub entries: Vec<String>,
    pub git: String,
    pub git_root: Option<PathBuf>,
    pub agents: Document,
    pub memory: Document,
}

fn inspect_files(root: PathBuf) -> Result<Inspection, String> {
    if !root.is_dir() || root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err(
            "This workspace folder moved or is unavailable. Open its current location.".into(),
        );
    }
    let name = root
        .file_name()
        .unwrap_or(root.as_os_str())
        .to_string_lossy()
        .into_owned();
    let rust = root.join("Cargo.toml").is_file();
    let stack = if rust {
        "Rust · Cargo"
    } else {
        "Project folder"
    }
    .to_string();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())?.take(1000) {
        let entry = entry.map_err(|e| e.to_string())?;
        let filename = entry.file_name().to_string_lossy().into_owned();
        if matches!(
            filename.as_str(),
            "target" | "node_modules" | "dist" | ".qa"
        ) || tools::resolve(&root, &filename).is_err()
            || entry.file_type().map_err(|e| e.to_string())?.is_symlink()
        {
            continue;
        }
        entries.push(format!(
            "{filename}{}",
            if entry.path().is_dir() { "/" } else { "" }
        ));
    }
    entries.sort();
    entries.truncate(40);
    let commands = if rust {
        let locked = if root.join("Cargo.lock").is_file() {
            " --locked"
        } else {
            ""
        };
        format!(
            "```sh\ncargo fmt --check\ncargo test{locked}\ncargo clippy --all-targets{locked} -- -D warnings\n```"
        )
    } else {
        "Use the build and verification commands documented by this project. Record them here once verified.".into()
    };
    let map = entries
        .iter()
        .filter(|name| name.ends_with('/'))
        .take(12)
        .map(|name| format!("- `{name}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let agents = Document::load(
        &root,
        "AGENTS.md",
        format!(
            "# Project instructions\n\n## Project\n\nThis workspace is {name}. Read README.md when present and MEMORY.md for shared decisions and handoff context.\n\n## Working agreements\n\n- Follow the existing architecture, languages and component libraries.\n- Inspect the relevant files before editing and keep changes focused on the task.\n- Verify changes using the project's actual checks; report results and unresolved work.\n- Keep credentials, account state and local run logs out of project files.\n\n## Verification\n\n{commands}\n\n## Directories\n\n{map}\n\n## Parallel work\n\nUse a separate Git worktree for each concurrent editing agent. Preserve other work. Propose lasting memory updates with the task's changes; keep individual run notes in Luxe.\n"
        ),
    )?;
    let memory = Document::load(
        &root,
        "MEMORY.md",
        format!(
            "# Project memory\n\nShared decisions and verified context for {name}. Keep this concise; individual conversations and run logs live in Luxe.\n\n## Decisions\n\n## Known issues\n\n## Next steps\n"
        ),
    )?;
    Ok(Inspection {
        root,
        name,
        stack,
        entries,
        git: "Checking Git…".into(),
        git_root: None,
        agents,
        memory,
    })
}

pub async fn inspect(root: PathBuf) -> Result<Inspection, String> {
    let mut inspection = tokio::task::spawn_blocking(move || inspect_files(root))
        .await
        .map_err(|e| e.to_string())??;
    let control = Arc::new(Control::new(Mode::Plan));
    let args = [
        "-c",
        "core.fsmonitor=false",
        "--no-optional-locks",
        "rev-parse",
        "--show-toplevel",
    ]
    .map(str::to_owned);
    match process::run("git", &args, &inspection.root, 5, None, control.clone()).await {
        Ok(output) if output.success => {
            inspection.git_root = Some(PathBuf::from(output.text.trim()));
            let args = [
                "-c",
                "core.fsmonitor=false",
                "--no-optional-locks",
                "status",
                "--short",
                "--branch",
            ]
            .map(str::to_owned);
            inspection.git = match process::run("git", &args, &inspection.root, 5, None, control)
                .await
            {
                Ok(status) if status.success => tools::clip(status.text.trim(), 1500),
                Ok(status) => format!("Git status unavailable: {}", tools::clip(&status.text, 300)),
                Err(error) => error,
            };
        }
        Ok(_) => inspection.git = "No Git repository · folder mode available".into(),
        Err(_) => inspection.git = "Git unavailable · folder mode available".into(),
    }
    Ok(inspection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_library_round_trips_and_canonical_paths_do_not_duplicate_projects() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store {
            root: data.path().into(),
        };
        let mut library = Library::default();
        library.select(root.path()).unwrap();
        library.projects[0].profile_id = Some("codex-profile".into());
        library.select(&root.path().join(".")).unwrap();
        library.save(&store).unwrap();
        let saved: Library = store.load("workspaces.json").unwrap().unwrap();
        assert_eq!(saved.projects.len(), 1);
        assert_eq!(saved.active, Some(root.path().canonicalize().unwrap()));
        assert_eq!(
            saved.projects[0].profile_id.as_deref(),
            Some("codex-profile")
        );
        assert!(library.select(&root.path().join("missing")).is_err());
        assert_eq!(library.active, saved.active);
    }

    #[test]
    fn onboarding_preserves_existing_instructions_and_refuses_stale_and_symlink_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("AGENTS.md"), "Existing team rules\n").unwrap();
        let mut inspection = inspect_files(root.clone()).unwrap();
        assert_eq!(inspection.agents.text, "Existing team rules\n");
        assert!(inspection.memory.original.is_none());
        inspection.memory.save(&root, "# Shared memory\n").unwrap();
        fs::write(root.join("AGENTS.md"), "A teammate changed this\n").unwrap();
        assert!(inspection.agents.save(&root, "My older edit\n").is_err());
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "A teammate changed this\n"
        );
        let mut fresh = inspect_files(root.clone()).unwrap();
        fresh.agents.save(&root, "Updated team rules\n").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "Updated team rules\n"
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("rules"), "Outside rules").unwrap();
            fs::remove_file(root.join("AGENTS.md")).unwrap();
            std::os::unix::fs::symlink(outside.path().join("rules"), root.join("AGENTS.md"))
                .unwrap();
            assert!(inspect_files(root.clone()).is_err());
            assert!(fresh.agents.save(&root, "No escape").is_err());
            assert_eq!(
                fs::read_to_string(outside.path().join("rules")).unwrap(),
                "Outside rules"
            );
        }
    }

    #[test]
    fn templates_use_project_commands_and_saving_does_not_change_folder_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"example\"\n").unwrap();
        fs::write(root.join("Cargo.lock"), "# lock").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        #[cfg(unix)]
        let before = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&root).unwrap().permissions().mode()
        };
        let mut inspection = inspect_files(root.clone()).unwrap();
        assert!(inspection.agents.text.contains("cargo test --locked"));
        assert!(inspection.agents.text.contains("`src/`"));
        for doc in [&mut inspection.agents, &mut inspection.memory] {
            doc.save(&root, &doc.text.clone()).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&root).unwrap().permissions().mode(), before);
        }
        assert!(read_document(&root, "AGENTS.md").unwrap().is_some());
        assert!(read_document(&root, "MEMORY.md").unwrap().is_some());
    }
}
