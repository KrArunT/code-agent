use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Gemini,
    Ollama,
    Openrouter,
    CustomOpenai,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ThinkMode {
    Auto,
    On,
    Off,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Master,
    Worker,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "autofix")]
#[command(about = "Interactive coding agent with multi-provider support")]
pub struct Config {
    #[arg(long, env = "AGENT_CONFIG", default_value = "autofix_config.json")]
    pub config_file: PathBuf,

    #[arg(long, value_enum, env = "AGENT_ROLE", default_value = "master")]
    pub role: AgentRole,

    #[arg(long, value_enum, env = "AGENT_PROVIDER", default_value = "ollama")]
    pub provider: ProviderKind,

    #[arg(long, env = "AGENT_MODEL")]
    pub model: Option<String>,

    #[arg(long, env = "AGENT_BASE_URL")]
    pub base_url: Option<String>,

    #[arg(long, env = "AGENT_API_KEY")]
    pub api_key: Option<String>,

    #[arg(long, env = "AGENT_WORKSPACE", default_value = ".")]
    pub workspace: PathBuf,

    #[arg(long, env = "AGENT_TASK_FILE")]
    pub task_file: Option<PathBuf>,

    #[arg(long, env = "AGENT_WORKER_ID")]
    pub worker_id: Option<String>,

    #[arg(long, env = "AGENT_WORKER_NAME")]
    pub worker_name: Option<String>,

    #[arg(long, env = "AGENT_SESSION_ID")]
    pub session_id: Option<String>,

    #[arg(long, env = "AGENT_RESUME_SESSION")]
    pub resume_session: Option<String>,

    #[arg(long, env = "AGENT_MEMORY_FILE", default_value = "memory.json")]
    pub memory_file: PathBuf,

    #[arg(long, env = "AGENT_SKILLS_DIR", default_value = "skills")]
    pub skills_dir: PathBuf,

    #[arg(long, env = "AGENT_SYSTEM_PROMPT")]
    pub system: Option<String>,

    #[arg(long, env = "AGENT_ALLOW_SHELL")]
    pub dangerously_allow_shell: bool,

    #[arg(long, env = "AGENT_AUTO_WRITE")]
    pub auto_write: bool,

    #[arg(long, env = "AGENT_AUTO_WORKTREE")]
    pub auto_worktree: bool,

    #[arg(long, value_enum, env = "AGENT_APPROVAL_MODE", default_value = "ask")]
    pub approval_mode: PermissionMode,

    #[arg(long, value_enum, env = "AGENT_SHELL_APPROVAL")]
    pub shell_approval: Option<PermissionMode>,

    #[arg(long, value_enum, env = "AGENT_WRITE_APPROVAL")]
    pub write_approval: Option<PermissionMode>,

    #[arg(long, env = "AGENT_MAX_TOOL_ROUNDS", default_value_t = 6)]
    pub max_tool_rounds: usize,

    #[arg(long, env = "AGENT_AUTONOMOUS")]
    pub autonomous: bool,

    #[arg(long, value_enum, env = "AGENT_THINK", default_value = "auto")]
    pub think: ThinkMode,

    #[arg(long, env = "AGENT_HIDE_THINKING")]
    pub hide_thinking: bool,

    #[arg(long = "stop", env = "AGENT_STOP", value_delimiter = ',')]
    pub stop_sequences: Vec<String>,

    #[arg(long = "skill", env = "AGENT_SKILL", value_delimiter = ',')]
    pub active_skills: Vec<String>,

    #[arg(long, env = "AGENT_TUI")]
    pub tui: bool,

    #[arg(long, env = "AGENT_FULL_SYSTEM_ACCESS")]
    pub full_system_access: bool,

    #[arg(long, env = "AGENT_BANNER_TITLE", default_value = "AutoFix")]
    pub banner_title: String,

