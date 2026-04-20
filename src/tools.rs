use crate::config::PermissionMode;
use anyhow::{anyhow, Context, Result};
use reqwest::Url;
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
    ListFiles {
        path: Option<String>,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    RunShell {
        command: String,
    },
    WebSearch {
        query: String,
        max_results: Option<usize>,
    },
    SpawnWorker {
        name: Option<String>,
        task: String,
    },
    ListWorkers,
    ReadWorker {
        id: String,
    },
}

impl ToolCall {
    pub fn summary(&self) -> String {
        match self {
            ToolCall::ListFiles { path } => {
                format!("list_files path={}", path.as_deref().unwrap_or("."))
            }
            ToolCall::ReadFile { path } => format!("read_file path={path}"),
            ToolCall::WriteFile { path, content } => {
                format!("write_file path={path} bytes={}", content.len())
            }
            ToolCall::RunShell { command } => format!("run_shell command={command}"),
            ToolCall::WebSearch { query, max_results } => format!(
                "web_search query={} results={}",
                query,
                max_results.unwrap_or(5)
            ),
            ToolCall::SpawnWorker { name, task } => format!(
                "spawn_worker name={} task={}",
                name.as_deref().unwrap_or("worker"),
                task.lines().next().unwrap_or("").trim()
            ),
            ToolCall::ListWorkers => "list_workers".to_string(),
            ToolCall::ReadWorker { id } => format!("read_worker id={id}"),
        }
    }
}

pub struct ToolRuntime {
    workspace: PathBuf,
    shell_permission: PermissionMode,
    write_permission: PermissionMode,
    full_system_access: bool,
}

impl ToolRuntime {
    pub fn new(
        workspace: PathBuf,
        shell_permission: PermissionMode,
        write_permission: PermissionMode,
        full_system_access: bool,
    ) -> Self {
        Self {
            workspace,
            shell_permission,
            write_permission,
            full_system_access,
        }
    }

    pub fn shell_permission(&self) -> PermissionMode {
        self.shell_permission
    }

    pub fn write_permission(&self) -> PermissionMode {
        self.write_permission
    }

    pub fn full_system_access(&self) -> bool {
        self.full_system_access
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
            ToolCall::WebSearch { query, max_results } => {
                self.web_search(&query, max_results.unwrap_or(5)).await
            }
            ToolCall::SpawnWorker { .. } | ToolCall::ListWorkers | ToolCall::ReadWorker { .. } => {
                Err(anyhow!(
                    "agent orchestration tools must be handled by the master agent"
                ))
            }
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
        let path = self.resolve_path(path)?;
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub fn write_file(&self, path: &str, content: &str) -> Result<String> {
        let path = self.resolve_path(path)?;
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

    pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        self.safe_path(path)
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

    pub async fn run_git(&self, args: &[&str]) -> Result<String> {
        let command = format!("git {}", args.join(" "));
        match approve(
            self.shell_permission,
            &format!("run git command `{command}`?"),
        )? {
            Approval::Approved => {}
            Approval::Cancelled => return Ok("git command cancelled by user".to_string()),
            Approval::Denied => return Ok("git command denied by permission mode".to_string()),
        }

        let output = Command::new("git")
            .args(args)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("failed to run `{command}`"))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status, stdout, stderr
        ))
    }

    pub async fn web_search(&self, query: &str, max_results: usize) -> Result<String> {
        let max_results = max_results.clamp(1, 8);
        let url = Url::parse_with_params(
            "https://html.duckduckgo.com/html/",
            &[("q", query), ("kl", "us-en"), ("kp", "-1"), ("ia", "web")],
        )
        .context("failed to build DuckDuckGo search URL")?;

        let html = reqwest::Client::new()
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                "autofix/1.0 (+https://github.com/KrArunT/code-agent)",
            )
            .send()
            .await
            .context("DuckDuckGo search request failed")?
            .error_for_status()
            .context("DuckDuckGo returned an error")?
            .text()
            .await
            .context("DuckDuckGo response was not text")?;

        let results = parse_duckduckgo_results(&html, max_results);
        if results.is_empty() {
            return Ok(format!("no DuckDuckGo results found for `{query}`"));
        }

        let mut output = format!("DuckDuckGo results for `{query}`:\n");
        for (index, result) in results.iter().enumerate() {
            output.push_str(&format!("\n{}. {}\n", index + 1, result.title));
            output.push_str(&format!("   {}\n", result.url));
            if !result.snippet.trim().is_empty() {
                output.push_str(&format!("   {}\n", result.snippet));
            }
        }
        Ok(output.trim_end().to_string())
    }

    fn safe_path(&self, path: &str) -> Result<PathBuf> {
        let requested = Path::new(path);
        if requested.is_absolute() {
            if self.full_system_access {
                return Ok(requested.to_path_buf());
            }
            return Err(anyhow!(
                "absolute paths are not allowed without --full-system-access: {path}"
            ));
        }
        let joined = self.workspace.join(requested);
        let parent = joined.parent().unwrap_or(&self.workspace);
        let canonical_parent = parent
            .canonicalize()
            .with_context(|| format!("path parent is not accessible: {}", parent.display()))?;
        if !self.full_system_access && !canonical_parent.starts_with(&self.workspace) {
            return Err(anyhow!("path escapes workspace: {path}"));
        }
        Ok(joined)
    }
}

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut search_start = 0;

    while results.len() < max_results {
        let Some(marker) = html[search_start..].find("result__a") else {
            break;
        };
        let anchor_hint = search_start + marker;
        let Some(tag_start) = html[..anchor_hint].rfind("<a") else {
            search_start = anchor_hint + "result__a".len();
            continue;
        };
        let Some(tag_end_rel) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel;
        let open_tag = &html[tag_start..=tag_end];
        if !open_tag.contains("result__a") {
            search_start = tag_end + 1;
            continue;
        }

        let Some(raw_href) = extract_attr(open_tag, "href") else {
            search_start = tag_end + 1;
            continue;
        };
        let Some(title_end_rel) = html[tag_end + 1..].find("</a>") else {
            break;
        };
        let title_html = &html[tag_end + 1..tag_end + 1 + title_end_rel];
        let title = html_to_text(title_html);
        if title.is_empty() {
            search_start = tag_end + 1;
            continue;
        }

        let result_end = html[tag_end + 1 + title_end_rel..]
            .find("result__a")
            .map(|offset| tag_end + 1 + title_end_rel + offset)
            .unwrap_or(html.len());
        let block = &html[tag_end + 1 + title_end_rel..result_end];
        let snippet = find_snippet(block).unwrap_or_default();
        let url = normalize_duckduckgo_url(&raw_href);
        results.push(SearchResult {
            title,
            url,
            snippet,
        });
        search_start = tag_end + 1 + title_end_rel + 4;
    }

    results
}

