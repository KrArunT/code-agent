use crate::{
    completion::{prompt_text, AgentCompleter},
    config::{list_ollama_models, Config, PermissionMode, ProviderKind, ThinkMode},
    provider::{Message, ProviderClient, Role, StreamEvent},
    tools::{ToolCall, ToolRuntime},
    ui,
};
use anyhow::{Context, Result};
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
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Terminal,
};
use rustyline::{
    config::{CompletionType, EditMode},
    error::ReadlineError,
    history::DefaultHistory,
    Config as RustylineConfig, Editor,
};
use std::{
    env, fs,
    io::{self, Stdout},
    process::{Command, Stdio},
    time::Duration,
};

pub struct Agent {
    config: Config,
    provider: ProviderClient,
    tools: ToolRuntime,
    messages: Vec<Message>,
    shell_mode: bool,
    prompt_attachments: Vec<PromptAttachment>,
}

const MAX_TUI_HISTORY: usize = 400;

impl Agent {
    pub fn new(config: Config) -> Result<Self> {
        let provider = ProviderClient::new(&config);
        let tools = ToolRuntime::new(
            config.workspace.clone(),
            config.shell_permission(),
            config.write_permission(),
            config.full_system_access,
        );
        let system = config.system.clone().unwrap_or_else(default_system_prompt);
        let messages = vec![Message {
            role: Role::System,
            content: system,
        }];
        Ok(Self {
            config,
            provider,
            tools,
            messages,
            shell_mode: false,
            prompt_attachments: Vec::new(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
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
        editor.set_helper(Some(AgentCompleter::new(self.config.workspace.clone())));

        loop {
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
            let _ = editor.add_history_entry(input.as_str());
            if input.starts_with('/') || input.starts_with('!') {
                if self.handle_command(&input).await? {
                    break;
                }
                continue;
            }
            if self.shell_mode {
                self.run_shell_command(&input).await?;
                continue;
            }

            if let Some(summary) = self.attachment_status_text() {
                ui::info(&summary);
            }
            let user_prompt = self.compose_user_prompt(&input);
            self.messages.push(Message {
                role: Role::User,
                content: user_prompt,
            });
            self.respond().await?;
        }
        Ok(())
    }

    pub fn is_tui_enabled(&self) -> bool {
        self.config.tui
    }

    pub async fn run_tui(&mut self) -> Result<()> {
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
        let mut input_history: Vec<String> = Vec::new();
        let mut history_index: Option<usize> = None;

        loop {
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
                )?;
                needs_draw = false;
            }

            if !event::poll(Duration::from_millis(100))? {
                continue;
            }

            match event::read()? {
                Event::Mouse(mouse) => {
                    if handle_mouse_event(mouse, size, &transcript, &mut scroll_offset)? {
                        needs_draw = true;
                    }
                }
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
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
                                needs_draw = true;
                                continue;
                            }

                            transcript.push(TranscriptItem::new("user", submitted.clone()));
                            if let Some(summary) = self.attachment_status_text() {
                                transcript.push(TranscriptItem::new("system", summary));
                            }
                            trim_transcript(&mut transcript);
                            let user_prompt = self.compose_user_prompt(&submitted);
                            self.messages.push(Message {
                                role: Role::User,
                                content: user_prompt,
                            });

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
                            )?;

                            let mut inline_thinking = false;
                            let mut visible_answer = String::new();
                            let show_thinking = self.config.show_thinking()
                                && !matches!(self.provider.think(), ThinkMode::Off);
                            let answer = self
                                .provider
                                .complete_stream(&self.messages, |event| {
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
                                    transcript[assistant_index].content = visible_answer.clone();
                                    draw_tui(
                                        terminal.inner(),
                                        self,
                                        &transcript,
                                        &input,
                                        &status,
                                        show_help,
                                        scroll_offset,
                                    )?;
                                    Ok(())
                                })
                                .await?;
                            let answer = strip_think_blocks(&answer);
                            transcript[assistant_index].content = answer.clone();
                            self.messages.push(Message {
                                role: Role::Assistant,
                                content: answer,
                            });
                            status = "ready".to_string();
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
            "/attach" => {
                if let Some(summary) = self.handle_attach_command(arg)? {
                    ui::info(&summary);
                }
                Ok(false)
            }
            "/help" => {
                println!("{}", ui::help_text());
                Ok(false)
            }
            "/provider" => {
                ui::info(&format!(
                    "provider={:?} model={} base_url={} think={:?} show_thinking={} stops={} permissions=shell:{:?},write:{:?} access={}",
                    self.config.provider,
                    self.provider.model(),
                    self.config.base_url(),
                    self.provider.think(),
                    self.config.show_thinking(),
                    format_stop_sequences(self.provider.stop_sequences()),
                    self.tools.shell_permission(),
                    self.tools.write_permission(),
                    self.config.access_label()
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
            "/attach" => {
                if let Some(summary) = self.handle_attach_command(arg)? {
                    transcript.push(TranscriptItem::new("system", summary));
                }
                Ok(false)
            }
            "/provider" => {
                transcript.push(TranscriptItem::new(
                    "system",
                    format!(
                        "provider={:?}\nmodel={}\nbase_url={}\nthink={:?}\npermissions=shell:{:?},write:{:?}\naccess={}",
                        self.config.provider,
                        self.provider.model(),
                        self.config.base_url(),
                        self.provider.think(),
                        self.tools.shell_permission(),
                        self.tools.write_permission(),
                        self.config.access_label()
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
        let result = self.tools.run_shell(command).await?;
        ui::tool_result(&result);
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

    async fn handle_config_command(&mut self, arg: &str) -> Result<String> {
        match arg {
            "" | "show" => Ok(format!(
                "config file: {}\nloaded: {}\nprovider={:?}\nmodel={}\nworkspace={}\nautonomous={}\nmax_tool_rounds={}",
                self.config.config_file.display(),
                if self.config.config_file_exists() {
                    "yes"
                } else {
                    "no"
                },
                self.config.provider,
                self.provider.model(),
                self.config.workspace.display(),
                self.config.autonomous,
                self.config.effective_max_tool_rounds()
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
        self.provider = ProviderClient::new(&self.config);
        self.tools = ToolRuntime::new(
            self.config.workspace.clone(),
            self.config.shell_permission(),
            self.config.write_permission(),
            self.config.full_system_access,
        );
        let system = self
            .config
            .system
            .clone()
            .unwrap_or_else(default_system_prompt);
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

    async fn respond(&mut self) -> Result<()> {
        for _ in 0..=self.config.effective_max_tool_rounds() {
            ui::assistant_start()?;
            let mut showed_thinking = false;
            let mut showed_answer = false;
            let mut inline_thinking = false;
            let mut markdown = ui::MarkdownStream::new();
            let show_thinking =
                self.config.show_thinking() && !matches!(self.provider.think(), ThinkMode::Off);
            let answer = self
                .provider
                .complete_stream(&self.messages, |event| {
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
                .await?;
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

            let Some(tool_call) = extract_tool_call(&answer)? else {
                return Ok(());
            };

            let result = self.tools.execute(tool_call).await?;
            ui::tool_result(&result);
            self.messages.push(Message {
                role: Role::User,
                content: format!("Tool result:\n{result}"),
            });
        }

        ui::error("stopped after max tool rounds");
        Ok(())
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

fn draw_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: &Agent,
    transcript: &[TranscriptItem],
    input: &str,
    status: &str,
    show_help: bool,
    scroll_offset: usize,
) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.size();
        let layout = tui_layout(area);

        let title = Paragraph::new(vec![
            Line::from(vec![
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
                Span::styled(
                    format!("{:?}", agent.config.provider),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" / "),
                Span::styled(
                    agent.provider.model().to_string(),
                    Style::default().fg(Color::Green),
                ),
                Span::raw(" / "),
                Span::styled(
                    agent.config.access_label().to_string(),
                    if agent.config.full_system_access {
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Blue)
                    },
                ),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("agent"));
        frame.render_widget(title, layout.top[0]);

        let status_panel = Paragraph::new(vec![
            Line::from(format!("status: {status}")),
            Line::from(format!(
                "perm: shell={:?} write={:?}",
                agent.tools.shell_permission(),
                agent.tools.write_permission()
            )),
            Line::from(format!(
                "access: {}",
                if agent.tools.full_system_access() {
                    "FULL SYSTEM"
                } else {
                    "workspace"
                }
            )),
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

    let mut lines = vec![Line::from(vec![Span::styled(
        format!("{}> ", item.role),
        role_style,
    )])];
    lines.extend(render_markdown_lines(&item.content));
    lines.push(Line::from(""));
    lines
}

fn render_markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            lines.push(Line::styled(
                trimmed.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            continue;
        }
        if in_code {
            lines.push(Line::styled(
                raw.to_string(),
                Style::default().fg(Color::LightBlue).bg(Color::Black),
            ));
            continue;
        }
        if let Some((level, heading)) = tui_heading(trimmed) {
            lines.push(Line::styled(
                format!("{} {}", "#".repeat(level), heading),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            lines.push(Line::from(vec![
                Span::styled("  - ", Style::default().fg(Color::Yellow)),
                Span::raw(item.to_string()),
            ]));
        } else if trimmed.starts_with('>') {
            lines.push(Line::styled(
                format!("| {}", trimmed.trim_start_matches('>').trim_start()),
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            lines.push(Line::from(raw.to_string()));
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
        Line::from("  /thinking /clear /exit"),
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

fn tui_help_text() -> &'static str {
    "Ratatui mode commands:\n- /help\n- /config\n- /config reload\n- /provider\n- /models\n- /use-model <name>\n- /thinking [auto|on|off|low|medium|high|show|hide]\n- /attach [show|clear|file <path>|image <path>]\n- /clear\n- /exit\n\nMouse and keyboard navigation:\n- Mouse wheel scrolls the transcript\n- Drag or click the scrollbar to reposition\n- Up/Down browse history when the input is empty\n- PgUp/PgDn scroll the transcript\n- ? toggles this help overlay\n\nUse default line mode for the full command surface while this TUI is iterating."
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

fn extract_tool_call(text: &str) -> Result<Option<ToolCall>> {
    let Some(start) = text.find("```json") else {
        return Ok(None);
    };
    let json_start = start + "```json".len();
    let Some(end) = text[json_start..].find("```") else {
        return Ok(None);
    };
    let json_text = &text[json_start..json_start + end];
    let call = serde_json::from_str::<ToolCall>(json_text.trim())
        .with_context(|| format!("failed to parse tool call JSON: {json_text}"))?;
    Ok(Some(call))
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
- After edits, run focused checks when possible, such as compile checks, relevant selftests, scripts/checkpatch.pl for patch hygiene, or grep-based validation.

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

You can ask to use tools by returning exactly one fenced JSON block:
```json
{"tool":"read_file","path":"src/main.rs"}
```

Available tools:
- list_files: {"tool":"list_files","path":"."}
- read_file: {"tool":"read_file","path":"relative/path"}
- write_file: {"tool":"write_file","path":"relative/path","content":"full file content"}
- run_shell: {"tool":"run_shell","command":"cargo test"}

When you need tool output, return only the tool-call JSON block. After receiving tool results, continue the backporting workflow."#
        .to_string()
}
