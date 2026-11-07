use super::control::Control;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
pub struct Output {
    pub text: String,
    pub success: bool,
    pub code: Option<i32>,
}

struct Group(u32);
impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        // Every process is started in its own group; cancellation also kills descendants.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

pub fn command(program: &str, args: &[String], root: &Path) -> tokio::process::Command {
    let mut paths =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    // Finder does not inherit shell PATH; the macOS bundle carries its search helper.
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable
            .parent()
            .filter(|path| path.file_name().is_some_and(|name| name == "MacOS"))
    {
        paths.insert(0, directory.to_path_buf());
    }
    for path in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        let path = std::path::PathBuf::from(path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::join_paths(paths).unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .kill_on_drop(true);
    for name in ["CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "LANG"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    cmd
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    control: Arc<Control>,
) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let keep = n.min(512_000_usize.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
        control.append_output(&String::from_utf8_lossy(&chunk[..keep]));
        truncated |= keep < n;
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str("\n[Output exceeded 512 KB per stream; remaining bytes discarded.]");
    }
    Ok(text)
}

pub async fn run(
    program: &str,
    args: &[String],
    root: &Path,
    seconds: u64,
    input: Option<&str>,
    control: Arc<Control>,
) -> Result<Output, String> {
    control.checkpoint().await?;
    let mut child = command(program, args, root)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start {program}: {e}"))?;
    let _group = Group(child.id().ok_or("Process has no ID")?);
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let stdin = child.stdin.take();
    let work = async {
        let write = async {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin
                    .write_all(input.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(())
        };
        let (out, err, status, ()) = tokio::try_join!(
            read_bounded(stdout, control.clone()),
            read_bounded(stderr, control.clone()),
            async { child.wait().await.map_err(|e| e.to_string()) },
            write
        )?;
        Ok(Output {
            success: status.success(),
            code: status.code(),
            text: format!("{out}{err}"),
        })
    };
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(seconds), work) => result.map_err(|_| format!("{program} timed out after {seconds}s"))?,
        _ = control.cancelled() => Err("Stopped by user".into()),
    }
}