    #[arg(
        long,
        env = "AGENT_BANNER_SUBTITLE",
        default_value = "An autonomous coding agent"
    )]
    pub banner_subtitle: String,

    #[arg(
        long,
        env = "AGENT_BANNER_TIP",
        default_value = "start with a backport commit ID, target kernel version, or a local patch series"
    )]
    pub banner_tip: String,

    #[arg(long = "banner-onboarding", env = "AGENT_BANNER_ONBOARDING")]
    pub banner_onboarding: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    pub role: Option<AgentRole>,
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub workspace: Option<PathBuf>,
    pub task_file: Option<PathBuf>,
    pub worker_id: Option<String>,
    pub worker_name: Option<String>,
    pub session_id: Option<String>,
    pub memory_file: Option<PathBuf>,
    pub skills_dir: Option<PathBuf>,
    pub system: Option<String>,
    pub dangerously_allow_shell: Option<bool>,
    pub auto_write: Option<bool>,
    pub auto_worktree: Option<bool>,
    pub approval_mode: Option<PermissionMode>,
    pub shell_approval: Option<PermissionMode>,
    pub write_approval: Option<PermissionMode>,
    pub max_tool_rounds: Option<usize>,
    pub autonomous: Option<bool>,
    pub think: Option<ThinkMode>,
    pub hide_thinking: Option<bool>,
    pub stop_sequences: Option<Vec<String>>,
    pub active_skills: Option<Vec<String>>,
    pub tui: Option<bool>,
    pub full_system_access: Option<bool>,
    pub banner_title: Option<String>,
    pub banner_subtitle: Option<String>,
    pub banner_tip: Option<String>,
    pub banner_onboarding: Option<Vec<String>>,
}

impl Config {
    pub async fn resolve(mut self) -> Result<Self> {
        self.apply_config_file_if_present()?;
        self.finish_resolve().await
    }

    pub async fn reload_from_disk(&mut self) -> Result<()> {
        self.apply_config_file_if_present()?;
        let resolved = self.clone().finish_resolve().await?;
        *self = resolved;
        Ok(())
    }

    pub fn config_file_exists(&self) -> bool {
        self.config_file.exists() || self.legacy_config_file().exists()
    }

