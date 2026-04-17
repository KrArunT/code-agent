# coding-agent-rs

A Rust terminal coding agent with local-first Ollama support, streaming output, workspace tools, approval controls, shell modes, and multiple LLM provider adapters.

## Features

- Local Ollama by default, with model discovery from `/api/tags`.
- Provider adapters for OpenAI-compatible APIs, Anthropic, Gemini, OpenRouter, Ollama, and custom OpenAI-compatible gateways.
- Streaming responses for all provider paths.
- Markdown rendering for streamed assistant/tool output: headings, lists, quotes, inline code, bold text, and fenced code blocks.
- Tab completion for slash commands, command arguments, and workspace paths.
- Workspace tools for file listing, file reads, file writes, and shell execution.
- Approval modes for shell commands and writes: `ask`, `allow`, or `deny`.
- Shell runner mode plus full terminal passthrough mode.
- Ollama thinking controls and stop sequences.

## Build

```bash
cargo build
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

Disable visible/thinking-model traces and allow shell/write actions without prompts:

```bash
cargo run -- --think off --hide-thinking --approval-mode allow
```

Use stricter permissions:

```bash
cargo run -- --approval-mode ask --shell-approval deny --write-approval ask
```

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
