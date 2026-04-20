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
const BANNER_WIDTH: usize = 54;
const BANNER_INNER_WIDTH: usize = 50;

pub fn banner(
    title: &str,
    subtitle: &str,
    provider: &str,
    model: &str,
    workspace: &str,
    access: &str,
    onboarding: &[String],
    tip: &str,
) {
    let title = center_banner_text(title, BANNER_INNER_WIDTH);
    let subtitle = center_banner_text(subtitle, BANNER_INNER_WIDTH);
    println!("{}", banner_border('╔', '═', '╗', BANNER_WIDTH));
    println!("{}", banner_empty_line(BANNER_WIDTH));
    println!("{BOLD}{CYAN}║ {title} ║{RESET}");
    println!("{BOLD}{CYAN}║ {subtitle} ║{RESET}");
    println!("{}", banner_empty_line(BANNER_WIDTH));
    println!("{}", banner_border('╚', '═', '╝', BANNER_WIDTH));
    println!(
        "{DIM}provider{RESET} {provider}  {DIM}model{RESET} {model}  {DIM}access{RESET} {access}"
    );
    println!("{DIM}workspace{RESET} {workspace}");
    if access == "full-system" {
        println!("{BOLD}{RED}warning>{RESET} full system access is enabled: absolute paths, path escapes, shell, and writes are allowed");
    }
    if onboarding.is_empty() {
        println!("{BOLD}{YELLOW}onboarding>{RESET} /help commands  /models local models  /search web search  /permissions safety  /terminal real shell  /exit quit");
    } else {
        for (idx, line) in onboarding.iter().enumerate() {
            if idx == 0 {
                println!("{BOLD}{YELLOW}onboarding>{RESET} {line}");
            } else {
                println!("{DIM}           {RESET} {line}");
            }
        }
    }
    println!("{DIM}tip{RESET} {tip}");
    println!();
}

pub fn assistant_start() -> io::Result<()> {
    print!("{BOLD}{CYAN}autofix>{RESET} ");
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
    print!("{RESET}\n{BOLD}{CYAN}autofix>{RESET} ");
    io::stdout().flush()
}

pub fn stream_reset() -> io::Result<()> {
    print!("{RESET}");
    io::stdout().flush()
}

pub fn tool_start(label: &str) {
    println!("{BOLD}{MAGENTA}tool>{RESET} {BOLD}{MAGENTA}{label}{RESET}");
}

pub fn tool_result(label: &str, result: &str) {
    println!("{BOLD}{MAGENTA}tool>{RESET} {BOLD}{MAGENTA}{label}{RESET}");
    render_markdown(result);
}

pub fn info(message: &str) {
    println!("{BOLD}{BLUE}info>{RESET} {message}");
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
    println!("{DIM}────────────────────────────────────────────────────────────{RESET}");
}

fn fit_banner_text(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in text.chars().take(max_chars) {
        output.push(ch);
    }
    output
}

fn center_banner_text(text: &str, width: usize) -> String {
    let text = fit_banner_text(text, width);
    let len = text.chars().count();
    if len >= width {
        return text;
    }

    let left = (width - len) / 2;
    let right = width - len - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn banner_border(left: char, fill: char, right: char, width: usize) -> String {
    format!(
        "{BOLD}{CYAN}{left}{}{right}{RESET}",
        fill.to_string().repeat(width.saturating_sub(2))
    )
}

fn banner_empty_line(width: usize) -> String {
    format!(
        "{BOLD}{CYAN}║{}║{RESET}",
        " ".repeat(width.saturating_sub(2))
    )
}

pub fn clear_screen() -> io::Result<()> {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush()
}

pub fn help_text() -> &'static str {
    r#"Commands
  /help              show this help
  /config            show current config file state
  /config reload     reload autofix_config.json from disk
  /memory            show memory notes
  /memory add <text> add a durable memory note
  /memory clear      clear all memory notes
  /memory reload     reload memory.json from disk
  /skills            show active skills
  /skills list       list available skills on disk
  /skills reload     reload active skill files
  /skills enable <name> add a skill to the active set
  /skills disable <name> remove a skill from the active set
  /worktree          show git worktree state
  /worktree list     list worktrees in porcelain format
  /worktree auto     create and switch to an auto worktree if needed
  /worktree add <path> [branch] create a new worktree and switch to it
  /worktree switch <path> switch the active workspace to an existing worktree
  /worktree remove <path> remove a worktree
  /worktree prune    prune stale worktree metadata
  /agents            list isolated worker agents
  /agents spawn <name> | <task> spawn a worker in a fresh worktree
  /agents read <id>  show worker status and file paths
  /session           show current session state
  /session list      list saved sessions
  /session history   show current command history
  /session save      persist the current session record
  /session new       start a fresh session
  /session resume <id> resume a saved session into a fresh current session
  /history           show current command history
  /interrupt         stop the active model stream and save the interrupted session
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
  /attach file <path> append a file to the next prompt
  /attach image <path> append an image reference to the next prompt
  /attach show       show queued prompt attachments
  /attach clear      clear queued prompt attachments
  /search <query>    search the web with DuckDuckGo
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