    pub fn load_config_file(&self) -> Result<Option<ConfigFile>> {
        let path = if self.config_file.exists() {
            self.config_file.clone()
        } else {
            let legacy = self.legacy_config_file();
            if legacy.exists() {
                legacy
            } else {
                return Ok(None);
            }
        };
        let path_display = path.display().to_string();
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path_display))?;
        let file = serde_json::from_str::<ConfigFile>(&text)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok(Some(file))
    }

    fn apply_config_file_if_present(&mut self) -> Result<()> {
        if let Some(file) = self.load_config_file()? {
            self.apply_config_file(file);
        }
        Ok(())
    }

    fn apply_config_file(&mut self, file: ConfigFile) {
        if let Some(value) = file.role {
            self.role = value;
        }
        if let Some(value) = file.provider {
            self.provider = value;
        }
        if let Some(value) = file.model {
            self.model = Some(value);
        }
        if let Some(value) = file.base_url {
            self.base_url = Some(value);
        }
        if let Some(value) = file.api_key {
            self.api_key = Some(value);
        }
        if let Some(value) = file.workspace {
            self.workspace = value;
        }
        if let Some(value) = file.task_file {
            self.task_file = Some(value);
        }
        if let Some(value) = file.worker_id {
            self.worker_id = Some(value);
        }
        if let Some(value) = file.worker_name {
            self.worker_name = Some(value);
        }
        if let Some(value) = file.session_id {
            self.session_id = Some(value);
        }
        if let Some(value) = file.memory_file {
            self.memory_file = value;
        }
        if let Some(value) = file.skills_dir {
            self.skills_dir = value;
        }
        if let Some(value) = file.system {
            self.system = Some(value);
        }
        if let Some(value) = file.dangerously_allow_shell {
            self.dangerously_allow_shell = value;
        }
        if let Some(value) = file.auto_write {
            self.auto_write = value;
        }
        if let Some(value) = file.auto_worktree {
            self.auto_worktree = value;
        }
        if let Some(value) = file.approval_mode {
            self.approval_mode = value;
        }
        if let Some(value) = file.shell_approval {
            self.shell_approval = Some(value);
        }
        if let Some(value) = file.write_approval {
            self.write_approval = Some(value);
        }
        if let Some(value) = file.max_tool_rounds {
            self.max_tool_rounds = value;
        }
        if let Some(value) = file.autonomous {
            self.autonomous = value;
        }
        if let Some(value) = file.think {
            self.think = value;
        }
        if let Some(value) = file.hide_thinking {
            self.hide_thinking = value;
        }
        if let Some(value) = file.stop_sequences {
            self.stop_sequences = value;
        }
        if let Some(value) = file.active_skills {
            self.active_skills = value;
        }
        if let Some(value) = file.tui {
            self.tui = value;
        }
        if let Some(value) = file.full_system_access {
            self.full_system_access = value;
        }
        if let Some(value) = file.banner_title {
            self.banner_title = value;
        }
        if let Some(value) = file.banner_subtitle {
            self.banner_subtitle = value;
        }
        if let Some(value) = file.banner_tip {
            self.banner_tip = value;
        }
        if let Some(value) = file.banner_onboarding {
            self.banner_onboarding = value;
        }
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().expect("model is resolved")
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().expect("base_url is resolved")
    }

    pub fn show_thinking(&self) -> bool {
        !self.hide_thinking
    }

    pub fn is_worker(&self) -> bool {
        matches!(self.role, AgentRole::Worker)
    }

    pub fn shell_permission(&self) -> PermissionMode {
        if self.full_system_access || self.dangerously_allow_shell {
            PermissionMode::Allow
        } else {
            self.shell_approval.unwrap_or(self.approval_mode)
        }
    }

    pub fn write_permission(&self) -> PermissionMode {
        if self.full_system_access || self.auto_write {
            PermissionMode::Allow
        } else {
            self.write_approval.unwrap_or(self.approval_mode)
        }
    }

    pub fn access_label(&self) -> &'static str {
        if self.full_system_access {
            "full-system"
        } else {
            "workspace"
        }
    }

    pub fn banner_onboarding(&self) -> Vec<String> {
        if self.banner_onboarding.is_empty() {
            vec![
                "/help commands".to_string(),
                "/models local models".to_string(),
                "/agents spawn task isolation".to_string(),
                "/permissions safety".to_string(),
                "/terminal real shell".to_string(),
                "/exit quit".to_string(),
            ]
        } else {
            self.banner_onboarding.clone()
        }
    }

    pub fn effective_max_tool_rounds(&self) -> usize {
        if self.autonomous {
            self.max_tool_rounds.max(50)
        } else {
            self.max_tool_rounds
        }
    }

    pub fn memory_file(&self) -> &PathBuf {
        &self.memory_file
    }

    pub fn skills_dir(&self) -> &PathBuf {
        &self.skills_dir
    }

    pub fn active_skills(&self) -> &[String] {
        &self.active_skills
    }

    pub fn set_active_skills(&mut self, skills: Vec<String>) {
        self.active_skills = skills;
    }

    pub fn snapshot_config_file(&self) -> ConfigFile {
        ConfigFile {
            role: Some(self.role),
            provider: Some(self.provider),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            workspace: Some(self.workspace.clone()),
            task_file: self.task_file.clone(),
            worker_id: self.worker_id.clone(),
            worker_name: self.worker_name.clone(),
            session_id: None,
            memory_file: Some(self.memory_file.clone()),
            skills_dir: Some(self.skills_dir.clone()),
            system: self.system.clone(),
            dangerously_allow_shell: Some(self.dangerously_allow_shell),
            auto_write: Some(self.auto_write),
            auto_worktree: Some(self.auto_worktree),
            approval_mode: Some(self.approval_mode),
            shell_approval: self.shell_approval,
            write_approval: self.write_approval,
            max_tool_rounds: Some(self.max_tool_rounds),
            autonomous: Some(self.autonomous),
            think: Some(self.think),
            hide_thinking: Some(self.hide_thinking),
            stop_sequences: Some(self.stop_sequences.clone()),
            active_skills: Some(self.active_skills.clone()),
            tui: Some(self.tui),
            full_system_access: Some(self.full_system_access),
            banner_title: Some(self.banner_title.clone()),
            banner_subtitle: Some(self.banner_subtitle.clone()),
            banner_tip: Some(self.banner_tip.clone()),
            banner_onboarding: Some(self.banner_onboarding.clone()),
        }
    }

    pub fn resolve_workspace_path(&self, path: &PathBuf) -> PathBuf {
        if path.is_absolute() {
            path.clone()
        } else {
            self.workspace.join(path)
        }
    }

    fn legacy_config_file(&self) -> PathBuf {
        self.config_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .join("config.json")
    }

    async fn finish_resolve(mut self) -> Result<Self> {
        self.workspace = self.workspace.canonicalize().map_err(|err| {
            anyhow!(
                "workspace '{}' is not accessible: {err}",
                self.workspace.display()
            )
        })?;
        self.memory_file = self.resolve_workspace_path(&self.memory_file);
        self.skills_dir = self.resolve_workspace_path(&self.skills_dir);
        if let Some(task_file) = self.task_file.clone() {
            if !task_file.is_absolute() {
                self.task_file = Some(self.resolve_workspace_path(&task_file));
            }
        }

        if self.is_worker() {
            self.tui = false;
            self.auto_worktree = false;
            self.autonomous = true;
            self.full_system_access = false;
            self.dangerously_allow_shell = false;
            self.auto_write = false;
            self.shell_approval = Some(PermissionMode::Allow);
            self.write_approval = Some(PermissionMode::Allow);
            if self.task_file.is_none() {
                return Err(anyhow!("worker mode requires --task-file"));
            }
        }

        if self.api_key.is_none() {
            self.api_key = match self.provider {
                ProviderKind::Openai => env::var("OPENAI_API_KEY").ok(),
                ProviderKind::Anthropic => env::var("ANTHROPIC_API_KEY").ok(),
                ProviderKind::Gemini => env::var("GEMINI_API_KEY").ok(),
                ProviderKind::Openrouter => env::var("OPENROUTER_API_KEY").ok(),
                ProviderKind::CustomOpenai => env::var("CUSTOM_API_KEY").ok(),
                ProviderKind::Ollama => None,
            };
        }

        if self.api_key.is_none() && !matches!(self.provider, ProviderKind::Ollama) {
            return Err(anyhow!(
                "missing API key; pass --api-key or set the provider-specific env var"
            ));
        }

        if self.base_url.is_none() {
            self.base_url = Some(
                match self.provider {
                    ProviderKind::Openai => "https://api.openai.com/v1/chat/completions",
                    ProviderKind::Anthropic => "https://api.anthropic.com/v1/messages",
                    ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
                    ProviderKind::Ollama => "http://localhost:11434/api/chat",
                    ProviderKind::Openrouter => "https://openrouter.ai/api/v1/chat/completions",
                    ProviderKind::CustomOpenai => {
                        return Err(anyhow!("--base-url is required for custom-openai"));
                    }
                }
                .to_string(),
            );
        }

        if self.model.is_none() {
            self.model = Some(match self.provider {
                ProviderKind::Openai => "gpt-4.1-mini".to_string(),
                ProviderKind::Anthropic => "claude-3-5-sonnet-latest".to_string(),
                ProviderKind::Gemini => "gemini-1.5-pro".to_string(),
                ProviderKind::Ollama => pick_ollama_model(self.base_url()).await?,
                ProviderKind::Openrouter => "openai/gpt-4.1-mini".to_string(),
                ProviderKind::CustomOpenai => {
                    return Err(anyhow!("--model is required for custom-openai"));
                }
            });
        }

        Ok(self)
    }
}

