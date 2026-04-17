use std::io::{self, Write};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const BLUE: &str = "\x1b[34m";
const BG_DARK: &str = "\x1b[48;5;236m";

pub fn banner(provider: &str, model: &str, workspace: &str) {
    println!("{BOLD}{CYAN}coding-agent-rs{RESET}");
    println!("{DIM}provider{RESET} {provider}  {DIM}model{RESET} {model}");
    println!("{DIM}workspace{RESET} {workspace}");
    println!("{DIM}type /help for commands, /exit to quit{RESET}");
    println!();
}

pub fn assistant_start() -> io::Result<()> {
    print!("{BOLD}{CYAN}assistant>{RESET} ");
    io::stdout().flush()
}

pub struct MarkdownStream {
    buffer: String,
    in_code_block: bool,
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_code_block: false,
        }
    }

    pub fn push(&mut self, delta: &str) -> io::Result<()> {
        self.buffer.push_str(delta);
        while let Some(newline) = self.buffer.find('\n') {
            let mut line = self.buffer[..newline].to_string();
            self.buffer.drain(..=newline);
            line.push('\n');
            self.print_line(&line)?;
        }
        io::stdout().flush()
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.print_line(&line)?;
        }
        print!("{RESET}");
        io::stdout().flush()
    }

    fn print_line(&mut self, line: &str) -> io::Result<()> {
        let without_newline = line.trim_end_matches('\n');
        let trimmed = without_newline.trim_start();

        if trimmed.starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                println!("{DIM}```{RESET}");
            } else {
                self.in_code_block = true;
                let lang = trimmed.trim_start_matches("```").trim();
                if lang.is_empty() {
                    println!("{DIM}```{RESET}");
                } else {
                    println!("{DIM}``` {lang}{RESET}");
                }
            }
            return Ok(());
        }

        if self.in_code_block {
            println!("{BG_DARK}{without_newline}{RESET}");
            return Ok(());
        }

        if trimmed.is_empty() {
            println!();
            return Ok(());
        }

        if let Some((level, text)) = markdown_heading(trimmed) {
            let marker = "#".repeat(level);
            println!("{BOLD}{CYAN}{marker} {}{RESET}", render_inline(text.trim()));
            return Ok(());
        }

        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            println!("  {YELLOW}-{RESET} {}", render_inline(item));
            return Ok(());
        }

        if let Some((number, item)) = ordered_list_item(trimmed) {
            println!("  {YELLOW}{number}.{RESET} {}", render_inline(item));
            return Ok(());
        }

        if trimmed.starts_with('>') {
            let quote = trimmed.trim_start_matches('>').trim_start();
            println!("{DIM}| {}{RESET}", render_inline(quote));
            return Ok(());
        }

        println!("{}", render_inline(without_newline));
        Ok(())
    }
}

pub fn render_markdown(text: &str) {
    let mut stream = MarkdownStream::new();
    let _ = stream.push(text);
    let _ = stream.finish();
}

pub fn thinking_start() -> io::Result<()> {
    print!("{DIM}[thinking] ");
    io::stdout().flush()
}

pub fn stream_thinking(delta: &str) -> io::Result<()> {
    print!("{delta}");
    io::stdout().flush()
}

pub fn thinking_end() -> io::Result<()> {
    print!("{RESET}\n{BOLD}{CYAN}answer>{RESET} ");
    io::stdout().flush()
}

pub fn stream_reset() -> io::Result<()> {
    print!("{RESET}");
    io::stdout().flush()
}

pub fn tool_result(result: &str) {
    print!("{BOLD}{MAGENTA}tool>{RESET} ");
    render_markdown(result);
}

pub fn info(message: &str) {
    println!("{BOLD}{YELLOW}info>{RESET} {message}");
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&level) && line[level..].starts_with(' ') {
        Some((level, &line[level + 1..]))
    } else {
        None
    }
}

fn ordered_list_item(line: &str) -> Option<(&str, &str)> {
    let dot = line.find('.')?;
    let number = &line[..dot];
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let rest = line[dot + 1..].strip_prefix(' ')?;
    Some((number, rest))
}

fn render_inline(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    let mut in_code = false;

    while let Some(index) = rest.find('`') {
        output.push_str(&render_emphasis(&rest[..index]));
        if in_code {
            output.push_str(RESET);
        } else {
            output.push_str(BG_DARK);
            output.push_str(BLUE);
        }
        in_code = !in_code;
        rest = &rest[index + 1..];
    }

    output.push_str(&render_emphasis(rest));
    if in_code {
        output.push_str(RESET);
    }
    output
}

fn render_emphasis(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    let mut bold = false;

    while let Some(index) = rest.find("**") {
        output.push_str(&rest[..index]);
        if bold {
            output.push_str(RESET);
        } else {
            output.push_str(BOLD);
        }
        bold = !bold;
        rest = &rest[index + 2..];
    }

    output.push_str(rest);
    if bold {
        output.push_str(RESET);
    }
    output
}

pub fn error(message: &str) {
    println!("{BOLD}{RED}error>{RESET} {message}");
}

pub fn divider() {
    println!("{DIM}------------------------------------------------------------{RESET}");
}

pub fn clear_screen() -> io::Result<()> {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush()
}

pub fn help_text() -> &'static str {
    r#"Commands
  /help              show this help
  /provider          show provider configuration
  /permissions       show shell/write approval modes
  /permissions ask   ask before shell commands and file writes
  /permissions allow allow shell commands and file writes
  /permissions deny  deny shell commands and file writes
  /permissions shell <ask|allow|deny>
  /permissions write <ask|allow|deny>
  /thinking          show thinking mode and trace visibility
  /thinking off      ask Ollama to stop thinking for faster answers
  /thinking on       ask Ollama to return thinking tokens
  /thinking hide     hide thinking trace in the TUI
  /thinking show     show thinking trace in the TUI
  /stop              show configured stop sequences
  /stop add <text>   add a stop sequence
  /stop set <a,b,c>  replace stop sequences
  /stop clear        clear stop sequences
  /models            list locally installed Ollama models
  /use-model <name>  switch to an installed Ollama model
  /list [path]       list workspace files
  /read <path>       read a file
  /write <path>      write a file; finish input with a single '.'
  /shell             enter shell mode
  /shell <command>   run one shell command with confirmation
  !<command>         run one shell command from chat mode
  /terminal          open your real shell in the workspace
  /terminal <shell>  open a specific shell, e.g. /terminal /bin/bash
  /chat              leave shell mode
  /exit-shell        leave shell mode
  /clear             clear the terminal
  /exit              quit

Shortcuts
  Empty input is ignored.
  Assistant responses stream as tokens arrive."#
}
