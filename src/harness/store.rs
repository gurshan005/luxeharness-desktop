use super::model::{RunRecord, Trace};
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn open() -> Self {
        let root = std::env::var_os("LUXE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
                if cfg!(target_os = "macos") {
                    home.join("Library/Application Support/LuxeHarness")
                } else {
                    std::env::var_os("XDG_DATA_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(".local/share"))
                        .join("luxeharness")
                }
            });
        Self { root }
    }

    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>, String> {
        match fs::read(self.root.join(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("Cannot read {name}: {e}. The original file was kept.")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn save(&self, name: &str, value: &impl Serialize) -> Result<(), String> {
        let path = self.root.join(name);
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        atomic_write(&path, &bytes)
    }

    pub fn run_path(&self, run: &str) -> PathBuf {
        self.root.join("runs").join(run)
    }

    pub fn record(&self, record: &RunRecord) -> Result<(), String> {
        self.save(&format!("runs/{}/session.json", record.spec.id), record)
    }

    pub fn trace(&self, trace: &Trace) -> Result<(), String> {
        let path = self.run_path(&trace.run).join("events.jsonl");
        prepare_parent(&path)?;
        let mut file = private_options()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut bytes = serde_json::to_vec(trace).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(|e| e.to_string())
    }

    pub fn runs(&self) -> Result<Vec<RunRecord>, String> {
        let mut records = Vec::new();
        let paths = match fs::read_dir(self.root.join("runs")) {
            Ok(paths) => paths,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(e) => return Err(e.to_string()),
        };
        for path in paths {
            let path = path.map_err(|e| e.to_string())?.path().join("session.json");
            if !path.is_file() {
                continue;
            }
            let mut record: RunRecord =
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            if !matches!(record.status.as_str(), "Completed" | "Failed" | "Stopped") {
                record.status = "Interrupted · resume available".into();
            }
            records.push(record);
        }
        records.sort_by_key(|r| std::cmp::Reverse(r.started));
        Ok(records)
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn prepare_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    prepare_parent(path)?;
    let temp = path.with_extension(format!("{}.tmp", super::model::id()));
    let result = (|| {
        let mut file = private_options()
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
