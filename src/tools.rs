use crate::config::PermissionMode;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::Command;
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolCall {
    ListFiles { path: Option<String> },
    ReadFile { path: String },
    WriteFile { path: String, content: String },
    RunShell { command: String },
}

pub struct ToolRuntime {
    workspace: PathBuf,
    shell_permission: PermissionMode,
    write_permission: PermissionMode,
}

impl ToolRuntime {
    pub fn new(
        workspace: PathBuf,
        shell_permission: PermissionMode,
        write_permission: PermissionMode,
    ) -> Self {
        Self {
            workspace,
            shell_permission,
            write_permission,
        }
    }

    pub fn shell_permission(&self) -> PermissionMode {
        self.shell_permission
    }

    pub fn write_permission(&self) -> PermissionMode {
        self.write_permission
    }

    pub fn set_shell_permission(&mut self, permission: PermissionMode) {
        self.shell_permission = permission;
    }

    pub fn set_write_permission(&mut self, permission: PermissionMode) {
        self.write_permission = permission;
    }

    pub async fn execute(&self, call: ToolCall) -> Result<String> {
        match call {
            ToolCall::ListFiles { path } => self.list_files(path.as_deref()),
            ToolCall::ReadFile { path } => self.read_file(&path),
            ToolCall::WriteFile { path, content } => self.write_file(&path, &content),
            ToolCall::RunShell { command } => self.run_shell(&command).await,
        }
    }

    pub fn list_files(&self, path: Option<&str>) -> Result<String> {
        let root = self.safe_path(path.unwrap_or("."))?;
        let mut entries = Vec::new();
        for entry in WalkDir::new(root)
            .max_depth(3)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path == self.workspace {
                continue;
            }
            let relative = path.strip_prefix(&self.workspace).unwrap_or(path);
            if entry.file_type().is_dir() {
                entries.push(format!("{}/", relative.display()));
            } else {
                entries.push(relative.display().to_string());
            }
            if entries.len() >= 200 {
                entries.push("... truncated at 200 entries".to_string());
                break;
            }
        }
        Ok(entries.join("\n"))
    }

    pub fn read_file(&self, path: &str) -> Result<String> {
        let path = self.safe_path(path)?;
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub fn write_file(&self, path: &str, content: &str) -> Result<String> {
        let path = self.safe_path(path)?;
        match approve(self.write_permission, &format!("write {}?", path.display()))? {
            Approval::Approved => {}
            Approval::Cancelled => return Ok("write cancelled by user".to_string()),
            Approval::Denied => return Ok("write denied by permission mode".to_string()),
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(format!("wrote {}", path.display()))
    }

    pub async fn run_shell(&self, command: &str) -> Result<String> {
        match approve(
            self.shell_permission,
            &format!("run shell command `{command}`?"),
        )? {
            Approval::Approved => {}
            Approval::Cancelled => return Ok("shell command cancelled by user".to_string()),
            Approval::Denied => return Ok("shell command denied by permission mode".to_string()),
        }

        let output = Command::new("sh")
            .arg("-lc")
            .arg(command)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("failed to run `{command}`"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status, stdout, stderr
        ))
    }

    fn safe_path(&self, path: &str) -> Result<PathBuf> {
        let requested = Path::new(path);
        if requested.is_absolute() {
            return Err(anyhow!("absolute paths are not allowed: {path}"));
        }
        let joined = self.workspace.join(requested);
        let parent = joined.parent().unwrap_or(&self.workspace);
        let canonical_parent = parent
            .canonicalize()
            .with_context(|| format!("path parent is not accessible: {}", parent.display()))?;
        if !canonical_parent.starts_with(&self.workspace) {
            return Err(anyhow!("path escapes workspace: {path}"));
        }
        Ok(joined)
    }
}

enum Approval {
    Approved,
    Cancelled,
    Denied,
}

fn approve(permission: PermissionMode, question: &str) -> Result<Approval> {
    match permission {
        PermissionMode::Allow => Ok(Approval::Approved),
        PermissionMode::Deny => Ok(Approval::Denied),
        PermissionMode::Ask => {
            if confirm(question)? {
                Ok(Approval::Approved)
            } else {
                Ok(Approval::Cancelled)
            }
        }
    }
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}
