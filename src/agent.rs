use crate::{
    completion::{prompt_text, AgentCompleter},
    config::{list_ollama_models, Config, PermissionMode, ProviderKind, ThinkMode},
    provider::{Message, ProviderClient, Role, StreamEvent},
    tools::{ToolCall, ToolRuntime},
    ui,
};
use anyhow::{Context, Result};
use rustyline::{
    config::{CompletionType, EditMode},
    error::ReadlineError,
    history::DefaultHistory,
    Config as RustylineConfig, Editor,
};
use std::{
    env, io,
    process::{Command, Stdio},
};

pub struct Agent {
    config: Config,
    provider: ProviderClient,
    tools: ToolRuntime,
    messages: Vec<Message>,
    shell_mode: bool,
}

impl Agent {
    pub fn new(config: Config) -> Result<Self> {
        let provider = ProviderClient::new(&config);
        let tools = ToolRuntime::new(
            config.workspace.clone(),
            config.shell_permission(),
            config.write_permission(),
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
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        ui::banner(
            &format!("{:?}", self.config.provider),
            self.provider.model(),
            &self.config.workspace.display().to_string(),
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

            self.messages.push(Message {
                role: Role::User,
                content: input,
            });
            self.respond().await?;
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
            "/help" => {
                println!("{}", ui::help_text());
                Ok(false)
            }
            "/provider" => {
                ui::info(&format!(
                    "provider={:?} model={} base_url={} think={:?} show_thinking={} stops={} permissions=shell:{:?},write:{:?}",
                    self.config.provider,
                    self.provider.model(),
                    self.config.base_url(),
                    self.provider.think(),
                    self.config.show_thinking(),
                    format_stop_sequences(self.provider.stop_sequences()),
                    self.tools.shell_permission(),
                    self.tools.write_permission()
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
                    &format!("{:?}", self.config.provider),
                    self.provider.model(),
                    &self.config.workspace.display().to_string(),
                );
                Ok(false)
            }
            _ => {
                ui::error(&format!("unknown command: {command}"));
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

    async fn respond(&mut self) -> Result<()> {
        for _ in 0..=self.config.max_tool_rounds {
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
    r#"You are a coding agent running inside a local workspace.

You can ask to use tools by returning exactly one fenced JSON block:
```json
{"tool":"read_file","path":"src/main.rs"}
```

Available tools:
- list_files: {"tool":"list_files","path":"."}
- read_file: {"tool":"read_file","path":"relative/path"}
- write_file: {"tool":"write_file","path":"relative/path","content":"full file content"}
- run_shell: {"tool":"run_shell","command":"cargo test"}

Use relative paths only. Explain what you need before requesting risky changes."#
        .to_string()
}
