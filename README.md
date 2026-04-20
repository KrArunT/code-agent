# coding-agent-rs

A Rust terminal coding agent with local-first Ollama support, streaming output, workspace tools, approval controls, shell modes, and multiple LLM provider adapters.

## Features

- Local Ollama by default, with model discovery from `/api/tags`.
- Provider adapters for OpenAI-compatible APIs, Anthropic, Gemini, OpenRouter, Ollama, and custom OpenAI-compatible gateways.
- Streaming responses for all provider paths.
- Markdown rendering for streamed assistant/tool output: headings, lists, quotes, inline code, bold text, and fenced code blocks.
- Tab completion for slash commands, command arguments, and workspace paths.
- Optional `ratatui` full-screen mode behind `--tui` with help overlay, scrollback, and input history.
- Configurable startup banner and onboarding help in both line mode and `--tui`.
- Workspace tools for file listing, file reads, file writes, and shell execution.
- Approval modes for shell commands and writes: `ask`, `allow`, or `deny`.
- Full system access mode for trusted sessions with absolute paths, workspace escapes, shell, and writes enabled.
- Shell runner mode plus full terminal passthrough mode.
- Ollama thinking controls and stop sequences.

## Build

```bash
cargo build
```

## Install

One-line install:

```bash
curl -fsSL https://raw.githubusercontent.com/KrArunT/code-agent/main/install.sh | bash
```

The script uses `cargo install --git` under the hood. You can override the repository, branch, or install root with environment variables:

```bash
REPO_URL=https://github.com/KrArunT/code-agent.git BRANCH=main INSTALL_ROOT=$HOME/.local curl -fsSL https://raw.githubusercontent.com/KrArunT/code-agent/main/install.sh | bash
```

## Quick Start

Run with local Ollama and auto-pick a local model:

```bash
cargo run
```

Use a specific Ollama model:

```bash
cargo run -- --model gemma3:270m
```

Start the full-screen Ratatui interface:

```bash
cargo run -- --tui --model gemma3:270m
```

Disable visible/thinking-model traces and allow shell/write actions without prompts:

```bash
cargo run -- --think off --hide-thinking --approval-mode allow
```

Use stricter permissions:

```bash
cargo run -- --approval-mode ask --shell-approval deny --write-approval ask
```

Run with full system access for a trusted local session:

```bash
cargo run -- --full-system-access
```

`--full-system-access` permits absolute paths, paths outside the workspace, shell commands, and writes without approval prompts. Use it only when you intend to give the agent broad local-machine access.

If you want the stricter workspace-scoped mode, omit `--full-system-access`. The UI labels the current access level as `workspace` or `full-system` so it stays visible during the session.

## Banner

The startup banner is configurable from CLI arguments or environment variables. The defaults are `AutoFix` and `An autonomous coding agent`:

```bash
cargo run -- \
  --banner-title "kernel-backport-bot" \
  --banner-subtitle "linux patch migration helper" \
  --banner-tip "start with the upstream commit SHA" \
  --banner-onboarding "/help commands" \
  --banner-onboarding "/terminal real shell"
```

The same banner settings are used by the line-mode startup banner, the `--clear` redraw, and the Ratatui header. If you omit the onboarding flags, the default onboarding lines are used.

## Providers

Ollama is the default provider:

```bash
cargo run -- --provider ollama --model lfm2.5-thinking:latest
```

OpenAI:

```bash
OPENAI_API_KEY=... cargo run -- --provider openai --model gpt-4.1
```

Anthropic:

```bash
ANTHROPIC_API_KEY=... cargo run -- --provider anthropic --model claude-3-5-sonnet-latest
```

Gemini:

```bash
GEMINI_API_KEY=... cargo run -- --provider gemini --model gemini-1.5-pro
```

OpenRouter:

```bash
OPENROUTER_API_KEY=... cargo run -- --provider openrouter --model anthropic/claude-3.5-sonnet
```

Any OpenAI-compatible provider:

```bash
CUSTOM_API_KEY=... cargo run -- \
  --provider custom-openai \
  --base-url https://api.example.com/v1/chat/completions \
  --model provider-model-name
```

## System Prompt

By default, the built-in system prompt configures the agent as an autonomous Linux kernel backporting agent. It is optimized for inspecting upstream commits, adapting patches to a target kernel tree, resolving conflicts semantically, preserving kernel style, and running focused verification.

Pass a system prompt directly:

```bash
cargo run -- --system "You are a concise Rust coding agent. Prefer small, safe patches."
```

Or use an environment variable:

```bash
AGENT_SYSTEM_PROMPT="You are a strict coding assistant." cargo run
```

