use super::{control::Control, process};
use std::{path::Path, sync::Arc};

pub async fn run(
    command: &str,
    root: &Path,
    seconds: u64,
    network: bool,
    control: Arc<Control>,
) -> Result<process::Output, String> {
    #[cfg(target_os = "macos")]
    {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let temp = root.join(".luxe-tmp");
        std::fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
        if std::fs::symlink_metadata(&temp)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Sandbox scratch directory must not be a symlink".into());
        }
        let root_literal =
            serde_json::to_string(&root.to_string_lossy()).map_err(|e| e.to_string())?;
        let temp_literal =
            serde_json::to_string(&temp.to_string_lossy()).map_err(|e| e.to_string())?;
        let networking = if network { "(allow network*)" } else { "" };
        let keychain = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Library/Keychains"));
        let private_keychain = match keychain {
            Some(path) => format!(
                "(deny file-read* file-write* (subpath {}))",
                serde_json::to_string(&path.to_string_lossy()).map_err(|e| e.to_string())?
            ),
            None => String::new(),
        };
        let mut git_read = String::new();
        if root.join(".git").is_file() {
            let query = process::run(
                "git",
                &[
                    "-c".into(),
                    "core.fsmonitor=false".into(),
                    "rev-parse".into(),
                    "--git-common-dir".into(),
                ],
                &root,
                3,
                None,
                Arc::new(Control::new(super::model::Mode::Plan)),
            )
            .await;
            if let Ok(output) = query
                && output.success
            {
                let path = root
                    .join(output.text.trim())
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                git_read = format!(
                    "(allow file-read* (subpath {}))",
                    serde_json::to_string(&path.to_string_lossy()).map_err(|e| e.to_string())?
                );
            }
        }
        let profile = format!(
            r#"(version 1)
(deny default)
(allow process* sysctl-read mach-lookup signal)
(allow file-read-metadata)
(allow file-read* (literal "/") (subpath "/System") (subpath "/Library/Developer") (subpath "/Applications/Xcode.app") (subpath "/opt/homebrew") (subpath "/usr") (subpath "/bin") (subpath "/sbin") (subpath "/dev") (subpath "/private/etc") (subpath "/private/var/db") (subpath {root_literal}))
(allow file-write* (subpath {root_literal}) (literal "/dev/null") (literal "/dev/dtracehelper"))
(deny file-read* file-write* (subpath (string-append {root_literal} "/.git")) (subpath (string-append {root_literal} "/.luxe")))
(allow file-read* (subpath (string-append {root_literal} "/.git")))
(allow file-read* (subpath (string-append {root_literal} "/.luxe/skills")))
{git_read}
(deny file-read* file-write* (regex #"(^|.*/)([.]env[^/]*|[.]ssh|[.]aws|[.]codex|[.]claude|credentials[^/]*|[^/]*[.]pem|[^/]*[.]key)(/.*)?$"))
{private_keychain}
(allow file-write* (subpath {temp_literal}))
{networking}"#
        );
        let shell = format!(
            "export TMPDIR={}; export TMP=$TMPDIR; export TEMP=$TMPDIR; {command}",
            shell_quote(&temp.to_string_lossy())
        );
        return process::run(
            "/usr/bin/sandbox-exec",
            &["-p".into(), profile, "/bin/sh".into(), "-c".into(), shell],
            &root,
            seconds,
            None,
            control,
        )
        .await;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (command, root, seconds, network, control);
        Err("A workspace OS sandbox is not configured for this platform; shell execution is disabled. Native agents use their own policies.".into())
    }
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
