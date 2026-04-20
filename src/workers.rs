use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Starting,
    Running,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub id: String,
    pub name: String,
    pub task: String,
    pub workspace: PathBuf,
    pub branch: String,
    pub config_file: PathBuf,
    pub task_file: PathBuf,
    pub log_file: PathBuf,
    pub pid: Option<u32>,
    pub status: WorkerStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub exit_status: Option<i32>,
}

pub fn registry_root_for_workspace(workspace: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(workspace)
        .output();

    let root = match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                workspace.join(".autofix")
            } else {
                let path = PathBuf::from(text);
                if path.is_absolute() {
                    path
                } else {
                    workspace.join(path)
                }
            }
        }
        _ => workspace.join(".autofix"),
    };

    Ok(root.join("autofix"))
}

pub fn workers_dir(workspace: &Path) -> Result<PathBuf> {
    Ok(registry_root_for_workspace(workspace)?.join("workers"))
}

pub fn load_worker_record(workspace: &Path, id: &str) -> Result<WorkerRecord> {
    let path = worker_record_path(workspace, id)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read worker record {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse worker record {}", path.display()))
}

pub fn save_worker_record(workspace: &Path, record: &WorkerRecord) -> Result<()> {
    let path = worker_record_path(workspace, &record.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(record).context("failed to serialize worker record")?;
    fs::write(&path, text)
        .with_context(|| format!("failed to write worker record {}", path.display()))
}

pub fn list_worker_records(workspace: &Path) -> Result<Vec<WorkerRecord>> {
    let dir = workers_dir(workspace)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        if let Ok(record) = serde_json::from_str::<WorkerRecord>(&text) {
            records.push(record);
        }
    }
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(records)
}

pub fn worker_record_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    Ok(workers_dir(workspace)?.join(format!("{id}.json")))
}

pub fn worker_log_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    Ok(workers_dir(workspace)?.join(format!("{id}.log")))
}

pub fn worker_task_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    Ok(workers_dir(workspace)?.join(format!("{id}.task.md")))
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn make_worker_id(name: &str) -> String {
    format!("{}-{}", sanitize(name), now_epoch())
}

pub fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn summarize_worker(record: &WorkerRecord) -> String {
    format!(
        "{} {} status={:?} pid={} branch={} workspace={}",
        record.id,
        record.name,
        record.status,
        record
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "none".to_string()),
        record.branch,
        record.workspace.display()
    )
}

pub fn worker_tail_summary(record: &WorkerRecord) -> String {
    format!(
        "worker {}\nstatus: {:?}\nworkspace: {}\nbranch: {}\nconfig: {}\nlog: {}\nexit_status: {}",
        record.id,
        record.status,
        record.workspace.display(),
        record.branch,
        record.config_file.display(),
        record.log_file.display(),
        record
            .exit_status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "none".to_string())
    )
}

pub fn task_excerpt(task: &str) -> String {
    let mut lines = task.lines();
    let preview = lines.by_ref().take(8).collect::<Vec<_>>().join("\n");
    if preview.trim().is_empty() {
        "no task text".to_string()
    } else if lines.next().is_some() {
        format!("{preview}\n...")
    } else {
        preview
    }
}