fn find_snippet(block: &str) -> Option<String> {
    let marker = block.find("result__snippet")?;
    let tag_start = block[..marker].rfind('<')?;
    let tag_end = block[tag_start..].find('>')? + tag_start;
    let tag_name = block[tag_start + 1..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect::<String>();
    let closing = if tag_name.is_empty() {
        block[tag_end + 1..].find("</")
    } else {
        block[tag_end + 1..].find(&format!("</{tag_name}"))
    };
    let end = closing.unwrap_or(block.len() - tag_end - 1);
    let snippet = html_to_text(&block[tag_end + 1..tag_end + 1 + end]);
    if snippet.trim().is_empty() {
        None
    } else {
        Some(snippet)
    }
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!(r#"{attr}=""#);
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

fn normalize_duckduckgo_url(raw_href: &str) -> String {
    let href = if raw_href.starts_with("//") {
        format!("https:{raw_href}")
    } else {
        raw_href.to_string()
    };

    if let Ok(url) = Url::parse(&href) {
        if let Some((_, value)) = url.query_pairs().find(|(name, _)| name == "uddg") {
            return value.into_owned();
        }
    }

    href
}

fn html_to_text(input: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                let mut entity = String::from("&");
                while let Some(next) = chars.peek().copied() {
                    entity.push(next);
                    chars.next();
                    if next == ';' || entity.len() > 12 {
                        break;
                    }
                }
                output.push_str(match entity.as_str() {
                    "&amp;" => "&",
                    "&lt;" => "<",
                    "&gt;" => ">",
                    "&quot;" => "\"",
                    "&#39;" => "'",
                    "&#x27;" => "'",
                    _ => entity.as_str(),
                });
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }

    output.split_whitespace().collect::<Vec<_>>().join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_results_from_html() {
        let html = r#"
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fone">First &amp; Result</a>
<span class="result__snippet">Snippet <b>one</b>.</span>
<a class="result__a" href="https://example.com/two">Second Result</a>
<div class="result__snippet">Snippet two.</div>
"#;

        let results = parse_duckduckgo_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "First & Result");
        assert_eq!(results[0].url, "https://example.com/one");
        assert_eq!(results[0].snippet, "Snippet one.");
        assert_eq!(results[1].title, "Second Result");
        assert_eq!(results[1].url, "https://example.com/two");
        assert_eq!(results[1].snippet, "Snippet two.");
    }
}
