use super::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

static GRANTS: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    workspace: PathBuf,
    tool: String,
    input: Value,
}

fn scope(root: &Path, tool: &str, input: &Value) -> Result<Grant, String> {
    Ok(Grant {
        workspace: root.canonicalize().map_err(|e| e.to_string())?,
        tool: tool.into(),
        input: input.clone(),
    })
}

pub fn granted(store: &Store, root: &Path, tool: &str, input: &Value) -> Result<bool, String> {
    let _lock = GRANTS.lock().unwrap();
    let grants: Vec<Grant> = store.load("permissions.json")?.unwrap_or_default();
    Ok(grants.contains(&scope(root, tool, input)?))
}

pub fn remember(store: &Store, root: &Path, tool: &str, input: &Value) -> Result<(), String> {
    let _lock = GRANTS.lock().unwrap();
    let grant = scope(root, tool, input)?;
    let mut grants: Vec<Grant> = store.load("permissions.json")?.unwrap_or_default();
    if !grants.contains(&grant) {
        if grants.len() >= 128 || input.to_string().len() > 64000 {
            return Err(
                "Remembered approval limit reached; allow this action once instead.".into(),
            );
        }
        grants.push(grant);
    }
    store.save("permissions.json", &grants)
}

pub fn revoke(store: &Store) -> Result<(), String> {
    let _lock = GRANTS.lock().unwrap();
    store.save("permissions.json", &Vec::<Grant>::new())
}

pub fn screened(task: &str, reply: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(reply) else {
        return false;
    };
    value["verdict"] == "allow"
        && value["user_quote"]
            .as_str()
            .is_some_and(|quote| quote.len() >= 12 && task.contains(quote))
}
