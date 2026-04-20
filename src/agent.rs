use crate::{
    completion::{prompt_text, AgentCompleter},
    config::{list_ollama_models, AgentRole, Config, PermissionMode, ProviderKind, ThinkMode},
    provider::{Message, ProviderClient, Role, StreamEvent},
    sessions::{
        list_session_records, load_session_record, make_session_id, save_session_record,
        session_history_summary, session_tail_summary, session_title_from_input, summarize_session,
        SessionRecord,
    },
    tools::{SearchResult, ToolCall, ToolRuntime},
    ui,
    workers::{
        list_worker_records, load_worker_record, make_worker_id, now_epoch,
        registry_root_for_workspace, save_worker_record, summarize_worker, task_excerpt,
        worker_log_path, worker_tail_summary, worker_task_path, WorkerRecord, WorkerStatus,
    },
};
use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Terminal,
};
use rustyline::{
    config::{CompletionType, EditMode},
    error::ReadlineError,
    history::DefaultHistory,
    Config as RustylineConfig, Editor,
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, Stdout},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Agent {
    config: Config,
    provider: ProviderClient,
    tools: ToolRuntime,
    messages: Vec<Message>,
    session: SessionRecord,
    shell_mode: bool,
    prompt_attachments: Vec<PromptAttachment>,
    search_picker: Option<SearchPicker>,
    memory_store: MemoryStore,
    skills: Vec<LoadedSkill>,
    completion_workspace: Arc<Mutex<PathBuf>>,
    last_config_mtime: Option<SystemTime>,
    progress: String,
    session_dirty: bool,
    session_interrupt: Arc<AtomicBool>,
    response_interrupted: bool,
}

const MAX_TUI_HISTORY: usize = 400;