impl ThinkMode {
    pub fn as_request_value(self) -> Option<serde_json::Value> {
        match self {
            ThinkMode::Auto => None,
            ThinkMode::On => Some(serde_json::Value::Bool(true)),
            ThinkMode::Off => Some(serde_json::Value::Bool(false)),
            ThinkMode::Low => Some(serde_json::Value::String("low".to_string())),
            ThinkMode::Medium => Some(serde_json::Value::String("medium".to_string())),
            ThinkMode::High => Some(serde_json::Value::String("high".to_string())),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OllamaTags {
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: String,
}

async fn pick_ollama_model(base_url: &str) -> Result<String> {
    let models = list_ollama_models(base_url).await?;
    if models.is_empty() {
        return Err(anyhow!(
            "Ollama is running but has no local models. Pull one first, for example: ollama pull lfm"
        ));
    }

    let picked = models
        .iter()
        .find(|name| name.as_str() == "lfm")
        .or_else(|| models.iter().find(|name| name.starts_with("lfm:")))
        .or_else(|| models.iter().find(|name| name.contains("lfm")))
        .unwrap_or(&models[0]);

    Ok(picked.clone())
}

pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>> {
    let tags_url = ollama_tags_url(base_url);
    let tags: OllamaTags = reqwest::Client::new()
        .get(&tags_url)
        .send()
        .await
        .with_context(|| format!("failed to query local Ollama models at {tags_url}"))?
        .error_for_status()
        .with_context(|| format!("Ollama model list returned an error at {tags_url}"))?
        .json()
        .await
        .context("Ollama model list response was not JSON")?;

    let mut models = tags
        .models
        .into_iter()
        .map(|model| model.name)
        .collect::<Vec<_>>();
    models.sort();
    Ok(models)
}

fn ollama_tags_url(base_url: &str) -> String {
    if let Some(prefix) = base_url.strip_suffix("/api/chat") {
        format!("{prefix}/api/tags")
    } else {
        format!("{}/api/tags", base_url.trim_end_matches('/'))
    }
}
