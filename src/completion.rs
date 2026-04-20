use rustyline::{
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
    Context, Helper,
};
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

const COMMANDS: &[&str] = &[
    "/chat",
    "/clear",
    "/config",
    "/exit",
    "/exit-shell",
    "/help",
    "/hide-thinking",
    "/list",
    "/memory",
    "/models",
    "/permissions",
    "/provider",
    "/read",
    "/attach",
    "/shell",
    "/show-thinking",
    "/skills",
    "/stop",
    "/terminal",
    "/thinking",
    "/use-model",
    "/write",
];

#[derive(Clone)]
pub struct AgentCompleter {
    workspace: PathBuf,
    shell_mode: bool,
}

impl AgentCompleter {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            shell_mode: false,
        }
    }

    pub fn set_shell_mode(&mut self, shell_mode: bool) {
        self.shell_mode = shell_mode;
    }
}

impl Helper for AgentCompleter {}
impl Highlighter for AgentCompleter {}
impl Validator for AgentCompleter {}

impl Hinter for AgentCompleter {
    type Hint = String;
}

impl Completer for AgentCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];

        if prefix.starts_with('/') {
            return Ok(complete_command_or_arg(&self.workspace, prefix));
        }

        if self.shell_mode {
            return Ok(complete_shell_path(&self.workspace, prefix));
        }

        Ok((pos, Vec::new()))
    }
}

fn complete_command_or_arg(workspace: &Path, prefix: &str) -> (usize, Vec<Pair>) {
    let parts = prefix.split_whitespace().collect::<Vec<_>>();
    if parts.len() <= 1 && !prefix.ends_with(' ') {
        return complete_words(prefix, COMMANDS);
    }

    let command = parts.first().copied().unwrap_or_default();
    match command {
        "/read" | "/write" | "/list" => {
            let arg_start = prefix.rfind(' ').map(|idx| idx + 1).unwrap_or(prefix.len());
            let arg_prefix = &prefix[arg_start..];
            (arg_start, path_pairs(workspace, arg_prefix))
        }
        "/attach" => {
            let arg_start = prefix.rfind(' ').map(|idx| idx + 1).unwrap_or(prefix.len());
            let arg_prefix = &prefix[arg_start..];
            match parts.get(1).copied() {
                Some("file") | Some("image") => (arg_start, path_pairs(workspace, arg_prefix)),
                _ => complete_words(current_word(prefix).1, &["show", "clear", "file", "image"]),
            }
        }
        "/config" => complete_words(current_word(prefix).1, &["show", "reload"]),
        "/memory" => complete_words(current_word(prefix).1, &["show", "add", "clear", "reload"]),
        "/skills" => {
            if parts.len() >= 3 && matches!(parts[1], "enable" | "disable") {
                let arg_start = prefix.rfind(' ').map(|idx| idx + 1).unwrap_or(prefix.len());
                let arg_prefix = &prefix[arg_start..];
                (arg_start, skill_pairs(workspace, arg_prefix))
            } else {
                complete_words(
                    current_word(prefix).1,
                    &["show", "list", "reload", "enable", "disable"],
                )
            }
        }
        "/permissions" => complete_words(
            current_word(prefix).1,
            &["ask", "allow", "deny", "shell", "write"],
        ),
        "/thinking" => complete_words(
            current_word(prefix).1,
            &["auto", "on", "off", "low", "medium", "high", "show", "hide"],
        ),
        "/stop" => complete_words(current_word(prefix).1, &["add", "set", "clear"]),
        _ => (prefix.len(), Vec::new()),
    }
}

fn complete_shell_path(workspace: &Path, prefix: &str) -> (usize, Vec<Pair>) {
    let (start, word) = current_word(prefix);
    (start, path_pairs(workspace, word))
}

fn complete_words(prefix: &str, words: &[&str]) -> (usize, Vec<Pair>) {
    let (start, word) = current_word(prefix);
    let pairs = words
        .iter()
        .filter(|candidate| candidate.starts_with(word))
        .map(|candidate| Pair {
            display: (*candidate).to_string(),
            replacement: (*candidate).to_string(),
        })
        .collect::<Vec<_>>();
    (start, pairs)
}

fn current_word(line: &str) -> (usize, &str) {
    let start = line
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    (start, &line[start..])
}

fn path_pairs(workspace: &Path, raw_prefix: &str) -> Vec<Pair> {
    if raw_prefix.starts_with('/') || raw_prefix.contains("..") {
        return Vec::new();
    }

    let prefix_path = Path::new(raw_prefix);
    let (dir, file_prefix) = if raw_prefix.ends_with('/') {
        (prefix_path, "")
    } else {
        (
            prefix_path.parent().unwrap_or_else(|| Path::new("")),
            prefix_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )
    };

    let read_dir = workspace.join(dir);
    let Ok(entries) = fs::read_dir(&read_dir) else {
        return Vec::new();
    };

    let mut pairs = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(file_prefix) {
                return None;
            }

            let mut replacement = PathBuf::from(dir).join(&name).display().to_string();
            if entry.file_type().ok()?.is_dir() {
                replacement.push('/');
            }
            Some(Pair {
                display: replacement.clone(),
                replacement,
            })
        })
        .collect::<Vec<_>>();

    pairs.sort_by(|a, b| a.display.cmp(&b.display));
    pairs
}

fn skill_pairs(workspace: &Path, raw_prefix: &str) -> Vec<Pair> {
    let skills_dir = workspace.join("skills");
    let Ok(entries) = fs::read_dir(&skills_dir) else {
        return Vec::new();
    };

    let mut pairs = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_stem().and_then(|name| name.to_str())?;
            if !name.starts_with(raw_prefix) {
                return None;
            }
            Some(Pair {
                display: name.to_string(),
                replacement: name.to_string(),
            })
        })
        .collect::<Vec<_>>();

    pairs.sort_by(|a, b| a.display.cmp(&b.display));
    pairs
}

pub fn prompt_text(shell_mode: bool) -> Cow<'static, str> {
    if shell_mode {
        Cow::Borrowed("\x1b[1m\x1b[35mshell>\x1b[0m ")
    } else {
        Cow::Borrowed("\x1b[1m\x1b[32muser>\x1b[0m ")
    }
}