impl Agent {
    pub fn new(config: Config) -> Result<Self> {
        let completion_workspace = config.workspace.clone();
        ensure_agent_doc(&config.workspace)?;
        let provider = ProviderClient::new(&config);
        let tools = ToolRuntime::new(
            config.workspace.clone(),
            config.shell_permission(),
            config.write_permission(),
            config.full_system_access,
        );
        let memory_store = load_memory_store(&config.memory_file)?;
        let skills = load_skills(&config.skills_dir, config.active_skills())?;
        let system = build_system_prompt(&config, &memory_store, &skills);
        let session = initialize_session(&config)?;
        let last_config_mtime = config.config_file_modified_time()?;
        let mut messages = vec![Message {
            role: Role::System,
            content: system,
        }];
        messages.extend(session.messages.clone());
        Ok(Self {
            config,
            provider,
            tools,
            messages,
            session,
            shell_mode: false,
            prompt_attachments: Vec::new(),
            search_picker: None,
            memory_store,
            skills,
            completion_workspace: Arc::new(Mutex::new(completion_workspace)),
            last_config_mtime,
            progress: String::new(),
            session_dirty: false,
            session_interrupt: Arc::new(AtomicBool::new(false)),
            response_interrupted: false,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        self.ensure_auto_worktree().await?;
        self.update_plan_file("startup").await?;
        self.persist_session_state()?;
        self.reset_interrupt_flag();
        ui::banner(
            &self.config.banner_title,
            &self.config.banner_subtitle,
            &format!("{:?}", self.config.provider),
            self.provider.model(),
            &self.config.workspace.display().to_string(),
            self.config.access_label(),
            &self.config.banner_onboarding(),
            &self.config.banner_tip,
        );

        let editor_config = RustylineConfig::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .build();
        let mut editor = Editor::<AgentCompleter, DefaultHistory>::with_config(editor_config)?;
        let mut completer = AgentCompleter::new(self.config.workspace.clone());
        if let Ok(workspace) = self.completion_workspace.lock() {
            completer.set_workspace(workspace.clone());
        }
        editor.set_helper(Some(completer));
        self.load_editor_history(&mut editor)?;

        loop {
            self.maybe_reload_config_if_changed().await?;
            if let Some(helper) = editor.helper_mut() {
                helper.set_shell_mode(self.shell_mode);
            }
            let input = match editor.readline(&prompt_text(self.shell_mode)) {
                Ok(input) => input,
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(ReadlineError::Eof) => break,
                Err(err) => return Err(err.into()),
            };
            if input.trim().is_empty() {
                continue;
            }
            self.record_session_input(&input)?;
            let _ = editor.add_history_entry(input.as_str());
            if input.starts_with('/') || input.starts_with('!') {
                if self.handle_command(&input).await? {
                    break;
                }
                if self.session_dirty {
                    self.load_editor_history(&mut editor)?;
                    self.session_dirty = false;
                }
                self.update_plan_file("command").await?;
                continue;
            }
            if self.shell_mode {
                self.run_shell_command(&input).await?;
                continue;
            }

            if let Some(summary) = self.attachment_status_text() {
                ui::info(&summary);
            }
            self.update_session_title_from_prompt(&input);
            let user_prompt = self.compose_user_prompt(&input);
            self.messages.push(Message {
                role: Role::User,
                content: user_prompt,
            });
            self.sync_session_messages()?;
            self.reset_response_interrupted();
            self.respond().await?;
            self.sync_session_messages()?;
            if self.response_interrupted {
                self.update_plan_file("interrupted").await?;
            } else {
                self.update_plan_file("response").await?;
            }
            self.clear_progress();
        }
        Ok(())
    }

    pub fn is_tui_enabled(&self) -> bool {
        self.config.tui
    }

    pub fn is_worker_mode(&self) -> bool {
        self.config.is_worker()
    }

    pub async fn run_worker(&mut self) -> Result<()> {
        let task_file = self
            .config
            .task_file
            .clone()
            .ok_or_else(|| anyhow!("worker mode requires --task-file"))?;
        let task = fs::read_to_string(&task_file)
            .with_context(|| format!("failed to read worker task {}", task_file.display()))?;
        let worker_name = self
            .config
            .worker_name
            .clone()
            .unwrap_or_else(|| "worker".to_string());
        let worker_id = self
            .config
            .worker_id
            .clone()
            .unwrap_or_else(|| "worker".to_string());

        self.set_progress(format!("worker {worker_id}: loading task"));

        let mut record = load_worker_record(&self.config.workspace, &worker_id)?;
        record.status = WorkerStatus::Running;
        record.updated_at = now_epoch();
        save_worker_record(&self.config.workspace, &record)?;

        ui::banner(
            &format!("{} worker", self.config.banner_title),
            &format!("{} - {}", self.config.banner_subtitle, worker_name),
            &format!("{:?}", self.config.provider),
            self.provider.model(),
            &self.config.workspace.display().to_string(),
            self.config.access_label(),
            &self.config.banner_onboarding(),
            &format!("task: {}", task_excerpt(&task)),
        );

        self.update_plan_file("startup").await?;
        self.persist_session_state()?;
        self.messages.push(Message {
            role: Role::User,
            content: task,
        });
        self.sync_session_messages()?;

        let result = self.respond().await;
        self.sync_session_messages()?;
        let mut finished = load_worker_record(&self.config.workspace, &worker_id)?;
        finished.updated_at = now_epoch();
        match &result {
            Ok(_) => {
                finished.status = WorkerStatus::Finished;
                finished.exit_status = Some(0);
                self.set_progress(format!("worker {worker_id}: finished"));
            }
            Err(err) => {
                finished.status = WorkerStatus::Failed;
                finished.exit_status = Some(1);
                finished.task = format!("{}\n\nworker error: {err}", finished.task);
                self.set_progress(format!("worker {worker_id}: failed"));
            }
        }
        save_worker_record(&self.config.workspace, &finished)?;
        result
    }

    pub async fn run_tui(&mut self) -> Result<()> {
        self.ensure_auto_worktree().await?;
        self.update_plan_file("startup").await?;
        self.persist_session_state()?;
        self.reset_interrupt_flag();
        let mut terminal = TuiGuard::enter()?;
        let banner_onboarding = self.config.banner_onboarding();
        let mut transcript = vec![TranscriptItem::new(
            "system",
            onboarding_text(self.config.full_system_access, &banner_onboarding),
        )];
        let mut input = String::new();
        let mut status = "ready".to_string();
        let mut needs_draw = true;
        let mut show_help = false;
        let mut scroll_offset: usize = 0;
        let mut input_history: Vec<String> = self.session.command_history.clone();
        let mut history_index: Option<usize> = None;

        loop {
            if self.maybe_reload_config_if_changed().await? {
                needs_draw = true;
            }
            let size = terminal.inner().size()?;
            let layout = tui_layout(size);
            let transcript_len = transcript_content_len(&transcript);
            let viewport_len = transcript_viewport_height(layout.transcript);
            scroll_offset = clamp_scroll_offset(scroll_offset, transcript_len, viewport_len);
            if needs_draw {
                draw_tui(
                    terminal.inner(),
                    self,
                    &transcript,
                    &input,
                    &status,
                    show_help,
                    scroll_offset,
                    self.search_picker.as_ref(),
                )?;
                needs_draw = false;
            }

            if !event::poll(Duration::from_millis(100))? {
                continue;
            }

            match event::read()? {
                Event::Mouse(mouse) => {
                    if self.search_picker.is_none() {
                        if handle_mouse_event(mouse, size, &transcript, &mut scroll_offset)? {
                            needs_draw = true;
                        }
                    }
                }
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    if self.search_picker.is_some() {
                        let mut close_picker = false;
                        match key.code {
                            KeyCode::Esc => {
                                close_picker = true;
                                status = "ready".to_string();
                            }
                            KeyCode::Enter => {
                                if let Some(picker) = self.search_picker.as_ref() {
                                    if let Some(result) = picker.selected_result() {
                                        input = result.url.clone();
                                        transcript.push(TranscriptItem::new(
                                            "system",
                                            format!(
                                                "selected search result:\n{}\n{}\n{}",
                                                result.title, result.url, result.snippet
                                            ),
                                        ));
                                        status = "search selected".to_string();
                                        needs_draw = true;
                                    }
                                }
                                close_picker = true;
                            }
                            KeyCode::Up => {
                                if let Some(picker) = self.search_picker.as_mut() {
                                    picker.selected = picker.selected.saturating_sub(1);
                                }
                                needs_draw = true;
                            }
                            KeyCode::Down => {
                                if let Some(picker) = self.search_picker.as_mut() {
                                    if picker.selected + 1 < picker.results.len() {
                                        picker.selected += 1;
                                    }
                                }
                                needs_draw = true;
                            }
                            KeyCode::Home => {
                                if let Some(picker) = self.search_picker.as_mut() {
                                    picker.selected = 0;
                                }
                                needs_draw = true;
                            }
                            KeyCode::End => {
                                if let Some(picker) = self.search_picker.as_mut() {
                                    if let Some(last) = picker.results.len().checked_sub(1) {
                                        picker.selected = last;
                                    }
                                }
                                needs_draw = true;
                            }
                            _ => {}
                        }
                        if close_picker {
                            self.search_picker = None;
                            needs_draw = true;
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break;
                        }
                        KeyCode::Esc => break,
                        KeyCode::Char('?') => {
                            show_help = !show_help;
                            needs_draw = true;
                        }
                        KeyCode::PageUp => {
                            scroll_offset = scroll_offset.saturating_add(8);
                            needs_draw = true;
                        }
                        KeyCode::PageDown => {
                            scroll_offset = scroll_offset.saturating_sub(8);
                            needs_draw = true;
                        }
                        KeyCode::Up if input.is_empty() => {
                            if let Some(next) =
                                previous_history_index(history_index, input_history.len())
                            {
                                history_index = Some(next);
                                input = input_history[next].clone();
                                needs_draw = true;
                            }
                        }
                        KeyCode::Down if input.is_empty() => {
                            if let Some(next) =
                                next_history_index(history_index, input_history.len())
                            {
                                history_index = Some(next);
                                input = input_history[next].clone();
                                needs_draw = true;
                            } else {
                                history_index = None;
                                input.clear();
                                needs_draw = true;
                            }
                        }
                        KeyCode::Home => {
                            scroll_offset = usize::MAX;
                            needs_draw = true;
                        }
                        KeyCode::End => {
                            scroll_offset = 0;
                            needs_draw = true;
                        }
                        KeyCode::Char(ch) => {
                            input.push(ch);
                            history_index = None;
                            needs_draw = true;
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            history_index = None;
                            needs_draw = true;
                        }
                        KeyCode::Enter => {
                            let submitted = input.trim_end().to_string();
                            input.clear();
                            history_index = None;
                            needs_draw = true;
                            if submitted.trim().is_empty() {
                                continue;
                            }

                            self.record_session_input(&submitted)?;
                            input_history.push(submitted.clone());
                            if input_history.len() > 200 {
                                let excess = input_history.len() - 200;
                                input_history.drain(0..excess);
                            }

                            if submitted.starts_with('/') {
                                if self
                                    .handle_tui_command(&submitted, &mut transcript, &mut status)
                                    .await?
                                {
                                    break;
                                }
                                if self.session_dirty {
                                    input_history = self.session.command_history.clone();
                                    history_index = None;
                                    transcript.clear();
                                    transcript.push(TranscriptItem::new(
                                        "system",
                                        format!(
                                            "session resumed: {}",
                                            summarize_session(&self.session)
                                        ),
                                    ));
                                    self.session_dirty = false;
                                }
                                self.update_plan_file("command").await?;
                                needs_draw = true;
                                continue;
                            }

                            transcript.push(TranscriptItem::new("user", submitted.clone()));
                            if let Some(summary) = self.attachment_status_text() {
                                transcript.push(TranscriptItem::new("system", summary));
                            }
                            trim_transcript(&mut transcript);
                            self.update_session_title_from_prompt(&submitted);
                            let user_prompt = self.compose_user_prompt(&submitted);
                            self.messages.push(Message {
                                role: Role::User,
                                content: user_prompt,
                            });
                            self.sync_session_messages()?;
                            self.reset_response_interrupted();

                            status = "streaming".to_string();
                            transcript.push(TranscriptItem::new("assistant", String::new()));
                            trim_transcript(&mut transcript);
                            let assistant_index = transcript.len() - 1;
                            draw_tui(
                                terminal.inner(),
                                self,
                                &transcript,
                                &input,
                                &status,
                                show_help,
                                scroll_offset,
                                self.search_picker.as_ref(),
                            )?;

                            self.reset_interrupt_flag();
                            let interrupt_watcher = self.spawn_interrupt_watcher();
                            let mut inline_thinking = false;
                            let mut visible_answer = String::new();
                            let show_thinking = self.config.show_thinking()
                                && !matches!(self.provider.think(), ThinkMode::Off);
                            let answer = self
                                .provider
                                .complete_stream(
                                    &self.messages,
                                    self.session_interrupt.clone(),
                                    |event| {
                                        match event {
                                            StreamEvent::Content(delta) => {
                                                visible_answer.push_str(&filter_tui_content_delta(
                                                    delta,
                                                    show_thinking,
                                                    &mut inline_thinking,
                                                ));
                                            }
                                            StreamEvent::Thinking(delta) => {
                                                if show_thinking {
                                                    visible_answer.push_str(delta);
                                                }
                                            }
                                        }
                                        transcript[assistant_index].content =
                                            visible_answer.clone();
                                        draw_tui(
                                            terminal.inner(),
                                            self,
                                            &transcript,
                                            &input,
                                            &status,
                                            show_help,
                                            scroll_offset,
                                            self.search_picker.as_ref(),
                                        )?;
                                        Ok(())
                                    },
                                )
                                .await;
                            interrupt_watcher.abort();
                            let answer = match answer {
                                Ok(answer) => answer,
                                Err(err) if err.to_string().contains("interrupted by user") => {
                                    transcript[assistant_index].content = visible_answer;
                                    transcript.push(TranscriptItem::new(
                                        "system",
                                        "session interrupted by the user before completion",
                                    ));
                                    self.messages.push(Message {
                                        role: Role::System,
                                        content: "Session interrupted by the user before completion. Preserve the current state and continue from the saved session when resumed.".to_string(),
                                    });
                                    self.sync_session_messages()?;
                                    status = "interrupted".to_string();
                                    self.response_interrupted = true;
                                    self.update_plan_file("interrupted").await?;
                                    self.clear_progress();
                                    needs_draw = true;
                                    continue;
                                }
                                Err(err) => return Err(err),
                            };
                            let answer = strip_think_blocks(&answer);
                            transcript[assistant_index].content = answer.clone();
                            self.messages.push(Message {
                                role: Role::Assistant,
                                content: answer,
                            });
                            self.sync_session_messages()?;
                            status = "ready".to_string();
                            self.update_plan_file("response").await?;
                            self.clear_progress();
                            needs_draw = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_command(&mut self, input: &str) -> Result<bool> {
        let mut parts = input.splitn(2, ' ');
        let command = parts.next().unwrap_or_default();
        let arg = parts.next().unwrap_or("").trim();

        match command {
            "/exit" | "/quit" => Ok(true),
            "/chat" | "/exit-shell" => {
                self.shell_mode = false;
                ui::info("chat mode enabled");
                Ok(false)
            }
            "/config" => {
                ui::info(&self.handle_config_command(arg).await?);
                Ok(false)
            }
            "/memory" => {
                ui::info(&self.handle_memory_command(arg).await?);
                Ok(false)
            }
            "/skills" => {
                ui::info(&self.handle_skills_command(arg).await?);
                Ok(false)
            }
            "/worktree" => {
                ui::info(&self.handle_worktree_command(arg).await?);
                Ok(false)
            }
            "/agents" => {
                ui::info(&self.handle_agents_command(arg).await?);
                Ok(false)
            }
            "/session" => {
                ui::info(&self.handle_session_command(arg).await?);
                Ok(false)
            }
            "/history" => {
                ui::info(&session_history_summary(&self.session));
                Ok(false)
            }
            "/attach" => {
                if let Some(summary) = self.handle_attach_command(arg)? {
                    ui::info(&summary);
                }
                Ok(false)
            }
            "/search" => {
                ui::render_markdown(&self.handle_search_command(arg).await?);
                Ok(false)
            }
            "/help" => {
                println!("{}", ui::help_text());
                Ok(false)
            }
            "/provider" => {
                ui::info(&format!(
                    "role={:?} provider={:?} model={} base_url={} think={:?} show_thinking={} stops={} permissions=shell:{:?},write:{:?} access={} session={}",
                    self.config.role,
                    self.config.provider,
                    self.provider.model(),
                    self.config.base_url(),
                    self.provider.think(),
                    self.config.show_thinking(),
                    format_stop_sequences(self.provider.stop_sequences()),
                    self.tools.shell_permission(),
                    self.tools.write_permission(),
                    self.config.access_label(),
                    self.session.id
                ));
                Ok(false)
            }
            "/permissions" => {
                self.handle_permissions(arg);
                Ok(false)
            }
            "/thinking" => {
                self.handle_thinking(arg);
                Ok(false)
            }
            "/hide-thinking" => {
                self.config.hide_thinking = true;
                ui::info("thinking trace hidden in the TUI");
                Ok(false)
            }
            "/show-thinking" => {
                self.config.hide_thinking = false;
                ui::info("thinking trace visible in the TUI");
                Ok(false)
            }
            "/stop" => {
                self.handle_stop_sequences(arg);
                Ok(false)
            }
            "/interrupt" => {
                self.request_interrupt();
                ui::info("interrupt requested");
                Ok(false)
            }
            "/models" => {
                if !matches!(self.config.provider, ProviderKind::Ollama) {
                    ui::error("/models is available for the Ollama provider");
                    return Ok(false);
                }
                ui::divider();
                for model in list_ollama_models(self.config.base_url()).await? {
                    let marker = if model == self.provider.model() {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {model}");
                }
                ui::divider();
                Ok(false)
            }
            "/use-model" => {
                if arg.is_empty() {
                    ui::error("usage: /use-model <model-name>");
                    return Ok(false);
                }
                if matches!(self.config.provider, ProviderKind::Ollama) {
                    let models = list_ollama_models(self.config.base_url()).await?;
                    if !models.iter().any(|model| model == arg) {
                        ui::error(&format!(
                            "model '{arg}' is not installed locally. Use /models to see choices."
                        ));
                        return Ok(false);
                    }
                }
                self.provider.set_model(arg.to_string());
                self.config.model = Some(arg.to_string());
                ui::info(&format!("model set to {arg}"));
                Ok(false)
            }
            "/list" => {
                println!(
                    "{}",
                    self.tools
                        .list_files(if arg.is_empty() { None } else { Some(arg) })?
                );
                Ok(false)
            }
            "/read" => {
                println!("{}", self.tools.read_file(arg)?);
                Ok(false)
            }
            "/write" => {
                println!("Enter content. Finish with a single '.' on its own line.");
                let mut content = String::new();
                loop {
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    if line.trim_end() == "." {
                        break;
                    }
                    content.push_str(&line);
                }
                println!("{}", self.tools.write_file(arg, &content)?);
                Ok(false)
            }
            "/shell" => {
                if arg.is_empty() {
                    self.shell_mode = true;
                    ui::info("shell mode enabled; use /chat or /exit-shell to return");
                } else {
                    self.run_shell_command(arg).await?;
                }
                Ok(false)
            }
            "/terminal" => {
                self.run_terminal(arg)?;
                Ok(false)
            }
            command if command.starts_with('!') => {
                let inline_command = input.trim_start_matches('!').trim();
                self.run_shell_command(inline_command).await?;
                Ok(false)
            }
            "/clear" => {
                ui::clear_screen()?;
                ui::banner(
                    &self.config.banner_title,
                    &self.config.banner_subtitle,
                    &format!("{:?}", self.config.provider),
                    self.provider.model(),
                    &self.config.workspace.display().to_string(),
                    self.config.access_label(),
                    &self.config.banner_onboarding(),
                    &self.config.banner_tip,
                );
                Ok(false)
            }
            _ => {
                ui::error(&format!("unknown command: {command}"));
                Ok(false)
            }
        }
    }

    async fn handle_tui_command(
        &mut self,
        input: &str,
        transcript: &mut Vec<TranscriptItem>,
        status: &mut String,
    ) -> Result<bool> {
        let mut parts = input.splitn(2, ' ');
        let command = parts.next().unwrap_or_default();
        let arg = parts.next().unwrap_or("").trim();

        match command {
            "/exit" | "/quit" => Ok(true),
            "/clear" => {
                transcript.clear();
                *status = "ready".to_string();
                Ok(false)
            }
            "/help" => {
                transcript.push(TranscriptItem::new("system", tui_help_text()));
                Ok(false)
            }
            "/config" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_config_command(arg).await?,
                ));
                Ok(false)
            }
            "/memory" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_memory_command(arg).await?,
                ));
                Ok(false)
            }
            "/skills" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_skills_command(arg).await?,
                ));
                Ok(false)
            }
            "/worktree" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_worktree_command(arg).await?,
                ));
                Ok(false)
            }
            "/agents" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_agents_command(arg).await?,
                ));
                Ok(false)
            }
            "/session" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    self.handle_session_command(arg).await?,
                ));
                Ok(false)
            }
            "/history" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    session_history_summary(&self.session),
                ));
                Ok(false)
            }
            "/attach" => {
                if let Some(summary) = self.handle_attach_command(arg)? {
                    transcript.push(TranscriptItem::new("system", summary));
                }
                Ok(false)
            }
            "/search" => {
                if let Some(summary) = self.handle_search_picker_command(arg).await? {
                    transcript.push(TranscriptItem::new("system", summary));
                } else if let Some(picker) = &self.search_picker {
                    transcript.push(TranscriptItem::new(
                        "system",
                        format!(
                            "search results for `{}` loaded: use ↑/↓ and Enter to pick",
                            picker.query
                        ),
                    ));
                }
                Ok(false)
            }
            "/provider" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    format!(
                        "role={:?}\nprovider={:?}\nmodel={}\nbase_url={}\nthink={:?}\npermissions=shell:{:?},write:{:?}\naccess={}\nsession={}",
                        self.config.role,
                        self.config.provider,
                        self.provider.model(),
                        self.config.base_url(),
                        self.provider.think(),
                        self.tools.shell_permission(),
                        self.tools.write_permission(),
                        self.config.access_label(),
                        self.session.id
                    ),
                ));
                Ok(false)
            }
            "/models" => {
                if !matches!(self.config.provider, ProviderKind::Ollama) {
                    transcript.push(TranscriptItem::new(
                        "error",
                        "/models is available for the Ollama provider",
                    ));
                    return Ok(false);
                }
                let models = list_ollama_models(self.config.base_url()).await?;
                let body = models
                    .into_iter()
                    .map(|model| {
                        if model == self.provider.model() {
                            format!("* {model}")
                        } else {
                            format!("  {model}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                transcript.push(TranscriptItem::new("system", body));
                Ok(false)
            }
            "/use-model" => {
                if arg.is_empty() {
                    transcript.push(TranscriptItem::new("error", "usage: /use-model <model>"));
                    return Ok(false);
                }
                if matches!(self.config.provider, ProviderKind::Ollama) {
                    let models = list_ollama_models(self.config.base_url()).await?;
                    if !models.iter().any(|model| model == arg) {
                        transcript.push(TranscriptItem::new(
                            "error",
                            format!("model '{arg}' is not installed locally"),
                        ));
                        return Ok(false);
                    }
                }
                self.provider.set_model(arg.to_string());
                self.config.model = Some(arg.to_string());
                transcript.push(TranscriptItem::new("system", format!("model set to {arg}")));
                Ok(false)
            }
            "/thinking" => {
                if arg.is_empty() {
                    transcript.push(TranscriptItem::new(
                        "system",
                        format!(
                            "think={:?}\nshow_thinking={}",
                            self.provider.think(),
                            self.config.show_thinking()
                        ),
                    ));
                    return Ok(false);
                }
                match arg {
                    "show" => {
                        self.config.hide_thinking = false;
                        transcript.push(TranscriptItem::new("system", "thinking trace visible"));
                    }
                    "hide" => {
                        self.config.hide_thinking = true;
                        transcript.push(TranscriptItem::new("system", "thinking trace hidden"));
                    }
                    _ => match parse_think_mode(arg) {
                        Some(mode) => {
                            self.provider.set_think(mode);
                            self.config.think = mode;
                            transcript
                                .push(TranscriptItem::new("system", format!("think={mode:?}")));
                        }
                        None => transcript.push(TranscriptItem::new(
                            "error",
                            "usage: /thinking [auto|on|off|low|medium|high|show|hide]",
                        )),
                    },
                }
                Ok(false)
            }
            "/interrupt" => {
                self.request_interrupt();
                transcript.push(TranscriptItem::new("system", "interrupt requested"));
                Ok(false)
            }
            _ => {
                transcript.push(TranscriptItem::new(
                    "error",
                    format!("unsupported TUI command: {command}. Use /help."),
                ));
                Ok(false)
            }
        }
    }

    async fn run_shell_command(&self, command: &str) -> Result<()> {
        if command.trim().is_empty() {
            ui::error("no shell command provided");
            return Ok(());
        }
        ui::tool_start(&format!("run_shell command={command}"));
        let result = self.tools.run_shell(command).await?;
        ui::tool_result("run_shell", &result);
        Ok(())
    }

    fn run_terminal(&self, arg: &str) -> Result<()> {
        let shell = if arg.is_empty() {
            env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        } else {
            arg.to_string()
        };

        ui::info(&format!(
            "opening terminal shell `{shell}` in {}; exit the shell to return",
            self.config.workspace.display()
        ));

        let status = Command::new(&shell)
            .current_dir(&self.config.workspace)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("failed to launch terminal shell `{shell}`"))?;

        ui::info(&format!("terminal exited with {status}"));
        Ok(())
    }

    fn handle_attach_command(&mut self, arg: &str) -> Result<Option<String>> {
        let mut parts = arg.splitn(2, ' ');
        let kind = parts.next().unwrap_or_default().trim();
        let value = parts.next().unwrap_or("").trim();

        match kind {
            "clear" => {
                self.prompt_attachments.clear();
                Ok(Some("attachments cleared".to_string()))
            }
            "show" => Ok(Some(
                self.attachment_status_text()
                    .unwrap_or_else(|| "attachments: none".to_string()),
            )),
            "file" => {
                if value.is_empty() {
                    ui::error("usage: /attach file <path>");
                    return Ok(None);
                }
                let path = self.tools.resolve_path(value)?;
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                self.prompt_attachments.push(PromptAttachment::File {
                    path: path.display().to_string(),
                    content,
                });
                Ok(Some(format!("queued file attachment: {}", path.display())))
            }
            "image" => {
                if value.is_empty() {
                    ui::error("usage: /attach image <path>");
                    return Ok(None);
                }
                let path = self.tools.resolve_path(value)?;
                let size = fs::metadata(&path)
                    .with_context(|| format!("failed to inspect {}", path.display()))?
                    .len();
                self.prompt_attachments.push(PromptAttachment::Image {
                    path: path.display().to_string(),
                    size_bytes: size,
                });
                Ok(Some(format!(
                    "queued image attachment: {} ({} bytes)",
                    path.display(),
                    size
                )))
            }
            _ => {
                ui::error("usage: /attach [show|clear|file <path>|image <path>]");
                Ok(None)
            }
        }
    }

    async fn handle_search_picker_command(&mut self, arg: &str) -> Result<Option<String>> {
        if arg.trim().is_empty() {
            ui::error("usage: /search <query>");
            return Ok(Some("usage: /search <query>".to_string()));
        }

        let results = self.tools.web_search_results(arg, 5).await?;
        if results.is_empty() {
            return Ok(Some(format!("no DuckDuckGo results found for `{arg}`")));
        }

        self.search_picker = Some(SearchPicker::new(arg.to_string(), results));
        Ok(None)
    }

    async fn handle_search_command(&mut self, arg: &str) -> Result<String> {
        if arg.trim().is_empty() {
            ui::error("usage: /search <query>");
            return Ok("usage: /search <query>".to_string());
        }

        self.tools.web_search(arg, 5).await
    }

    async fn handle_config_command(&mut self, arg: &str) -> Result<String> {
        match arg {
            "" | "show" => Ok(format!(
                "config file: {}\nloaded: {}\nrole={:?}\nprovider={:?}\nmodel={}\nworkspace={}\nsession_id={}\nresume_session={}\ntask_file={}\nworker_id={}\nworker_name={}\nautonomous={}\nauto_worktree={}\nmax_tool_rounds={}\nmemory_file={}\nskills_dir={}\nactive_skills={}",
                self.config.config_file.display(),
                if self.config.config_file_exists() {
                    "yes"
                } else {
                    "no"
                },
                self.config.role,
                self.config.provider,
                self.provider.model(),
                self.config.workspace.display(),
                self.config
                    .session_id
                    .as_deref()
                    .unwrap_or(self.session.id.as_str()),
                self.config
                    .resume_session
                    .as_deref()
                    .unwrap_or("none"),
                self.config
                    .task_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "none".to_string()),
                self.config.worker_id.as_deref().unwrap_or("none"),
                self.config.worker_name.as_deref().unwrap_or("none"),
                self.config.autonomous,
                self.config.auto_worktree,
                self.config.effective_max_tool_rounds(),
                self.config.memory_file().display(),
                self.config.skills_dir().display(),
                self.config.active_skills().join(", ")
            )),
            "reload" => {
                self.reload_from_config_file().await?;
                Ok(format!(
                    "reloaded config from {}",
                    self.config.config_file.display()
                ))
            }
            _ => Ok("usage: /config [show|reload]".to_string()),
        }
    }

    async fn reload_from_config_file(&mut self) -> Result<()> {
        self.config.reload_from_disk().await?;
        self.memory_store = load_memory_store(self.config.memory_file())?;
        self.skills = load_skills(self.config.skills_dir(), self.config.active_skills())?;
        self.reload_context_layers()?;
        self.refresh_config_stamp()?;
        Ok(())
    }

    async fn handle_memory_command(&mut self, arg: &str) -> Result<String> {
        let arg = arg.trim();
        match arg {
            "" | "show" => Ok(self.memory_summary()),
            "clear" => {
                self.memory_store.notes.clear();
                save_memory_store(self.config.memory_file(), &self.memory_store)?;
                self.reload_context_layers()?;
                Ok("memory cleared".to_string())
            }
            "reload" => {
                self.memory_store = load_memory_store(self.config.memory_file())?;
                self.reload_context_layers()?;
                Ok(format!(
                    "reloaded memory from {}",
                    self.config.memory_file().display()
                ))
            }
            _ if arg.starts_with("add ") => {
                let note = arg[4..].trim();
                if note.is_empty() {
                    return Ok("usage: /memory add <text>".to_string());
                }
                self.memory_store.notes.push(note.to_string());
                save_memory_store(self.config.memory_file(), &self.memory_store)?;
                self.reload_context_layers()?;
                Ok(format!("added memory note: {note}"))
            }
            _ => Ok("usage: /memory [show|add <text>|clear|reload]".to_string()),
        }
    }

    async fn handle_skills_command(&mut self, arg: &str) -> Result<String> {
        let arg = arg.trim();
        match arg {
            "" | "show" => Ok(self.skills_summary()),
            "list" => Ok(self.available_skills_summary()),
            "reload" => {
                self.skills = load_skills(self.config.skills_dir(), self.config.active_skills())?;
                self.reload_context_layers()?;
                Ok(format!(
                    "reloaded skills from {}",
                    self.config.skills_dir().display()
                ))
            }
            _ if arg.starts_with("enable ") => {
                let name = arg[7..].trim();
                if name.is_empty() {
                    return Ok("usage: /skills enable <name>".to_string());
                }
                if !self
                    .config
                    .active_skills()
                    .iter()
                    .any(|skill| skill == name)
                {
                    let mut active = self.config.active_skills().to_vec();
                    active.push(name.to_string());
                    self.config.set_active_skills(active);
                    self.persist_config_file()?;
                    self.skills =
                        load_skills(self.config.skills_dir(), self.config.active_skills())?;
                    self.reload_context_layers()?;
                }
                Ok(format!("enabled skill: {name}"))
            }
            _ if arg.starts_with("disable ") => {
                let name = arg[8..].trim();
                if name.is_empty() {
                    return Ok("usage: /skills disable <name>".to_string());
                }
                let mut active = self.config.active_skills().to_vec();
                active.retain(|skill| skill != name);
                self.config.set_active_skills(active);
                self.persist_config_file()?;
                self.skills = load_skills(self.config.skills_dir(), self.config.active_skills())?;
                self.reload_context_layers()?;
                Ok(format!("disabled skill: {name}"))
            }
            _ => Ok("usage: /skills [show|list|reload|enable <name>|disable <name>]".to_string()),
        }
    }

    async fn handle_worktree_command(&mut self, arg: &str) -> Result<String> {
        let mut parts = arg.split_whitespace();
        let subcommand = parts.next().unwrap_or_default();

        match subcommand {
            "" | "status" | "list" => self.tools.run_git(&["worktree", "list", "--porcelain"]).await,
            "prune" => self.tools.run_git(&["worktree", "prune"]).await,
            "auto" => {
                self.ensure_auto_worktree().await?;
                Ok(format!("auto worktree check complete: {}", self.config.workspace.display()))
            }
            "add" | "create" => {
                let path = parts.next().unwrap_or_default();
                if path.is_empty() {
                    return Ok("usage: /worktree add <path> [branch]".to_string());
                }
                let branch = parts.next();
                let mut args = vec!["worktree", "add"];
                if let Some(branch) = branch {
                    args.push("-b");
                    args.push(branch);
                }
                args.push(path);
                let result = self.tools.run_git(&args).await?;
                let new_workspace = self.resolve_worktree_path(path);
                self.switch_workspace(new_workspace)?;
                Ok(format!(
                    "{result}\n\nworkspace switched to {}",
                    self.config.workspace.display()
                ))
            }
            "use" | "switch" => {
                let path = parts.next().unwrap_or_default();
                if path.is_empty() {
                    return Ok("usage: /worktree switch <path>".to_string());
                }
                let new_workspace = self.resolve_worktree_path(path);
                self.switch_workspace(new_workspace)?;
                Ok(format!("workspace switched to {}", self.config.workspace.display()))
            }
            "remove" | "rm" => {
                let path = parts.next().unwrap_or_default();
                if path.is_empty() {
                    return Ok("usage: /worktree remove <path>".to_string());
                }
                let target = self.resolve_worktree_path(path);
                if self.same_path(&target, &self.config.workspace) {
                    return Ok("refusing to remove the active workspace".to_string());
                }
                self.tools
                    .run_git(&["worktree", "remove", target.to_string_lossy().as_ref()])
                    .await
            }
            _ => Ok(
                "usage: /worktree [status|list|auto|add <path> [branch]|switch <path>|remove <path>|prune]"
                    .to_string(),
            ),
        }
    }

    async fn handle_agents_command(&mut self, arg: &str) -> Result<String> {
        let mut parts = arg.split_whitespace();
        let subcommand = parts.next().unwrap_or_default();

        match subcommand {
            "" | "list" => self.list_workers_summary(),
            "spawn" => {
                let spec = parts.collect::<Vec<_>>();
                if spec.is_empty() {
                    return Ok("usage: /agents spawn <name> | <task>".to_string());
                }
                let joined = spec.join(" ");
                let (name, task) = if let Some((name, task)) = joined.split_once('|') {
                    let name = name.trim();
                    let task = task.trim();
                    (
                        if name.is_empty() {
                            None
                        } else {
                            Some(name.to_string())
                        },
                        task.to_string(),
                    )
                } else {
                    (None, joined)
                };
                if task.is_empty() {
                    return Ok("usage: /agents spawn <name> | <task>".to_string());
                }
                self.spawn_worker(name, task).await
            }
            "read" => {
                let id = parts.next().unwrap_or_default();
                if id.is_empty() {
                    return Ok("usage: /agents read <id>".to_string());
                }
                self.read_worker_summary(id)
            }
            _ => Ok("usage: /agents [list|spawn <name> | <task>|read <id>]".to_string()),
        }
    }

    async fn handle_session_command(&mut self, arg: &str) -> Result<String> {
        let mut parts = arg.split_whitespace();
        let subcommand = parts.next().unwrap_or_default();

        match subcommand {
            "" | "show" => Ok(session_tail_summary(&self.session)),
            "list" => {
                let records = list_session_records(&self.config.workspace)?;
                if records.is_empty() {
                    Ok("sessions: none".to_string())
                } else {
                    Ok(format!(
                        "sessions:\n{}",
                        records
                            .iter()
                            .map(summarize_session)
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "history" => Ok(session_history_summary(&self.session)),
            "save" => {
                self.persist_session_state()?;
                Ok(format!("session saved: {}", self.session.id))
            }
            "new" => {
                self.start_new_session()?;
                Ok(format!("started new session: {}", self.session.id))
            }
            "resume" => {
                let id = parts.next().unwrap_or_default();
                if id.is_empty() {
                    return Ok("usage: /session resume <id>".to_string());
                }
                self.resume_session(id)?;
                Ok(format!("resumed session {} from {}", self.session.id, id))
            }
            _ => Ok("usage: /session [show|list|history|save|new|resume <id>]".to_string()),
        }
    }

    async fn ensure_auto_worktree(&mut self) -> Result<()> {
        if !self.config.auto_worktree {
            return Ok(());
        }
        if !self.is_git_repo()? {
            return Ok(());
        }
        if self.is_linked_worktree()? {
            return Ok(());
        }

        let new_workspace = self.create_auto_worktree_path()?;
        let branch = self.auto_worktree_branch_name()?;
        let worktree_path = new_workspace.to_string_lossy().to_string();
        let args = [
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            worktree_path.as_str(),
            "HEAD",
        ];
        self.tools.run_git(&args).await?;
        self.switch_workspace(new_workspace)?;
        Ok(())
    }

    async fn spawn_worker(&mut self, name: Option<String>, task: String) -> Result<String> {
        let worker_name = name.unwrap_or_else(|| "worker".to_string());
        let worker_id = make_worker_id(&worker_name);
        self.set_progress(format!("spawning worker {worker_id}"));
        let new_workspace = self.create_worker_worktree_path(&worker_id)?;
        let branch = self.worker_branch_name(&worker_id)?;
        let worktree_path = new_workspace.to_string_lossy().to_string();
        let args = [
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            worktree_path.as_str(),
            "HEAD",
        ];
        self.tools.run_git(&args).await?;

        sync_workspace_context(&self.config.workspace, &new_workspace)?;

        let worker_task = format!("# Worker Task: {worker_name}\n\n{}\n", task.trim());
        let task_file = worker_task_path(&self.config.workspace, &worker_id)?;
        if let Some(parent) = task_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&task_file, worker_task)
            .with_context(|| format!("failed to write task file {}", task_file.display()))?;

        let config_file = new_workspace.join("autofix_config.json");
        let worker_config = worker_config_snapshot(
            &self.config,
            &new_workspace,
            &task_file,
            &worker_id,
            &worker_name,
        );
        write_config_snapshot(&config_file, &worker_config)?;

        let log_file = worker_log_path(&self.config.workspace, &worker_id)?;
        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let log_handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .with_context(|| format!("failed to open worker log {}", log_file.display()))?;
        let child_log = log_handle
            .try_clone()
            .context("failed to clone worker log handle")?;

        let control_root = registry_root_for_workspace(&self.config.workspace)?;
        fs::create_dir_all(control_root.join("workers"))
            .with_context(|| format!("failed to create {}", control_root.display()))?;
        let mut record = WorkerRecord {
            id: worker_id.clone(),
            name: worker_name.clone(),
            task: task.clone(),
            workspace: new_workspace.clone(),
            branch: branch.clone(),
            config_file: config_file.clone(),
            task_file: task_file.clone(),
            log_file: log_file.clone(),
            pid: None,
            status: WorkerStatus::Starting,
            created_at: now_epoch(),
            updated_at: now_epoch(),
            exit_status: None,
        };
        save_worker_record(&self.config.workspace, &record)?;

        let exe = env::current_exe().context("failed to resolve current executable")?;
        let child = Command::new(exe)
            .arg("--role")
            .arg("worker")
            .arg("--config-file")
            .arg(&config_file)
            .arg("--task-file")
            .arg(&task_file)
            .arg("--worker-id")
            .arg(&worker_id)
            .arg("--worker-name")
            .arg(&worker_name)
            .current_dir(&new_workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_handle))
            .stderr(Stdio::from(child_log))
            .spawn()
            .with_context(|| format!("failed to spawn worker {}", worker_id))?;

        record.pid = Some(child.id());
        record.status = WorkerStatus::Running;
        record.updated_at = now_epoch();
        save_worker_record(&self.config.workspace, &record)?;
        self.set_progress(format!("worker {worker_id} running"));

        Ok(format!(
            "spawned worker {worker_id}\nbranch: {branch}\nworkspace: {}\npid: {}\ntask: {}",
            new_workspace.display(),
            child.id(),
            task_excerpt(&task)
        ))
    }

    fn list_workers_summary(&self) -> Result<String> {
        let records = list_worker_records(&self.config.workspace)?;
        if records.is_empty() {
            return Ok("workers: none".to_string());
        }
        Ok(format!(
            "workers:\n{}",
            records
                .iter()
                .map(summarize_worker)
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    fn read_worker_summary(&self, id: &str) -> Result<String> {
        let record = load_worker_record(&self.config.workspace, id)?;
        Ok(worker_tail_summary(&record))
    }

    fn reload_context_layers(&mut self) -> Result<()> {
        ensure_agent_doc(&self.config.workspace)?;
        let system = build_system_prompt(&self.config, &self.memory_store, &self.skills);
        self.provider = ProviderClient::new(&self.config);
        self.tools = ToolRuntime::new(
            self.config.workspace.clone(),
            self.config.shell_permission(),
            self.config.write_permission(),
            self.config.full_system_access,
        );
        if let Some(first) = self.messages.first_mut() {
            first.role = Role::System;
            first.content = system;
        } else {
            self.messages.insert(
                0,
                Message {
                    role: Role::System,
                    content: system,
                },
            );
        }
        Ok(())
    }

    fn record_session_input(&mut self, input: &str) -> Result<()> {
        self.session.command_history.push(input.to_string());
        self.session.touch();
        self.persist_session_state()
    }

    fn update_session_title_from_prompt(&mut self, input: &str) {
        if self.session.title.is_none() && !input.trim().is_empty() {
            self.session.title = Some(session_title_from_input(input));
        }
    }

    fn sync_session_messages(&mut self) -> Result<()> {
        self.session.messages = self.messages.iter().skip(1).cloned().collect();
        self.session.touch();
        self.persist_session_state()
    }

    fn persist_session_state(&mut self) -> Result<()> {
        save_session_record(&self.config.workspace, &self.session)
    }

    fn reset_interrupt_flag(&self) {
        self.session_interrupt.store(false, Ordering::SeqCst);
    }

    fn reset_response_interrupted(&mut self) {
        self.response_interrupted = false;
    }

    fn request_interrupt(&self) {
        self.session_interrupt.store(true, Ordering::SeqCst);
    }

    fn spawn_interrupt_watcher(&self) -> tokio::task::JoinHandle<()> {
        let interrupt = self.session_interrupt.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            interrupt.store(true, Ordering::SeqCst);
        })
    }

    fn load_editor_history(
        &self,
        editor: &mut Editor<AgentCompleter, DefaultHistory>,
    ) -> Result<()> {
        editor.clear_history()?;
        for command in &self.session.command_history {
            let _ = editor.add_history_entry(command.as_str());
        }
        Ok(())
    }

    fn start_new_session(&mut self) -> Result<()> {
        self.session = SessionRecord::new(
            self.config.workspace.clone(),
            self.config.role,
            make_session_id(self.config.role),
        );
        self.messages.truncate(1);
        self.shell_mode = false;
        self.prompt_attachments.clear();
        self.clear_progress();
        self.persist_session_state()?;
        self.session_dirty = true;
        Ok(())
    }

    fn resume_session(&mut self, id: &str) -> Result<()> {
        let source = load_session_record(&self.config.workspace, id)?;
        self.session = SessionRecord::resume_from(
            &source,
            self.config.workspace.clone(),
            self.config.role,
            make_session_id(self.config.role),
        );
        self.messages.truncate(1);
        self.messages.extend(self.session.messages.clone());
        self.shell_mode = false;
        self.prompt_attachments.clear();
        self.clear_progress();
        self.persist_session_state()?;
        self.session_dirty = true;
        Ok(())
    }

    fn switch_workspace(&mut self, new_workspace: PathBuf) -> Result<()> {
        let old_workspace = self.config.workspace.clone();
        if self.same_path(&old_workspace, &new_workspace) {
            return Ok(());
        }
        sync_workspace_context(&old_workspace, &new_workspace)?;
        self.config.workspace = new_workspace.clone();
        self.config.config_file =
            relocate_under_workspace(&self.config.config_file, &old_workspace, &new_workspace);
        self.config.memory_file =
            relocate_under_workspace(&self.config.memory_file, &old_workspace, &new_workspace);
        self.config.skills_dir =
            relocate_under_workspace(&self.config.skills_dir, &old_workspace, &new_workspace);
        self.memory_store = load_memory_store(&self.config.memory_file)?;
        self.skills = load_skills(&self.config.skills_dir, self.config.active_skills())?;
        self.reload_context_layers()?;
        self.session.workspace = self.config.workspace.clone();
        self.session.touch();
        self.persist_session_state()?;
        if let Ok(mut workspace) = self.completion_workspace.lock() {
            *workspace = self.config.workspace.clone();
        }
        self.persist_config_file()?;
        Ok(())
    }

    fn resolve_worktree_path(&self, path: &str) -> PathBuf {
        let requested = Path::new(path);
        if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.config.workspace.join(requested)
        }
    }

    fn same_path(&self, left: &Path, right: &Path) -> bool {
        if left == right {
            return true;
        }
        match (left.canonicalize(), right.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    fn is_git_repo(&self) -> Result<bool> {
        Ok(self
            .git_output(&["rev-parse", "--is-inside-work-tree"])?
            .map(|text| text.trim() == "true")
            .unwrap_or(false))
    }

    fn is_linked_worktree(&self) -> Result<bool> {
        Ok(self
            .git_output(&["rev-parse", "--git-dir"])?
            .map(|text| text.trim() != ".git")
            .unwrap_or(false))
    }

    fn create_auto_worktree_path(&self) -> Result<PathBuf> {
        let root = self
            .git_output(&["rev-parse", "--show-toplevel"])?
            .ok_or_else(|| anyhow!("cannot determine git root"))?;
        let root = PathBuf::from(root.trim());
        let repo_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo");
        let branch = self.auto_worktree_branch_name()?;
        let worktrees_root = root.parent().unwrap_or(&root).join("worktrees");
        fs::create_dir_all(&worktrees_root)
            .with_context(|| format!("failed to create {}", worktrees_root.display()))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let candidate = worktrees_root.join(format!(
            "{}-{}-{}-{}",
            repo_name,
            sanitize_name(&branch),
            timestamp,
            pid
        ));
        Ok(candidate)
    }

    fn create_worker_worktree_path(&self, worker_id: &str) -> Result<PathBuf> {
        let root = self
            .git_output(&["rev-parse", "--show-toplevel"])?
            .ok_or_else(|| anyhow!("cannot determine git root"))?;
        let root = PathBuf::from(root.trim());
        let repo_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo");
        let worktrees_root = root.parent().unwrap_or(&root).join("worktrees");
        fs::create_dir_all(&worktrees_root)
            .with_context(|| format!("failed to create {}", worktrees_root.display()))?;
        let candidate = worktrees_root.join(format!(
            "{}-{}-{}-{}",
            repo_name,
            sanitize_name(worker_id),
            now_epoch(),
            std::process::id()
        ));
        Ok(candidate)
    }

    fn auto_worktree_branch_name(&self) -> Result<String> {
        let branch = self
            .git_output(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .unwrap_or_else(|| "feature".to_string());
        let branch = branch.trim();
        let short_sha = self
            .git_output(&["rev-parse", "--short", "HEAD"])?
            .unwrap_or_else(|| "head".to_string());
        Ok(format!(
            "autofix/{}/{}",
            sanitize_name(branch),
            short_sha.trim()
        ))
    }

    fn worker_branch_name(&self, worker_id: &str) -> Result<String> {
        let branch = self
            .git_output(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .unwrap_or_else(|| "feature".to_string());
        Ok(format!(
            "autofix/{}/{}",
            sanitize_name(branch.trim()),
            sanitize_name(worker_id)
        ))
    }

    fn git_output(&self, args: &[&str]) -> Result<Option<String>> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.config.workspace)
            .stdin(Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
            }
            Ok(_) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn persist_config_file(&mut self) -> Result<()> {
        let file = self.config.snapshot_config_file();
        write_config_snapshot(&self.config.config_file, &file)?;
        self.refresh_config_stamp()?;
        Ok(())
    }

    fn refresh_config_stamp(&mut self) -> Result<()> {
        self.last_config_mtime = self.config.config_file_modified_time()?;
        Ok(())
    }

    async fn maybe_reload_config_if_changed(&mut self) -> Result<bool> {
        let current = match self.config.config_file_modified_time() {
            Ok(current) => current,
            Err(err) => {
                ui::error(&format!("failed to inspect config file for reload: {err}"));
                return Ok(false);
            }
        };
        if current == self.last_config_mtime {
            return Ok(false);
        }

        self.last_config_mtime = current;
        if current.is_none() {
            return Ok(false);
        }

        let previous_workspace = self.config.workspace.clone();
        if let Err(err) = self.reload_from_config_file().await {
            ui::error(&format!(
                "auto-reload failed for {}: {err}",
                self.config.config_file.display()
            ));
            return Ok(false);
        }
        if self.config.workspace != previous_workspace {
            self.session.workspace = self.config.workspace.clone();
            self.session.touch();
            self.persist_session_state()?;
            let _ = self.completion_workspace.lock().map(|mut workspace| {
                *workspace = self.config.workspace.clone();
            });
        }
        self.set_progress(format!(
            "reloaded config from {}",
            self.config.config_file.display()
        ));
        Ok(true)
    }

    async fn update_plan_file(&mut self, stage: &str) -> Result<()> {
        let plan_path = self.config.workspace.join("PLAN.md");
        let text = self.render_plan(stage)?;
        fs::write(&plan_path, text)
            .with_context(|| format!("failed to write plan file {}", plan_path.display()))?;
        self.refresh_system_prompt()?;
        Ok(())
    }

    fn render_plan(&self, stage: &str) -> Result<String> {
        let branch = self
            .git_output(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .unwrap_or_else(|| "unknown".to_string());
        let status = self.git_output(&["status", "--short"])?.unwrap_or_default();
        let changed_files = parse_git_status(&status);
        let changed_block = if changed_files.is_empty() {
            "- none".to_string()
        } else {
            changed_files
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let next_steps = if changed_files.is_empty() {
            vec![
                "Continue the current user task.".to_string(),
                "Keep PLAN.md refreshed after each meaningful step.".to_string(),
            ]
        } else {
            vec![
                "Review the git diff for the listed files.".to_string(),
                "Run a targeted validation pass before reporting complete.".to_string(),
            ]
        };

        let next_steps_block = next_steps
            .iter()
            .map(|step| format!("- {step}"))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!(
            "# PLAN\n\n## Summary\n\n- Stage: {stage}\n- Workspace: {}\n- Branch: {}\n- Provider: {:?}\n- Model: {}\n- Access: {}\n- Autonomous: {}\n- Auto worktree: {}\n- Active skills: {}\n- Memory notes: {}\n\n## Files Changed\n\n{}\n\n## Next Steps\n\n{}\n",
            self.config.workspace.display(),
            branch.trim(),
            self.config.provider,
            self.provider.model(),
            self.config.access_label(),
            self.config.autonomous,
            self.config.auto_worktree,
            if self.config.active_skills().is_empty() {
                "none".to_string()
            } else {
                self.config.active_skills().join(", ")
            },
            self.memory_store.notes.len(),
            changed_block,
            next_steps_block,
        ))
    }

    fn refresh_system_prompt(&mut self) -> Result<()> {
        let system = build_system_prompt(&self.config, &self.memory_store, &self.skills);
        if let Some(first) = self.messages.first_mut() {
            first.role = Role::System;
            first.content = system;
        } else {
            self.messages.insert(
                0,
                Message {
                    role: Role::System,
                    content: system,
                },
            );
        }
        Ok(())
    }

    fn memory_summary(&self) -> String {
        if self.memory_store.notes.is_empty() {
            "memory: none".to_string()
        } else {
            format!(
                "memory notes:\n{}",
                self.memory_store
                    .notes
                    .iter()
                    .enumerate()
                    .map(|(idx, note)| format!("{}: {}", idx + 1, note))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }

    fn skills_summary(&self) -> String {
        if self.skills.is_empty() {
            return "skills: none active".to_string();
        }

        let active = self
            .skills
            .iter()
            .map(|skill| format!("{} ({})", skill.name, skill.path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!("active skills:\n{active}")
    }

    fn available_skills_summary(&self) -> String {
        let mut names = Vec::new();
        if let Ok(entries) = fs::read_dir(self.config.skills_dir()) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                if path.is_file()
                    && (path.extension().and_then(|ext| ext.to_str()) == Some("md")
                        || path.extension().and_then(|ext| ext.to_str()) == Some("txt"))
                {
                    names.push(name.to_string());
                } else if path.is_dir() && path.join("SKILL.md").exists() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        if names.is_empty() {
            "available skills: none found".to_string()
        } else {
            format!("available skills:\n{}", names.join("\n"))
        }
    }

    fn compose_user_prompt(&mut self, input: &str) -> String {
        let attachments = std::mem::take(&mut self.prompt_attachments);
        compose_prompt_with_attachments(input, &attachments)
    }

    fn attachment_status_text(&self) -> Option<String> {
        if self.prompt_attachments.is_empty() {
            None
        } else {
            Some(format!(
                "pending prompt attachments: {}",
                self.prompt_attachments
                    .iter()
                    .map(PromptAttachment::summary)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }

    fn set_progress(&mut self, message: impl Into<String>) {
        self.progress = message.into();
        if !self.config.tui {
            ui::info(&format!("progress: {}", self.progress));
        }
    }

    fn clear_progress(&mut self) {
        self.progress.clear();
    }

    async fn respond(&mut self) -> Result<()> {
        let total_rounds = self.config.effective_max_tool_rounds();
        let mut warned_near_limit = false;
        for round in 0..=total_rounds {
            self.maybe_reload_config_if_changed().await?;
            if !warned_near_limit && round >= total_rounds.saturating_sub(1) {
                self.messages.push(Message {
                    role: Role::System,
                    content: "You are close to the tool budget limit. Prefer one of these outcomes now: return a final JSON turn, return a blocked JSON turn, or delegate remaining work to a worker. Use the Codex-style turn protocol and do not keep making exploratory tool calls unless they are essential.".to_string(),
                });
                warned_near_limit = true;
            }
            self.set_progress(format!(
                "round {}/{}: thinking",
                round + 1,
                total_rounds + 1
            ));
            self.reset_interrupt_flag();
            let interrupt_watcher = self.spawn_interrupt_watcher();
            ui::assistant_start()?;
            let mut showed_thinking = false;
            let mut showed_answer = false;
            let mut inline_thinking = false;
            let mut markdown = ui::MarkdownStream::new();
            let show_thinking =
                self.config.show_thinking() && !matches!(self.provider.think(), ThinkMode::Off);
            let answer = self
                .provider
                .complete_stream(&self.messages, self.session_interrupt.clone(), |event| {
                    match event {
                        StreamEvent::Content(delta) => {
                            stream_content_delta(
                                delta,
                                show_thinking,
                                &mut inline_thinking,
                                &mut showed_thinking,
                                &mut showed_answer,
                                &mut markdown,
                            )?;
                        }
                        StreamEvent::Thinking(delta) => {
                            if show_thinking {
                                if !showed_thinking {
                                    ui::thinking_start()?;
                                    showed_thinking = true;
                                }
                                ui::stream_thinking(delta)?;
                            }
                        }
                    }
                    Ok(())
                })
                .await;
            interrupt_watcher.abort();
            let answer = match answer {
                Ok(answer) => answer,
                Err(err) if err.to_string().contains("interrupted by user") => {
                    self.set_progress("interrupted");
                    ui::error("session interrupted");
                    self.messages.push(Message {
                        role: Role::System,
                        content: "Session interrupted by the user before completion. Preserve the current state and continue from the saved session when resumed.".to_string(),
                    });
                    self.sync_session_messages()?;
                    self.response_interrupted = true;
                    let _ = self.update_plan_file("interrupted").await;
                    self.clear_progress();
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            markdown.finish()?;
            if showed_thinking && !showed_answer {
                ui::stream_reset()?;
            }
            println!();
            let answer = strip_think_blocks(&answer);
            self.messages.push(Message {
                role: Role::Assistant,
                content: answer.clone(),
            });

            match parse_agent_turn(&answer)? {
                Some(AgentTurn::Final { summary }) => {
                    if !summary.trim().is_empty() {
                        self.messages.push(Message {
                            role: Role::User,
                            content: format!("Final summary:\n{summary}"),
                        });
                    }
                    self.set_progress("complete");
                    return Ok(());
                }
                Some(AgentTurn::Blocked { reason }) => {
                    if !reason.trim().is_empty() {
                        self.messages.push(Message {
                            role: Role::User,
                            content: format!("Blocked:\n{reason}"),
                        });
                    }
                    self.set_progress("blocked");
                    return Ok(());
                }
                Some(AgentTurn::NeedsWorker { task }) => {
                    let result = self.spawn_worker(None, task).await?;
                    ui::info(&result);
                    self.messages.push(Message {
                        role: Role::User,
                        content: format!("Worker spawned:\n{result}"),
                    });
                    continue;
                }
                Some(AgentTurn::ToolCalls { calls: tool_calls }) => {
                    let tool_total = tool_calls.len();
                    let mut result_lines = Vec::new();
                    for (index, tool_call) in tool_calls.into_iter().enumerate() {
                        let tool_label = tool_call.summary();
                        self.set_progress(format!(
                            "round {}/{}: executing {} ({}/{})",
                            round + 1,
                            total_rounds + 1,
                            tool_label,
                            index + 1,
                            tool_total
                        ));
                        ui::tool_start(&tool_label);
                        let result = match self.execute_tool_call(tool_call).await {
                            Ok(result) => result,
                            Err(err) => format!("tool execution error: {err}"),
                        };
                        ui::tool_result(&tool_label, &result);
                        result_lines.push(format!("{}: {result}", index + 1));
                    }
                    let result = if result_lines.len() == 1 {
                        result_lines.remove(0)
                    } else {
                        format!("Tool results:\n{}", result_lines.join("\n"))
                    };
                    self.messages.push(Message {
                        role: Role::User,
                        content: format!("Tool result:\n{result}"),
                    });
                    continue;
                }
                None => {
                    let tool_calls = match extract_tool_calls(&answer) {
                        Ok(tool_calls) if tool_calls.is_empty() => {
                            self.set_progress("complete");
                            return Ok(());
                        }
                        Ok(tool_calls) => tool_calls,
                        Err(err) => {
                            let message = format!(
                                "tool call parse error: {err}\nReturn a single JSON turn object like {{\"type\":\"tool_calls\",\"calls\":[...]}} or {{\"type\":\"final\",\"summary\":\"...\"}}."
                            );
                            ui::error(&message);
                            self.messages.push(Message {
                                role: Role::User,
                                content: message,
                            });
                            continue;
                        }
                    };

                    let tool_total = tool_calls.len();
                    let mut result_lines = Vec::new();
                    for (index, tool_call) in tool_calls.into_iter().enumerate() {
                        let tool_label = tool_call.summary();
                        self.set_progress(format!(
                            "round {}/{}: executing {} ({}/{})",
                            round + 1,
                            total_rounds + 1,
                            tool_label,
                            index + 1,
                            tool_total
                        ));
                        ui::tool_start(&tool_label);
                        let result = match self.execute_tool_call(tool_call).await {
                            Ok(result) => result,
                            Err(err) => format!("tool execution error: {err}"),
                        };
                        ui::tool_result(&tool_label, &result);
                        result_lines.push(format!("{}: {result}", index + 1));
                    }
                    let result = if result_lines.len() == 1 {
                        result_lines.remove(0)
                    } else {
                        format!("Tool results:\n{}", result_lines.join("\n"))
                    };
                    self.messages.push(Message {
                        role: Role::User,
                        content: format!("Tool result:\n{result}"),
                    });
                }
            }
        }

        self.set_progress("stopped after max tool rounds");
        ui::error("stopped after max tool rounds");
        Ok(())
    }

    async fn execute_tool_call(&mut self, call: ToolCall) -> Result<String> {
        match call {
            ToolCall::SpawnWorker { name, task } => self.spawn_worker(name, task).await,
            ToolCall::ListWorkers => self.list_workers_summary(),
            ToolCall::ReadWorker { id } => self.read_worker_summary(&id),
            other => self.tools.execute(other).await,
        }
    }

    fn handle_thinking(&mut self, arg: &str) {
        if arg.is_empty() {
            ui::info(&format!(
                "think={:?} show_thinking={}",
                self.provider.think(),
                self.config.show_thinking()
            ));
            return;
        }

        match arg {
            "show" => {
                self.config.hide_thinking = false;
                ui::info("thinking trace visible in the TUI");
            }
            "hide" => {
                self.config.hide_thinking = true;
                ui::info("thinking trace hidden in the TUI");
            }
            _ => match parse_think_mode(arg) {
                Some(mode) => {
                    self.provider.set_think(mode);
                    self.config.think = mode;
                    ui::info(&format!("think set to {mode:?}"));
                }
                None => ui::error("usage: /thinking [auto|on|off|low|medium|high|show|hide]"),
            },
        }
    }

    fn handle_permissions(&mut self, arg: &str) {
        if arg.is_empty() {
            ui::info(&format!(
                "permissions shell={:?} write={:?}",
                self.tools.shell_permission(),
                self.tools.write_permission()
            ));
            return;
        }

        let parts = arg.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            [mode] => match parse_permission_mode(mode) {
                Some(permission) => {
                    self.tools.set_shell_permission(permission);
                    self.tools.set_write_permission(permission);
                    ui::info(&format!("shell and write permissions set to {permission:?}"));
                }
                None => ui::error("usage: /permissions [ask|allow|deny]"),
            },
            ["shell", mode] => match parse_permission_mode(mode) {
                Some(permission) => {
                    self.tools.set_shell_permission(permission);
                    ui::info(&format!("shell permission set to {permission:?}"));
                }
                None => ui::error("usage: /permissions shell [ask|allow|deny]"),
            },
            ["write", mode] => match parse_permission_mode(mode) {
                Some(permission) => {
                    self.tools.set_write_permission(permission);
                    ui::info(&format!("write permission set to {permission:?}"));
                }
                None => ui::error("usage: /permissions write [ask|allow|deny]"),
            },
            _ => ui::error(
                "usage: /permissions, /permissions [ask|allow|deny], /permissions shell <mode>, or /permissions write <mode>",
            ),
        }
    }

    fn handle_stop_sequences(&mut self, arg: &str) {
        if arg.is_empty() {
            ui::info(&format!(
                "stop sequences: {}",
                format_stop_sequences(self.provider.stop_sequences())
            ));
            return;
        }

        if arg == "clear" {
            self.provider.set_stop_sequences(Vec::new());
            self.config.stop_sequences.clear();
            ui::info("stop sequences cleared");
            return;
        }

        let Some((command, value)) = arg.split_once(' ') else {
            ui::error("usage: /stop, /stop clear, /stop add <text>, or /stop set <a,b,c>");
            return;
        };

        match command {
            "add" => {
                let mut stops = self.provider.stop_sequences().to_vec();
                stops.push(value.to_string());
                self.provider.set_stop_sequences(stops.clone());
                self.config.stop_sequences = stops;
                ui::info(&format!(
                    "stop sequences: {}",
                    format_stop_sequences(self.provider.stop_sequences())
                ));
            }
            "set" => {
                let stops = value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                self.provider.set_stop_sequences(stops.clone());
                self.config.stop_sequences = stops;
                ui::info(&format!(
                    "stop sequences: {}",
                    format_stop_sequences(self.provider.stop_sequences())
                ));
            }
            _ => ui::error("usage: /stop, /stop clear, /stop add <text>, or /stop set <a,b,c>"),
        }
    }
}

fn stream_content_delta(
    delta: &str,
    show_thinking: bool,
    inline_thinking: &mut bool,
    showed_thinking: &mut bool,
    showed_answer: &mut bool,
    markdown: &mut ui::MarkdownStream,
) -> Result<()> {
    let mut rest = delta;
    loop {
        if *inline_thinking {
            if let Some(end) = rest.find("</think>") {
                let thinking = &rest[..end];
                if show_thinking {
                    ui::stream_thinking(thinking)?;
                }
                *inline_thinking = false;
                rest = &rest[end + "</think>".len()..];
                continue;
            }

            if show_thinking {
                ui::stream_thinking(rest)?;
            }
            return Ok(());
        }

        if let Some(start) = rest.find("<think>") {
            let content = &rest[..start];
            if !content.is_empty() {
                stream_answer_delta(content, showed_thinking, showed_answer, markdown)?;
            }
            *inline_thinking = true;
            if show_thinking && !*showed_thinking {
                ui::thinking_start()?;
                *showed_thinking = true;
            }
            rest = &rest[start + "<think>".len()..];
            continue;
        }

        if !rest.is_empty() {
            stream_answer_delta(rest, showed_thinking, showed_answer, markdown)?;
        }
        return Ok(());
    }
}

struct TuiGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    fn inner(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        );
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Clone)]
struct TranscriptItem {
    role: &'static str,
    content: String,
}

impl TranscriptItem {
    fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Clone)]
enum PromptAttachment {
    File { path: String, content: String },
    Image { path: String, size_bytes: u64 },
}

impl PromptAttachment {
    fn summary(&self) -> String {
        match self {
            Self::File { path, .. } => format!("file:{path}"),
            Self::Image { path, .. } => format!("image:{path}"),
        }
    }
}

#[derive(Clone)]
struct SearchPicker {
    query: String,
    results: Vec<SearchResult>,
    selected: usize,
}

impl SearchPicker {
    fn new(query: String, results: Vec<SearchResult>) -> Self {
        Self {
            query,
            results,
            selected: 0,
        }
    }

    fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemoryStore {
    notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct LoadedSkill {
    name: String,
    path: PathBuf,
    content: String,
}

fn compose_prompt_with_attachments(input: &str, attachments: &[PromptAttachment]) -> String {
    if attachments.is_empty() {
        return input.to_string();
    }

    let mut output = String::new();
    for attachment in attachments {
        match attachment {
            PromptAttachment::File { path, content } => {
                output.push_str(&format!(
                    "[attached file: {path}]\n```text\n{content}\n```\n\n"
                ));
            }
            PromptAttachment::Image { path, size_bytes } => {
                output.push_str(&format!(
                    "[attached image: {path}]\nlocal image reference ({size_bytes} bytes)\n\n"
                ));
            }
        }
    }
    output.push_str(input);
    output
}

fn load_memory_store(path: &Path) -> Result<MemoryStore> {
    if !path.exists() {
        return Ok(MemoryStore::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read memory file {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse memory file {}", path.display()))
}

fn save_memory_store(path: &Path, store: &MemoryStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(store).context("failed to serialize memory store")?;
    fs::write(path, text).with_context(|| format!("failed to write memory file {}", path.display()))
}

fn load_skills(dir: &Path, active_skills: &[String]) -> Result<Vec<LoadedSkill>> {
    let mut loaded = Vec::new();
    for name in active_skills {
        if let Some(skill) = load_skill(dir, name)? {
            loaded.push(skill);
        }
    }
    Ok(loaded)
}

fn relocate_under_workspace(path: &Path, old_workspace: &Path, new_workspace: &Path) -> PathBuf {
    if path.is_absolute() {
        if let Ok(relative) = path.strip_prefix(old_workspace) {
            new_workspace.join(relative)
        } else {
            path.to_path_buf()
        }
    } else if let Ok(relative) = path.strip_prefix(old_workspace) {
        new_workspace.join(relative)
    } else {
        new_workspace.join(path)
    }
}

fn sync_workspace_context(old_workspace: &Path, new_workspace: &Path) -> Result<()> {
    copy_file_if_exists(old_workspace, new_workspace, "AGENT.md")?;
    copy_file_if_exists(old_workspace, new_workspace, "AGENTS.md")?;
    copy_file_if_exists(old_workspace, new_workspace, "PLAN.md")?;
    copy_file_if_exists(old_workspace, new_workspace, "autofix_config.json")?;
    copy_file_if_exists(old_workspace, new_workspace, "memory.json")?;
    copy_dir_if_exists(old_workspace, new_workspace, "skills")?;
    Ok(())
}

fn initialize_session(config: &Config) -> Result<SessionRecord> {
    let session_id = config
        .session_id
        .clone()
        .unwrap_or_else(|| make_session_id(config.role));

    if let Some(source_id) = config.resume_session.as_deref() {
        let source = load_session_record(&config.workspace, source_id)?;
        return Ok(SessionRecord::resume_from(
            &source,
            config.workspace.clone(),
            config.role,
            session_id,
        ));
    }

    Ok(SessionRecord::new(
        config.workspace.clone(),
        config.role,
        session_id,
    ))
}

fn copy_file_if_exists(old_workspace: &Path, new_workspace: &Path, name: &str) -> Result<()> {
    let source = old_workspace.join(name);
    if !source.exists() {
        return Ok(());
    }
    let dest = new_workspace.join(name);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(&source, &dest)
        .with_context(|| format!("failed to copy {} to {}", source.display(), dest.display()))?;
    Ok(())
}

fn copy_dir_if_exists(old_workspace: &Path, new_workspace: &Path, name: &str) -> Result<()> {
    let source = old_workspace.join(name);
    if !source.exists() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(&source)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let relative = path.strip_prefix(old_workspace).unwrap_or(path);
        let dest = new_workspace.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest)
                .with_context(|| format!("failed to create {}", dest.display()))?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(path, &dest).with_context(|| {
                format!("failed to copy {} to {}", path.display(), dest.display())
            })?;
        }
    }
    Ok(())
}

fn write_config_snapshot(path: &Path, file: &crate::config::ConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(file).context("failed to serialize config")?;
    fs::write(path, text).with_context(|| format!("failed to write config file {}", path.display()))
}

fn worker_config_snapshot(
    config: &Config,
    worker_workspace: &Path,
    task_file: &Path,
    worker_id: &str,
    worker_name: &str,
) -> crate::config::ConfigFile {
    let mut snapshot = config.snapshot_config_file();
    snapshot.role = Some(AgentRole::Worker);
    snapshot.workspace = Some(worker_workspace.to_path_buf());
    snapshot.task_file = Some(task_file.to_path_buf());
    snapshot.worker_id = Some(worker_id.to_string());
    snapshot.worker_name = Some(worker_name.to_string());
    snapshot.memory_file = Some(PathBuf::from("memory.json"));
    snapshot.skills_dir = Some(PathBuf::from("skills"));
    snapshot.auto_worktree = Some(false);
    snapshot.autonomous = Some(true);
    snapshot.tui = Some(false);
    snapshot.full_system_access = Some(false);
    snapshot.dangerously_allow_shell = Some(false);
    snapshot.auto_write = Some(false);
    snapshot.approval_mode = Some(PermissionMode::Allow);
    snapshot.shell_approval = Some(PermissionMode::Allow);
    snapshot.write_approval = Some(PermissionMode::Allow);
    snapshot
}

fn load_skill(dir: &Path, name: &str) -> Result<Option<LoadedSkill>> {
    let candidates = [
        dir.join(format!("{name}.md")),
        dir.join(format!("{name}.txt")),
        dir.join(name).join("SKILL.md"),
    ];
    for path in candidates {
        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read skill {}", path.display()))?;
            return Ok(Some(LoadedSkill {
                name: name.to_string(),
                path,
                content,
            }));
        }
    }
    Ok(None)
}

fn build_system_prompt(config: &Config, memory: &MemoryStore, skills: &[LoadedSkill]) -> String {
    let mut sections = Vec::new();
    sections.push(config.system.clone().unwrap_or_else(default_system_prompt));

    if !memory.notes.is_empty() {
        sections.push(format!(
            "Persistent memory:\n{}",
            memory
                .notes
                .iter()
                .map(|note| format!("- {note}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !skills.is_empty() {
        let rendered = skills
            .iter()
            .map(|skill| format!("Skill: {}\n{}", skill.name, skill.content.trim()))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(rendered);
    }

    if let Some(agents) = read_workspace_note(&config.workspace, "AGENT.md")
        .or_else(|| read_workspace_note(&config.workspace, "AGENTS.md"))
    {
        sections.push(format!("Workspace instructions:\n{}", agents.trim()));
    }

    if let Some(plan) = read_workspace_note(&config.workspace, "PLAN.md") {
        sections.push(format!("Session plan:\n{}", plan.trim()));
    }

    if config.is_worker() {
        sections.push(
            "Worker orchestration:\n- You are an isolated worker agent running in a dedicated worktree.\n- Work only on the assigned task and report concise progress.\n- Do not spawn additional workers.\n- Keep the result scoped to the branch and workspace you were given."
                .to_string(),
        );
    } else {
        sections.push(
            "Master orchestration:\n- You may delegate independent subproblems to isolated worker agents.\n- Prefer workers for side tasks that can live in their own worktree and context.\n- Review worker summaries before merging their branches.\n- Keep the main chat focused on coordination, review, and final decisions."
                .to_string(),
        );
    }

    sections.join("\n\n")
}

fn read_workspace_note(workspace: &Path, name: &str) -> Option<String> {
    let path = workspace.join(name);
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().take(120).collect::<Vec<_>>().join("\n"))
}

fn ensure_agent_doc(workspace: &Path) -> Result<()> {
    let agent_md = workspace.join("AGENT.md");
    if agent_md.exists() {
        return Ok(());
    }

    let agents_md = workspace.join("AGENTS.md");
    if agents_md.exists() {
        fs::copy(&agents_md, &agent_md).with_context(|| {
            format!("failed to initialize AGENT.md from {}", agents_md.display())
        })?;
        return Ok(());
    }

    let default = r#"# AGENT.md

## Operating Rules

- Keep PLAN.md current and rewrite it when the session state changes.
- Use PLAN.md for summary, files changed, and next steps.
- Prefer isolated worktrees for feature work when auto_worktree is enabled.
- Use isolated worker agents for side tasks that can live in their own worktree and context.
- Keep the master agent focused on coordination, review, and final decisions.
- Keep memory.json and skills/ as durable context, not repeated chat history.
- Do not revert unrelated user changes.
- Preserve kernel backport constraints: minimal patching, semantic conflict resolution, targeted validation.

## Session Flow

- Update PLAN.md after startup, config reloads, worktree changes, skill changes, memory changes, and after tool rounds that change the codebase.
- Keep the plan short and actionable.
- When blocked, record the blocker in PLAN.md and stop adding speculative steps.
- When delegating to a worker, copy only the scoped task into that worker's prompt and let the worker own its own PLAN.md.
"#;

    fs::write(&agent_md, default)
        .with_context(|| format!("failed to initialize {}", agent_md.display()))?;
    Ok(())
}

fn draw_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: &Agent,
    transcript: &[TranscriptItem],
    input: &str,
    status: &str,
    show_help: bool,
    scroll_offset: usize,
    search_picker: Option<&SearchPicker>,
) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.size();
        let layout = tui_layout(area);

        let title = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("agent", Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(
                    agent.config.banner_title.as_str(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    agent.config.banner_subtitle.as_str(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(vec![
                badge(format!("{:?}", agent.config.provider), Color::Yellow),
                Span::raw(" "),
                badge(agent.provider.model().to_string(), Color::Green),
                Span::raw(" "),
                badge(
                    agent.config.access_label().to_string(),
                    access_color(agent.config.full_system_access),
                ),
                Span::raw(" "),
                badge(
                    if agent.config.is_worker() {
                        "worker"
                    } else {
                        "master"
                    },
                    Color::Magenta,
                ),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("agent"));
        frame.render_widget(title, layout.top[0]);

        let status_panel = Paragraph::new(vec![
            Line::from(vec![
                label("status", Color::DarkGray),
                Span::raw(" "),
                badge(status.to_string(), status_color(status)),
            ]),
            Line::from(vec![
                label("progress", Color::DarkGray),
                Span::raw(" "),
                badge(
                    if agent.progress.is_empty() {
                        "idle".to_string()
                    } else {
                        agent.progress.clone()
                    },
                    Color::Blue,
                ),
            ]),
            Line::from(vec![
                label("perm", Color::DarkGray),
                Span::raw(" "),
                badge(
                    format!("shell={:?}", agent.tools.shell_permission()),
                    Color::Yellow,
                ),
                Span::raw(" "),
                badge(
                    format!("write={:?}", agent.tools.write_permission()),
                    Color::Yellow,
                ),
            ]),
            Line::from(vec![
                label("access", Color::DarkGray),
                Span::raw(" "),
                badge(
                    if agent.tools.full_system_access() {
                        "FULL SYSTEM".to_string()
                    } else {
                        "workspace".to_string()
                    },
                    access_color(agent.tools.full_system_access()),
                ),
            ]),
            Line::from(vec![
                label("worker", Color::DarkGray),
                Span::raw(" "),
                badge(
                    agent
                        .config
                        .worker_id
                        .as_deref()
                        .unwrap_or("none")
                        .to_string(),
                    Color::Magenta,
                ),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("state"));
        frame.render_widget(status_panel, layout.top[1]);

        let transcript_lines = transcript
            .iter()
            .flat_map(render_transcript_item)
            .collect::<Vec<_>>();
        let transcript_height = transcript_viewport_height(layout.transcript);
        let max_scroll = max_scroll_offset(transcript_lines.len(), transcript_height);
        let scroll_offset = scroll_offset.min(max_scroll);
        let mut scrollbar_state = ScrollbarState::new(transcript_lines.len())
            .position(scroll_offset)
            .viewport_content_length(transcript_height);
        let transcript_widget = Paragraph::new(transcript_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("transcript")
                    .title_bottom(Line::from(vec![Span::styled(
                        "PgUp/PgDn scroll  mouse wheel/drag  ? help  Esc quit",
                        Style::default().fg(Color::DarkGray),
                    )])),
            )
            .scroll((scroll_offset.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(transcript_widget, layout.transcript);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            layout.transcript,
            &mut scrollbar_state,
        );

        let input_widget = Paragraph::new(input.to_string())
            .block(Block::default().borders(Borders::ALL).title("input"))
            .wrap(Wrap { trim: false });
        frame.render_widget(input_widget, layout.input);

        if show_help {
            let overlay_area = centered_rect(80, 75, area);
            frame.render_widget(Clear, overlay_area);
            let help = Paragraph::new(tui_help_panel())
                .block(Block::default().borders(Borders::ALL).title("help"))
                .wrap(Wrap { trim: false });
            frame.render_widget(help, overlay_area);
        }

        if let Some(picker) = search_picker {
            let overlay_area = centered_rect(84, 70, area);
            frame.render_widget(Clear, overlay_area);

            let items = picker
                .results
                .iter()
                .map(|result| {
                    let mut lines = vec![Line::from(vec![
                        Span::styled("• ", Style::default().fg(Color::Yellow)),
                        Span::styled(
                            result.title.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])];
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(result.url.clone(), Style::default().fg(Color::Blue)),
                    ]));
                    if !result.snippet.trim().is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled("  ", Style::default().fg(Color::DarkGray)),
                            Span::styled(result.snippet.clone(), Style::default().fg(Color::Gray)),
                        ]));
                    }
                    ListItem::new(lines)
                })
                .collect::<Vec<_>>();

            let mut list_state = ListState::default();
            list_state.select(Some(picker.selected));
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("search: {}", picker.query))
                        .title_bottom(Line::from(vec![Span::styled(
                            "↑/↓ move  Enter insert URL  Esc close",
                            Style::default().fg(Color::DarkGray),
                        )])),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );
            frame.render_stateful_widget(list, overlay_area, &mut list_state);
        }
    })?;
    Ok(())
}

struct TuiLayout {
    top: [Rect; 2],
    transcript: Rect,
    input: Rect,
}

fn tui_layout(area: Rect) -> TuiLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(vertical[0]);

    TuiLayout {
        top: [top[0], top[1]],
        transcript: vertical[1],
        input: vertical[2],
    }
}

fn transcript_viewport_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

fn transcript_content_len(transcript: &[TranscriptItem]) -> usize {
    transcript
        .iter()
        .map(render_transcript_item)
        .map(|lines| lines.len())
        .sum()
}

fn max_scroll_offset(content_len: usize, viewport_len: usize) -> usize {
    content_len.saturating_sub(viewport_len)
}

fn clamp_scroll_offset(scroll_offset: usize, content_len: usize, viewport_len: usize) -> usize {
    scroll_offset.min(max_scroll_offset(content_len, viewport_len))
}

fn handle_mouse_event(
    mouse: MouseEvent,
    area: Rect,
    transcript: &[TranscriptItem],
    scroll_offset: &mut usize,
) -> Result<bool> {
    let layout = tui_layout(area);
    let transcript_area = layout.transcript;
    let transcript_len = transcript_content_len(transcript);
    let viewport_len = transcript_viewport_height(transcript_area);
    let max_scroll = max_scroll_offset(transcript_len, viewport_len);

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            *scroll_offset = scroll_offset.saturating_add(3).min(max_scroll);
            return Ok(true);
        }
        MouseEventKind::ScrollDown => {
            *scroll_offset = scroll_offset.saturating_sub(3);
            return Ok(true);
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if is_on_transcript_scrollbar(mouse, transcript_area) {
                *scroll_offset = mouse_row_to_scroll_offset(mouse.row, transcript_area, max_scroll);
                return Ok(true);
            }
        }
        _ => {}
    }

    Ok(false)
}

fn is_on_transcript_scrollbar(mouse: MouseEvent, transcript_area: Rect) -> bool {
    if transcript_area.width < 1 || transcript_area.height < 1 {
        return false;
    }
    let scrollbar_x = transcript_area.right().saturating_sub(1);
    mouse.column == scrollbar_x
        && mouse.row >= transcript_area.top()
        && mouse.row < transcript_area.bottom()
}

fn mouse_row_to_scroll_offset(row: u16, transcript_area: Rect, max_scroll: usize) -> usize {
    if max_scroll == 0 || transcript_area.height <= 1 {
        return 0;
    }

    let rel = row.saturating_sub(transcript_area.top()) as usize;
    let track_len = transcript_area.height.saturating_sub(1) as usize;
    if track_len == 0 {
        return 0;
    }

    rel.min(track_len).saturating_mul(max_scroll) / track_len
}

fn render_transcript_item(item: &TranscriptItem) -> Vec<Line<'static>> {
    let role_style = match item.role {
        "user" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "assistant" => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        "system" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "error" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::BOLD),
    };

    let role_name = if item.role == "assistant" {
        "autofix"
    } else {
        item.role
    };
    let accent = match item.role {
        "user" => Color::Green,
        "assistant" => Color::Cyan,
        "system" => Color::Yellow,
        "error" => Color::Red,
        _ => Color::DarkGray,
    };

    let mut lines = vec![Line::from(vec![
        Span::styled("│ ", Style::default().fg(accent)),
        Span::styled(format!(" {role_name} "), role_style),
    ])];
    lines.extend(render_markdown_lines(&item.content, accent));
    lines.push(Line::from(""));
    lines
}

fn render_markdown_lines(text: &str, accent: Color) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            lines.push(Line::styled(
                format!("  {}", trimmed),
                Style::default().fg(Color::DarkGray),
            ));
            continue;
        }
        if in_code {
            lines.push(Line::styled(
                format!("    {raw}"),
                Style::default().fg(Color::LightBlue).bg(Color::Black),
            ));
            continue;
        }
        if let Some((level, heading)) = tui_heading(trimmed) {
            lines.push(Line::styled(
                format!("  {} {}", "#".repeat(level), heading),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            lines.push(Line::from(vec![
                Span::styled("  ├ ", Style::default().fg(accent)),
                Span::raw(item.to_string()),
            ]));
        } else if trimmed.starts_with('>') {
            lines.push(Line::styled(
                format!("  │ {}", trimmed.trim_start_matches('>').trim_start()),
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(Color::DarkGray)),
                Span::raw(raw.to_string()),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn tui_help_panel() -> Vec<Line<'static>> {
    vec![
        Line::from("Keys"),
        Line::from("  Enter  submit"),
        Line::from("  Tab    complete commands and paths"),
        Line::from("  Up/Down browse input history when input is empty"),
        Line::from("  PgUp/PgDn scroll transcript"),
        Line::from("  Mouse wheel scroll transcript"),
        Line::from("  Drag or click the scrollbar to reposition"),
        Line::from("  ?      toggle this help"),
        Line::from("  Esc    exit"),
        Line::from(""),
        Line::from("Commands"),
        Line::from("  /help /provider /models /use-model"),
        Line::from("  /thinking /search /clear /exit"),
        Line::from("  /worktree /worktree list /worktree auto /worktree add <path>"),
        Line::from("  /agents /agents spawn <name> | <task> /agents read <id>"),
        Line::from(""),
        Line::from("The full line-mode command surface still handles shell, terminal, writes, and permissions."),
    ]
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn trim_transcript(transcript: &mut Vec<TranscriptItem>) {
    let excess = transcript.len().saturating_sub(MAX_TUI_HISTORY);
    if excess > 0 {
        transcript.drain(0..excess);
    }
}

fn previous_history_index(current: Option<usize>, len: usize) -> Option<usize> {
    match current {
        None if len > 0 => Some(len - 1),
        Some(0) => Some(0),
        Some(idx) => Some(idx.saturating_sub(1)),
        None => None,
    }
}

fn next_history_index(current: Option<usize>, len: usize) -> Option<usize> {
    match current {
        Some(idx) if idx + 1 < len => Some(idx + 1),
        _ => None,
    }
}

fn tui_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&level) && line[level..].starts_with(' ') {
        Some((level, &line[level + 1..]))
    } else {
        None
    }
}

fn badge<T: Into<String>>(text: T, color: Color) -> Span<'static> {
    let text = text.into();
    Span::styled(
        format!(" {text} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn label(text: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!("{text}:"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn access_color(full_system: bool) -> Color {
    if full_system {
        Color::Red
    } else {
        Color::Blue
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "ready" => Color::Green,
        "streaming" => Color::Cyan,
        "thinking" => Color::Yellow,
        "interrupted" => Color::Magenta,
        "blocked" => Color::Red,
        "complete" => Color::Green,
        _ => Color::DarkGray,
    }
}

fn tui_help_text() -> &'static str {
    "Ratatui mode commands:\n- /help\n- /config\n- /config reload\n- /provider\n- /models\n- /use-model <name>\n- /thinking [auto|on|off|low|medium|high|show|hide]\n- /attach [show|clear|file <path>|image <path>]\n- /search <query> (opens a result picker)\n- /worktree [status|list|auto|add <path> [branch]|switch <path>|remove <path>|prune]\n- /agents [list|spawn <name> | <task>|read <id>]\n- /session [show|list|history|save|new|resume <id>]\n- /history\n- /interrupt\n- /clear\n- /exit\n\nMouse and keyboard navigation:\n- Mouse wheel scrolls the transcript\n- Drag or click the scrollbar to reposition\n- Up/Down browse history when the input is empty\n- PgUp/PgDn scroll the transcript\n- ? toggles this help overlay\n\nCtrl-C interrupts the active model stream and saves the session."
}

fn onboarding_text(full_system_access: bool, onboarding: &[String]) -> String {
    let access = if full_system_access {
        "\n\nWARNING: full system access is enabled. Absolute paths, path escapes, shell commands, and writes are allowed."
    } else {
        ""
    };
    let onboarding_lines = if onboarding.is_empty() {
        vec![
            "/help commands".to_string(),
            "/models local models".to_string(),
            "/search web search".to_string(),
            "/permissions safety".to_string(),
            "/terminal real shell".to_string(),
            "/exit quit".to_string(),
        ]
    } else {
        onboarding.to_vec()
    };
    let onboarding_text = onboarding_lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# Welcome\n- Paste a kernel commit ID, patch context, or target backport task to begin.\n{onboarding_text}{access}"
    )
}

fn filter_tui_content_delta(
    delta: &str,
    show_thinking: bool,
    inline_thinking: &mut bool,
) -> String {
    let mut output = String::new();
    let mut rest = delta;
    loop {
        if *inline_thinking {
            if let Some(end) = rest.find("</think>") {
                if show_thinking {
                    output.push_str(&rest[..end]);
                }
                *inline_thinking = false;
                rest = &rest[end + "</think>".len()..];
                continue;
            }
            if show_thinking {
                output.push_str(rest);
            }
            break;
        }

        if let Some(start) = rest.find("<think>") {
            output.push_str(&rest[..start]);
            *inline_thinking = true;
            rest = &rest[start + "<think>".len()..];
            continue;
        }

        output.push_str(rest);
        break;
    }
    output
}

fn stream_answer_delta(
    delta: &str,
    showed_thinking: &mut bool,
    showed_answer: &mut bool,
    markdown: &mut ui::MarkdownStream,
) -> Result<()> {
    if *showed_thinking && !*showed_answer {
        ui::thinking_end()?;
    }
    *showed_answer = true;
    markdown.push(delta)?;
    Ok(())
}

fn strip_think_blocks(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;

    loop {
        let Some(start) = rest.find("<think>") else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        rest = &rest[start + "<think>".len()..];

        let Some(end) = rest.find("</think>") else {
            break;
        };
        rest = &rest[end + "</think>".len()..];
    }

    output.trim().to_string()
}

fn parse_think_mode(value: &str) -> Option<ThinkMode> {
    match value {
        "auto" => Some(ThinkMode::Auto),
        "on" | "true" => Some(ThinkMode::On),
        "off" | "false" | "nothink" => Some(ThinkMode::Off),
        "low" => Some(ThinkMode::Low),
        "medium" => Some(ThinkMode::Medium),
        "high" => Some(ThinkMode::High),
        _ => None,
    }
}

fn parse_permission_mode(value: &str) -> Option<PermissionMode> {
    match value {
        "ask" | "prompt" => Some(PermissionMode::Ask),
        "allow" | "auto" | "always" => Some(PermissionMode::Allow),
        "deny" | "never" => Some(PermissionMode::Deny),
        _ => None,
    }
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '-',
        })
        .collect::<String>()
}

fn parse_git_status(status: &str) -> Vec<String> {
    status
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            if line.len() < 3 {
                return Some(line.to_string());
            }
            let path = line[3..].trim();
            if path.is_empty() {
                None
            } else {
                Some(format!("{line}"))
            }
        })
        .collect()
}

fn format_stop_sequences(stops: &[String]) -> String {
    if stops.is_empty() {
        "none".to_string()
    } else {
        stops
            .iter()
            .map(|stop| format!("{stop:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn extract_tool_calls(text: &str) -> Result<Vec<ToolCall>> {
    let mut calls = Vec::new();
    let mut search_start = 0;

    while let Some(start) = text[search_start..].find("```json") {
        let start = search_start + start;
        let json_start = start + "```json".len();
        let Some(end) = text[json_start..].find("```") else {
            break;
        };
        let json_text = &text[json_start..json_start + end];
        let block = json_text.trim();
        if block.is_empty() {
            search_start = json_start + end + "```".len();
            continue;
        }

        if let Ok(batch) = serde_json::from_str::<Vec<ToolCall>>(block) {
            calls.extend(batch);
        } else {
            let call = serde_json::from_str::<ToolCall>(block)
                .with_context(|| format!("failed to parse tool call JSON: {json_text}"))?;
            calls.push(call);
        }
        search_start = json_start + end + "```".len();
    }

    Ok(calls)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentTurn {
    ToolCalls { calls: Vec<ToolCall> },
    Final { summary: String },
    Blocked { reason: String },
    NeedsWorker { task: String },
}

fn parse_agent_turn(text: &str) -> Result<Option<AgentTurn>> {
    let Some(json_text) = extract_json_candidate(text) else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(json_text.trim())
        .with_context(|| format!("failed to parse turn JSON: {json_text}"))?;
    if let Some(turn) = parse_agent_turn_value(&value, json_text)? {
        return Ok(Some(turn));
    }
    Ok(None)
}

fn extract_json_candidate(text: &str) -> Option<&str> {
    if let Some(start) = text.find("```json") {
        let json_start = start + "```json".len();
        let end = text[json_start..].find("```")?;
        return Some(&text[json_start..json_start + end]);
    }

    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        Some(trimmed)
    } else {
        None
    }
}

fn parse_agent_turn_value(value: &serde_json::Value, raw: &str) -> Result<Option<AgentTurn>> {
    if let Ok(turn) = serde_json::from_value::<AgentTurn>(value.clone()) {
        return Ok(Some(turn));
    }

    if let Some(state) = value.get("state").and_then(|state| state.as_str()) {
        match state {
            "final" => {
                if let Some(summary) = value.get("summary").and_then(|summary| summary.as_str()) {
                    return Ok(Some(AgentTurn::Final {
                        summary: summary.to_string(),
                    }));
                }
            }
            "blocked" => {
                if let Some(reason) = value.get("reason").and_then(|reason| reason.as_str()) {
                    return Ok(Some(AgentTurn::Blocked {
                        reason: reason.to_string(),
                    }));
                }
            }
            "needs_worker" => {
                if let Some(task) = value.get("task").and_then(|task| task.as_str()) {
                    return Ok(Some(AgentTurn::NeedsWorker {
                        task: task.to_string(),
                    }));
                }
            }
            "tool_calls" => {
                if let Some(calls) = value.get("calls").or_else(|| value.get("tool_calls")) {
                    let calls = parse_tool_calls_value(calls, raw)?;
                    return Ok(Some(AgentTurn::ToolCalls { calls }));
                }
            }
            _ => {}
        }
    }

    if let Some(kind) = value.get("kind").and_then(|kind| kind.as_str()) {
        if kind == "tool_calls" {
            if let Some(calls) = value.get("calls").or_else(|| value.get("tool_calls")) {
                let calls = parse_tool_calls_value(calls, raw)?;
                return Ok(Some(AgentTurn::ToolCalls { calls }));
            }
        }
    }

    if value.get("tool").is_some() {
        let call = serde_json::from_value::<ToolCall>(value.clone())
            .with_context(|| format!("failed to parse tool call JSON: {raw}"))?;
        return Ok(Some(AgentTurn::ToolCalls { calls: vec![call] }));
    }

    if let Some(calls) = value.get("calls").or_else(|| value.get("tool_calls")) {
        let calls = parse_tool_calls_value(calls, raw)?;
        return Ok(Some(AgentTurn::ToolCalls { calls }));
    }

    if value.is_array() {
        let calls = serde_json::from_value::<Vec<ToolCall>>(value.clone())
            .with_context(|| format!("failed to parse tool call array: {raw}"))?;
        return Ok(Some(AgentTurn::ToolCalls { calls }));
    }

    if value
        .get("summary")
        .and_then(|summary| summary.as_str())
        .is_some()
        && value.get("final").is_some()
    {
        if let Some(summary) = value.get("summary").and_then(|summary| summary.as_str()) {
            return Ok(Some(AgentTurn::Final {
                summary: summary.to_string(),
            }));
        }
    }

    Ok(None)
}

fn parse_tool_calls_value(value: &serde_json::Value, raw: &str) -> Result<Vec<ToolCall>> {
    if value.is_array() {
        serde_json::from_value::<Vec<ToolCall>>(value.clone())
            .with_context(|| format!("failed to parse tool call array: {raw}"))
    } else {
        let call = serde_json::from_value::<ToolCall>(value.clone())
            .with_context(|| format!("failed to parse tool call JSON: {raw}"))?;
        Ok(vec![call])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_batched_tool_blocks() {
        let text = r#"
Here is the work.
```json
{"tool":"read_file","path":"src/main.rs"}
```
```json
{"tool":"list_files","path":"src"}
```
"#;

        let calls = extract_tool_calls(text).expect("tool blocks should parse");
        assert_eq!(calls.len(), 2);
        match &calls[0] {
            ToolCall::ReadFile { path } => assert_eq!(path, "src/main.rs"),
            other => panic!("unexpected first call: {other:?}"),
        }
        match &calls[1] {
            ToolCall::ListFiles { path } => assert_eq!(path.as_deref(), Some("src")),
            other => panic!("unexpected second call: {other:?}"),
        }
    }

    #[test]
    fn parses_codex_turn_object() {
        let turn = r#"
```json
{"type":"tool_calls","calls":[{"tool":"read_file","path":"src/main.rs"},{"tool":"list_files","path":"src"}]}
```
"#;

        match parse_agent_turn(turn)
            .expect("turn should parse")
            .expect("turn should exist")
        {
            AgentTurn::ToolCalls { calls } => {
                assert_eq!(calls.len(), 2);
                match &calls[0] {
                    ToolCall::ReadFile { path } => assert_eq!(path, "src/main.rs"),
                    other => panic!("unexpected first call: {other:?}"),
                }
                match &calls[1] {
                    ToolCall::ListFiles { path } => assert_eq!(path.as_deref(), Some("src")),
                    other => panic!("unexpected second call: {other:?}"),
                }
            }
            other => panic!("unexpected turn: {other:?}"),
        }
    }

    #[test]
    fn parses_codex_final_directive() {
        let final_block = r#"
```json
{"type":"final","summary":"done"}
```
"#;
        let blocked_block = r#"
```json
{"type":"blocked","reason":"missing context"}
```
"#;
        let worker_block = r#"
```json
{"type":"needs_worker","task":"isolate parser work"}
```
"#;

        match parse_agent_turn(final_block)
            .expect("final directive should parse")
            .expect("directive should exist")
        {
            AgentTurn::Final { summary } => assert_eq!(summary, "done"),
            other => panic!("unexpected directive: {other:?}"),
        }

        match parse_agent_turn(blocked_block)
            .expect("blocked directive should parse")
            .expect("directive should exist")
        {
            AgentTurn::Blocked { reason } => assert_eq!(reason, "missing context"),
            other => panic!("unexpected directive: {other:?}"),
        }

        match parse_agent_turn(worker_block)
            .expect("worker directive should parse")
            .expect("directive should exist")
        {
            AgentTurn::NeedsWorker { task } => assert_eq!(task, "isolate parser work"),
            other => panic!("unexpected directive: {other:?}"),
        }
    }
}

fn default_system_prompt() -> String {
    r#"You are an autonomous Linux kernel backporting agent running inside a local workspace.

Your primary job is to backport upstream Linux kernel commits into the target kernel tree safely and explain the resulting technical choices. Prefer acting end-to-end: inspect the tree, identify relevant files, apply or synthesize the backport, resolve conflicts, build or run focused verification when available, and report the exact result.

Backporting workflow:
- First identify the target kernel version, current branch, and dirty worktree state when relevant.
- Inspect the upstream commit context before editing: commit message, touched files, surrounding code, dependencies, and prerequisite commits.
- Compare upstream code with the target tree instead of blindly applying patches.
- Preserve target-tree APIs and stable-kernel conventions when upstream helpers or structures do not exist.
- Resolve conflicts semantically, not mechanically.
- Keep changes minimal and limited to the requested backport.
- Do not revert unrelated local changes.
- Prefer existing kernel style, local helper APIs, and nearby patterns.
- For conflicts, explain which side was kept, what was adapted, and why.
- For missing prerequisite functionality, either backport the minimal dependency or adapt to the target API; state the tradeoff.
- For kernel code, avoid broad refactors unless they are required for correctness.
- Use the built-in `web_search` tool for internet lookups. It defaults to DuckDuckGo and should be used when you need recent upstream context, documentation, or external references.
- After edits, run focused checks when possible, such as compile checks, relevant selftests, scripts/checkpatch.pl for patch hygiene, or grep-based validation.
- Use the Codex-style turn protocol:
  - When tools are needed, return a single JSON object with `{"type":"tool_calls","calls":[...]}`.
  - Put every required tool call for the next step into that `calls` array.
  - When the task is done, return `{"type":"final","summary":"..."}`.
  - When the task cannot proceed, return `{"type":"blocked","reason":"..."}`.
  - When remaining work should move to a separate worktree, return `{"type":"needs_worker","task":"..."}`.
- Do not bury tool calls inside prose or spread them across multiple messages. One turn should either ask for all needed tools or finish.
- Near the tool budget limit, stop exploring and choose final, blocked, or needs_worker.

Safety rules:
- Treat shell commands and writes as potentially risky and use the available tool workflow.
- Use relative paths only.
- Never use destructive commands like reset, checkout, clean, or rm unless the user explicitly asks.
- Never claim a backport is complete unless you have inspected the affected code and verified the final tree state as far as the environment allows.
- If verification cannot be run, say exactly why and suggest the next concrete command.

Response style:
- Be concise and technical.
- Lead with the current action or result.
- Include file paths and concrete function or symbol names when explaining changes.
- When blocked, state the blocker and the next best action.

You can ask to use tools by returning one or more fenced JSON blocks:
```json
{"tool":"read_file","path":"src/main.rs"}
```

Available tools:
- list_files: {"tool":"list_files","path":"."}
- read_file: {"tool":"read_file","path":"relative/path"}
- write_file: {"tool":"write_file","path":"relative/path","content":"full file content"}
- run_shell: {"tool":"run_shell","command":"cargo test"}
- web_search: {"tool":"web_search","query":"linux kernel backporting", "max_results": 5}
- spawn_worker: {"tool":"spawn_worker","name":"optional label","task":"isolated task text"}
- list_workers: {"tool":"list_workers"}
- read_worker: {"tool":"read_worker","id":"worker-id"}

When you need tool output, return only the tool-call JSON block. After receiving tool results, continue the backporting workflow."#
        .to_string()
}
