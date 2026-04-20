# AGENTS.md

Legacy compatibility copy. The canonical file is [`AGENT.md`](AGENT.md).

## Operating Rules

- Keep `PLAN.md` current. Rewrite it in place when the session state changes.
- Use `PLAN.md` for summary, files changed, and next steps.
- Prefer isolated worktrees for feature work when `auto_worktree` is enabled.
- Use isolated worker agents for side tasks that can live in their own worktree and context.
- Keep the master agent focused on coordination, review, and final decisions.
- Keep `memory.json` and `skills/` as durable context, not repeated chat history.
- Use `web_search` for internet lookups. It defaults to DuckDuckGo and should be used for recent upstream context, docs, and external references.
- Do not revert unrelated user changes.
- Preserve kernel backport constraints: minimal patching, semantic conflict resolution, targeted validation.

## Session Flow

- Update `PLAN.md` after startup, config reloads, worktree changes, skill changes, memory changes, and after tool rounds that change the codebase.
- Keep the plan short and actionable.
- When blocked, record the blocker in `PLAN.md` and stop adding speculative steps.
- When delegating to a worker, copy only the scoped task into that worker's prompt and let the worker own its own `PLAN.md`.
