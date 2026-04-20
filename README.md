# coding-agent-rs

A Rust terminal coding agent with local-first Ollama support, streaming output, workspace tools, approval controls, shell modes, and multiple LLM provider adapters.

## Quick Map

- [Build](#build)
- [Docs](#docs)
- [Install](#install)
- [Config](#config)
- [Session History](#session-history)
- [Session Interruption](#session-interruption)
- [Memory And Skills](#memory-and-skills)
- [Memory Strategy](#memory-strategy)
- [Worktrees](#worktrees)
- [Workers](#workers)
- [Quick Start](#quick-start)
- [Banner](#banner)
- [Providers](#providers)
- [System Prompt](#system-prompt)
- [Commands](#commands)
- [Approval Modes](#approval-modes)
- [Agent Tool Calls](#agent-tool-calls)
- [Notes](#notes)

## Highlights

- Local Ollama by default, with model discovery from `/api/tags`.
- Provider adapters for OpenAI-compatible APIs, Anthropic, Gemini, OpenRouter, Ollama, and custom OpenAI-compatible gateways.
- Streaming responses for all provider paths.
- Markdown rendering for streamed assistant/tool output: headings, lists, quotes, inline code, bold text, and fenced code blocks.
- Live progress updates in the terminal and TUI status panel while the agent thinks, calls tools, or runs workers.
- Tab completion for slash commands, command arguments, and workspace paths.
- Optional `ratatui` full-screen mode behind `--tui` with help overlay, scrollback, and input history.
- Configurable startup banner and onboarding help in both line mode and `--tui`.
- Workspace tools for file listing, file reads, file writes, and shell execution.
- Persistent memory notes and reusable skills loaded into the system prompt.
- Persistent session command history and resumable task snapshots.
- Git worktree management for multi-branch kernel work.
- Automatic worktree creation for feature sessions.
- Master/worker orchestration with isolated worker processes in separate worktrees.
- Workspace instructions in `AGENT.md` and live session state in `PLAN.md`.
- Approval modes for shell commands and writes: `ask`, `allow`, or `deny`.
- Full system access mode for trusted sessions with absolute paths, workspace escapes, shell, and writes enabled.
- Shell runner mode plus full terminal passthrough mode.
- Ollama thinking controls and stop sequences.

## Common Workflows

- Start the agent: `cargo run`
- Resume a previous task: `cargo run -- --resume-session <session-id>`
- Interrupt a long response: `/interrupt` or `Ctrl-C`
- Inspect live state: `/session`, `/history`, `/provider`, `/memory`, `/skills`
- Isolate feature work: `/worktree auto` or `/agents spawn <name> | <task>`
- Build docs: `./scripts/build-docs.sh`

## Build

```bash
cargo build
```

## Docs

Build HTML and PDF documentation from the README with Pandoc:

```bash
./scripts/build-docs.sh
```

By default the script writes:

- `docs/build/AutoFix.html`
- `docs/build/AutoFix.pdf`

If you want a different source file or output directory, override the script inputs:

```bash
DOC_SOURCE=README.md DOC_TITLE=autofix OUT_DIR=docs/build ./scripts/build-docs.sh
```

The script expects `pandoc` and a LaTeX engine such as `xelatex` to be installed locally. The repo does not vendor those tools.

## Install

One-line install:

```bash
curl -fsSL https://raw.githubusercontent.com/KrArunT/code-agent/main/install.sh | bash
```

The script bootstraps Rust with `rustup` if `cargo` is missing, then uses `cargo install --git` under the hood. You can override the repository, branch, or install root with environment variables:

```bash
REPO_URL=https://github.com/KrArunT/code-agent.git BRANCH=main INSTALL_ROOT=$HOME/.local curl -fsSL https://raw.githubusercontent.com/KrArunT/code-agent/main/install.sh | bash
```

The installed binary is named `autofix`.

## Config

The agent reads `autofix_config.json` from the current working directory when it starts, if the file exists. It also watches that file’s modified time and auto-reloads the saved config before the next prompt or tool round when the file changes. Use `/config reload` for an explicit refresh.

The included [`autofix_config.json`](autofix_config.json) is a starter profile you can edit for future runs. It is a plain JSON file with the same core fields as the CLI: provider, workspace, permissions, thinking mode, banner text, onboarding lines, the autonomy toggle, `auto_worktree`, and the tool-loop budget controls.

Set `"autonomous": true` to raise the tool-loop budget for hands-off execution. Set `"unlimited_tool_rounds": true` when you want the loop to keep going until the model explicitly returns `final`, `blocked`, or `needs_worker`.

## Session History

Each run gets a persistent session record under the repo control directory. It stores the command history and a resumable message snapshot so you can pick up a task later.

Use `/session` to inspect the active session:

```text
/session show
/session list
/session history
/session save
/session new
/session resume <id>
/history
```

To start a fresh process from a previous session, pass the session id back on the CLI:

```bash
cargo run -- --resume-session <session-id>
```

`/session resume <id>` also switches the live process to a new session cloned from the saved one, so you can continue immediately and keep the old record intact.

### Session Interruption

You can stop an active model stream and keep the current session state with either:

```text
/interrupt
Ctrl-C
```

When interrupted, the agent:

- stops the in-flight model request
- saves the current session record
- writes an interruption note into `PLAN.md`
- leaves the current session resumable through `/session resume <id>` or `--resume-session <id>`

This is the right way to pause long backports, large refactors, or work that should be picked up later without losing the current command history.

## Memory And Skills

The agent loads `memory.json` and the `skills/` directory from the workspace on startup. These are merged into the system prompt so repeated context can stay out of the chat transcript.
It also auto-initializes `AGENT.md` from `AGENTS.md` if needed, reads `AGENT.md` and `PLAN.md` from the workspace root, and rewrites `PLAN.md` as the session changes.

The sample files in the repo are:

- [`memory.json`](memory.json) for persistent notes.
- [`skills/context-management.md`](skills/context-management.md) for context hygiene.
- [`skills/kernel-backporting.md`](skills/kernel-backporting.md) for backport-specific instructions.
- [`skills/plan-mode.md`](skills/plan-mode.md) for short, explicit decomposition before execution.

Use `/memory` to inspect or update the memory file during a session:

```text
/memory show
/memory add keep context small
/memory clear
/memory reload
```

Use `/skills` to inspect or switch active skills:

```text
/skills show
/skills list
/skills enable context-management
/skills disable kernel-backporting
/skills reload
```

The active skill list is stored in `autofix_config.json` under `active_skills`, and `/config reload` re-reads `autofix_config.json`, `memory.json`, and the active skills without restarting the agent.

### Memory Strategy

The current design keeps memory simple and deterministic:

- `memory.json` stores durable notes that should survive across runs.
- `PLAN.md` stores live session state, summary, files changed, and next steps.
- `AGENT.md` stores repo-local operating rules.
- `skills/` stores reusable task-specific instruction blocks.
- `sessions/` stores command history and resumable message snapshots.

For this repo, that is usually enough.

You do **not** need a vector database yet if the goal is:

- remembering the active task
- carrying forward local instructions
- resuming recent sessions
- keeping worktree-local context isolated

A vector database becomes useful when you need semantic recall over a large corpus of past tasks, logs, patches, or external documents. Until then, a simple index is lower risk and easier to debug.

Recommended next step if memory grows:

- add a lightweight keyword or tag index over `memory.json`, `PLAN.md`, and session summaries
- keep the raw source files as the source of truth
- add embeddings/vector search only after the keyword index becomes too weak for retrieval

## Worktrees

When `auto_worktree` is enabled, `autofix` creates a fresh git worktree on startup if the session begins at the repository root. That keeps feature work isolated from the original tree.

Use `/worktree` to inspect or change the active git worktree:

```text
/worktree auto
/worktree status
/worktree list
/worktree add ../worktrees/backport-fix backport-fix
/worktree switch ../worktrees/backport-fix
/worktree remove ../worktrees/backport-fix
/worktree prune
```

`/worktree auto` checks or creates the default isolated worktree for the current repo. `/worktree add` creates a new worktree, switches the agent to it, reloads workspace memory and skills when they live under the workspace, and saves the updated workspace back to `autofix_config.json`.

## Workers

The master agent can delegate isolated subproblems to worker agents.

Workers run as separate `autofix` processes in their own git worktrees, each with its own copied context files:

- `AGENT.md`
- `PLAN.md`
- `memory.json`
- `skills/`
- `autofix_config.json`

Use the agent command surface to manage them:

```text
/agents
/agents spawn parser-fix | isolate the parser logic and patch the failing branch
/agents read <id>
```

Workers are hard-isolated from the master session at the filesystem level. They do not share the same workspace path, and the master keeps control through the shared worker registry under the repository’s git common directory.

If you launch a worker manually, use:

```bash
autofix --role worker --config-file /path/to/worktree/autofix_config.json --task-file /path/to/task.md --worker-id worker-123
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

Spawn a worker directly:

```bash
autofix
# then in the agent UI:
/agents spawn backport-fix | isolate the backport into its own worktree
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
Both modes start with an onboarding block that points you at `/help`, `/models`, `/search`, `/permissions`, `/terminal`, and the backporting workflow.

TUI navigation:

- `?` toggles the help overlay.
- `PgUp` and `PgDn` scroll the transcript.
- `Home` and `End` jump to the top or bottom of the transcript.
- `Up` and `Down` browse command history when the input box is empty.
- Mouse wheel scrolls the transcript.
- Drag or click the scrollbar to reposition the transcript.
- The bottom hint line shows the current shortcut summary inside the full-screen UI.

| Area | Command | Purpose |
| --- | --- | --- |
| Core | `/help` | Show command help. |
| Core | `/provider` | Show provider, model, thinking, stop, and permission state. |
| Core | `/models` | List installed Ollama models. |
| Core | `/use-model <name>` | Switch to an installed Ollama model. |
| Core | `/clear` | Clear and redraw the terminal header. |
| Core | `/exit` | Exit the agent. |
| Workspace | `/list [path]` | List files under the workspace. |
| Workspace | `/read <path>` | Print a file. |
| Workspace | `/write <path>` | Write content until a line containing only `.`. |
| Workspace | `/config` | Show the current config file state. |
| Workspace | `/config reload` | Reload `autofix_config.json` from disk. |
| Workspace | `/memory` | Show and edit persistent memory notes. |
| Workspace | `/skills` | Show and edit active skills. |
| Workspace | `/attach file <path>` | Queue a file to prepend to the next prompt. |
| Workspace | `/attach image <path>` | Queue an image reference and metadata to prepend to the next prompt. |
| Workspace | `/attach show` | Show queued prompt attachments. |
| Workspace | `/attach clear` | Clear queued prompt attachments. |
| Workspace | `/search <query>` | Search the web with DuckDuckGo and open a picker in TUI. |
| Session | `/session` | Show the active session state. |
| Session | `/session list` | List saved sessions. |
| Session | `/session history` | Show the active command history. |
| Session | `/session save` | Persist the active session record. |
| Session | `/session new` | Start a fresh session. |
| Session | `/session resume <id>` | Resume a saved session into a fresh current session. |
| Session | `/history` | Show the active command history. |
| Session | `/interrupt` | Stop the active model stream and save the interrupted session. |
| Worktree | `/worktree` | Show and edit git worktree state. |
| Shell | `/shell` | Enter command-runner shell mode. |
| Shell | `/shell <command>` | Run one command. |
| Shell | `!<command>` | Run one shell command from chat mode. |
| Shell | `/chat` or `/exit-shell` | Leave shell mode. |
| Shell | `/terminal` | Open your real shell in the workspace. |
| Shell | `/terminal <shell>` | Open a specific shell, for example `/terminal /bin/bash`. |
| Permissions | `/permissions` | Show shell/write approval modes. |
| Permissions | `/permissions ask` | Ask before shell commands and writes. |
| Permissions | `/permissions allow` | Allow shell commands and writes. |
| Permissions | `/permissions deny` | Deny shell commands and writes. |
| Permissions | `/permissions shell <ask|allow|deny>` | Change shell approval only. |
| Permissions | `/permissions write <ask|allow|deny>` | Change write approval only. |
| Access | `--full-system-access` | Enable broad local access and allow absolute/outside-workspace paths. |
| Access | `/provider` or startup banner | Show `access=full-system` when full system access is enabled. |
| Thinking | `/thinking` | Show thinking mode and trace visibility. |
| Thinking | `/thinking off` | Ask Ollama to stop thinking for faster answers. |
| Thinking | `/thinking on` | Ask Ollama to return thinking tokens. |
| Thinking | `/thinking hide` | Hide thinking trace in the TUI. |
| Thinking | `/thinking show` | Show thinking trace in the TUI. |
| Thinking | `/stop` | Show stop sequences. |
| Thinking | `/stop add <text>` | Add a stop sequence. |
| Thinking | `/stop set <a,b,c>` | Replace stop sequences. |
| Thinking | `/stop clear` | Clear stop sequences. |

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

The model uses a Codex-style JSON turn protocol:

```json
{"type":"tool_calls","calls":[{"tool":"read_file","path":"src/main.rs"},{"tool":"list_files","path":"src"}]}
```

```json
{"type":"final","summary":"Done. The patch compiles and the remaining work is documented in PLAN.md."}
```

Supported tools inside `calls`:

- `list_files`: `{ "tool": "list_files", "path": "." }`
- `read_file`: `{ "tool": "read_file", "path": "src/main.rs" }`
- `write_file`: `{ "tool": "write_file", "path": "notes.txt", "content": "..." }`
- `run_shell`: `{ "tool": "run_shell", "command": "cargo test" }`
- `web_search`: `{ "tool": "web_search", "query": "linux kernel backporting", "max_results": 5 }`
- `spawn_worker`: `{ "tool": "spawn_worker", "name": "parser", "task": "..." }`
- `list_workers`: `{ "tool": "list_workers" }`
- `read_worker`: `{ "tool": "read_worker", "id": "worker-id" }`

Tool paths are workspace-relative. Absolute paths and parent-directory escapes are rejected.
The agent executes every tool in the `calls` array in order, then sends the results back as one tool-result turn. That keeps the loop closer to Codex-style function calling and reduces round churn.

The model can also use `{"type":"final","summary":"..."}`, `{"type":"blocked","reason":"..."}`, or `{"type":"needs_worker","task":"..."}` as explicit completion states.

## Notes

- `/shell` captures command output and feeds it through the TUI renderer.
- `/terminal` hands control to a real shell with inherited terminal I/O, then returns when the shell exits.
- `/search` uses DuckDuckGo HTML search results by default and returns compact title, URL, and snippet output.
- Some Ollama thinking models may still emit inline `<think>...</think>` text. The TUI filters those blocks from conversation history and can hide them from display with `/thinking hide` or `--hide-thinking`.
