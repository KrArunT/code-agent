use crate::{
    config::AgentRole,
    provider::Message,
    workers::{now_epoch, registry_root_for_workspace, sanitize},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_SESSION_COMMANDS: usize = 300;
const MAX_SESSION_MESSAGES: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: Option<String>,
    pub resumed_from: Option<String>,
    pub workspace: PathBuf,
    pub role: AgentRole,
    pub command_history: Vec<String>,
    pub messages: Vec<Message>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl SessionRecord {
    pub fn new(workspace: PathBuf, role: AgentRole, id: String) -> Self {
        let now = now_epoch();
        Self {
            id,
            title: None,
            resumed_from: None,
            workspace,
            role,
            command_history: Vec::new(),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn resume_from(
        source: &SessionRecord,
        workspace: PathBuf,
        role: AgentRole,
        id: String,
    ) -> Self {
        let mut record = source.clone();
        record.id = id;
        record.resumed_from = Some(source.id.clone());
        record.workspace = workspace;
        record.role = role;
        record.created_at = now_epoch();
        record.updated_at = record.created_at;
        record
    }

    pub fn touch(&mut self) {
        self.updated_at = now_epoch();
        trim_session_record(self);
    }
}

pub fn make_session_id(role: AgentRole) -> String {
    let pid = std::process::id();
    format!(
        "{}-{}-{pid}",
        sanitize(&format!("{:?}", role).to_lowercase()),
        now_epoch()
    )
}

pub fn sessions_dir(workspace: &Path) -> Result<PathBuf> {
    Ok(registry_root_for_workspace(workspace)?.join("sessions"))
}

pub fn session_record_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    Ok(sessions_dir(workspace)?.join(format!("{id}.json")))
}

pub fn load_session_record(workspace: &Path, id: &str) -> Result<SessionRecord> {
    let path = session_record_path(workspace, id)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read session record {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse session record {}", path.display()))
}

pub fn save_session_record(workspace: &Path, record: &SessionRecord) -> Result<()> {
    let path = session_record_path(workspace, &record.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut record = record.clone();
    trim_session_record(&mut record);
    let text =
        serde_json::to_string_pretty(&record).context("failed to serialize session record")?;
    fs::write(&path, text)
        .with_context(|| format!("failed to write session record {}", path.display()))
}

pub fn list_session_records(workspace: &Path) -> Result<Vec<SessionRecord>> {
    let dir = sessions_dir(workspace)?;
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
        if let Ok(record) = serde_json::from_str::<SessionRecord>(&text) {
            records.push(record);
        }
    }
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(records)
}

pub fn summarize_session(record: &SessionRecord) -> String {
    let title = record
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("untitled");
    format!(
        "{} {} role={:?} commands={} messages={} workspace={} resumed_from={} updated_at={}",
        record.id,
        title,
        record.role,
        record.command_history.len(),
        record.messages.len(),
        record.workspace.display(),
        record.resumed_from.as_deref().unwrap_or("none"),
        record.updated_at
    )
}

pub fn session_tail_summary(record: &SessionRecord) -> String {
    let title = record
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("untitled");
    let commands = if record.command_history.is_empty() {
        "none".to_string()
    } else {
        record
            .command_history
            .iter()
            .rev()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let messages = if record.messages.is_empty() {
        "none".to_string()
    } else {
        record
            .messages
            .iter()
            .rev()
            .take(4)
            .map(|message| format!("{:?}: {}", message.role, task_excerpt(&message.content)))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "session {}\ntitle: {}\nworkspace: {}\nrole: {:?}\ncommands: {}\nmessages: {}\nresumed_from: {}\nlast_commands:\n{}\nlast_messages:\n{}",
        record.id,
        title,
        record.workspace.display(),
        record.role,
        record.command_history.len(),
        record.messages.len(),
        record
            .resumed_from
            .as_deref()
            .unwrap_or("none"),
        commands,
        messages
    )
}

pub fn session_history_summary(record: &SessionRecord) -> String {
    if record.command_history.is_empty() {
        "session history: none".to_string()
    } else {
        format!(
            "session history:\n{}",
            record
                .command_history
                .iter()
                .enumerate()
                .map(|(index, entry)| format!("{}: {}", index + 1, entry))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

pub fn session_title_from_input(input: &str) -> String {
    let title = task_excerpt(input);
    if title.is_empty() {
        "interactive session".to_string()
    } else {
        title
    }
}

fn trim_session_record(record: &mut SessionRecord) {
    if record.command_history.len() > MAX_SESSION_COMMANDS {
        let excess = record.command_history.len() - MAX_SESSION_COMMANDS;
        record.command_history.drain(0..excess);
    }
    if record.messages.len() > MAX_SESSION_MESSAGES {
        let excess = record.messages.len() - MAX_SESSION_MESSAGES;
        record.messages.drain(0..excess);
    }
}

fn task_excerpt(task: &str) -> String {
    let mut lines = task.lines();
    let preview = lines.by_ref().take(4).collect::<Vec<_>>().join(" ");
    let preview = preview.trim().to_string();
    if preview.is_empty() {
        String::new()
    } else if preview.len() > 120 {
        preview.chars().take(120).collect()
    } else {
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_session_copies_history_and_messages() {
        let mut source = SessionRecord::new(
            PathBuf::from("/tmp/workspace"),
            AgentRole::Master,
            "session-old".to_string(),
        );
        source.title = Some("kernel backport".to_string());
        source.command_history.push("/help".to_string());
        source.messages.push(Message {
            role: crate::provider::Role::User,
            content: "check the diff".to_string(),
        });

        let resumed = SessionRecord::resume_from(
            &source,
            PathBuf::from("/tmp/workspace"),
            AgentRole::Master,
            "session-new".to_string(),
        );

        assert_eq!(resumed.id, "session-new");
        assert_eq!(resumed.resumed_from.as_deref(), Some("session-old"));
        assert_eq!(resumed.title.as_deref(), Some("kernel backport"));
        assert_eq!(resumed.command_history, source.command_history);
        assert_eq!(resumed.messages.len(), 1);
    }

    #[test]
    fn history_summary_includes_commands() {
        let mut record = SessionRecord::new(
            PathBuf::from("/tmp/workspace"),
            AgentRole::Master,
            "session-old".to_string(),
        );
        record.command_history.push("/config show".to_string());
        record.command_history.push("/session history".to_string());

        let summary = session_history_summary(&record);
        assert!(summary.contains("/config show"));
        assert!(summary.contains("/session history"));
    }
}