For a longer prompt:

```bash
cargo run -- --system "$(cat system-prompt.txt)"
```

## Commands

Press `Tab` to complete slash commands and workspace paths.

The default line-mode UI supports the full command set below. The `--tui` full-screen mode currently supports chat plus `/help`, `/provider`, `/models`, `/use-model`, `/thinking`, `/clear`, and `/exit`; line mode remains available for shell, terminal passthrough, writes, and the full command surface while the Ratatui path is iterating.
Both modes start with an onboarding block that points you at `/help`, `/models`, `/permissions`, `/terminal`, and the backporting workflow.

TUI navigation:

- `?` toggles the help overlay.
- `PgUp` and `PgDn` scroll the transcript.
- `Home` and `End` jump to the top or bottom of the transcript.
- `Up` and `Down` browse command history when the input box is empty.
- Mouse wheel scrolls the transcript.
- Drag or click the scrollbar to reposition the transcript.
- The bottom hint line shows the current shortcut summary inside the full-screen UI.

Core commands:

- `/help` shows command help.
- `/provider` shows provider, model, thinking, stop, and permission state.
- `/models` lists installed Ollama models.
- `/use-model <name>` switches to an installed Ollama model.
- `/clear` clears and redraws the terminal header.
- `/exit` exits.

Workspace commands:

- `/list [path]` lists files under the workspace.
- `/read <path>` prints a file.
- `/write <path>` writes content until a line containing only `.`.
- `/attach file <path>` queues a file to prepend to the next prompt.
- `/attach image <path>` queues an image reference and metadata to prepend to the next prompt.
- `/attach show` shows queued prompt attachments.
- `/attach clear` clears queued prompt attachments.

Shell commands:

- `/shell` enters command-runner shell mode.
- `/shell <command>` runs one command.
- `!<command>` runs one shell command from chat mode.
- `/chat` or `/exit-shell` leaves shell mode.
- `/terminal` opens your real shell in the workspace.
- `/terminal <shell>` opens a specific shell, for example `/terminal /bin/bash`.

Permissions:

- `/permissions` shows shell/write approval modes.
- `/permissions ask` asks before shell commands and writes.
- `/permissions allow` allows shell commands and writes.
- `/permissions deny` denies shell commands and writes.
- `/permissions shell <ask|allow|deny>` changes shell approval only.
- `/permissions write <ask|allow|deny>` changes write approval only.

Full system access:

- `--full-system-access` enables broad local access.
- In this mode, file tools can read/write absolute paths and paths outside the workspace.
- Shell and write permissions are treated as `allow`.
- Startup status and `/provider` show `access=full-system`.
- The same access label appears in the Ratatui status panel and startup banner.

Ollama thinking and stops:

- `/thinking` shows thinking mode and trace visibility.
- `/thinking off` asks Ollama to stop thinking for faster answers.
- `/thinking on` asks Ollama to return thinking tokens.
- `/thinking hide` hides thinking trace in the TUI.
- `/thinking show` shows thinking trace in the TUI.
- `/stop` shows stop sequences.
- `/stop add <text>` adds a stop sequence.
- `/stop set <a,b,c>` replaces stop sequences.
- `/stop clear` clears stop sequences.

## Approval Modes

CLI flags:

```bash
cargo run -- --approval-mode ask
cargo run -- --approval-mode deny
cargo run -- --shell-approval allow --write-approval ask
```

Legacy compatibility flags still work:

```bash
cargo run -- --dangerously-allow-shell
cargo run -- --auto-write
```

Mode behavior:

- `ask`: prompt before risky actions.
- `allow`: run without prompting.
- `deny`: block without prompting.

## Agent Tool Calls

The model can request a tool call by returning one fenced JSON block:

```json
{"tool":"read_file","path":"src/main.rs"}
```

Supported tools:

- `list_files`: `{ "tool": "list_files", "path": "." }`
- `read_file`: `{ "tool": "read_file", "path": "src/main.rs" }`
- `write_file`: `{ "tool": "write_file", "path": "notes.txt", "content": "..." }`
- `run_shell`: `{ "tool": "run_shell", "command": "cargo test" }`

Tool paths are workspace-relative. Absolute paths and parent-directory escapes are rejected.

## Notes

- `/shell` captures command output and feeds it through the TUI renderer.
- `/terminal` hands control to a real shell with inherited terminal I/O, then returns when the shell exits.
- Some Ollama thinking models may still emit inline `<think>...</think>` text. The TUI filters those blocks from conversation history and can hide them from display with `/thinking hide` or `--hide-thinking`.
