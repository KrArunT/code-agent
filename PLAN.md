# PLAN

## Summary

- AutoFix now supports master/worker orchestration with isolated worker processes in separate git worktrees.
- The master agent can spawn workers, list them, and inspect their records without sharing workspace state.
- Tool execution now batches multiple fenced JSON tool blocks from one assistant message before the next model turn.
- The UI now shows live progress messages for thinking, tool execution, and worker startup/completion.
- The agent keeps its session plan in this file and rewrites it as the session changes.

## Files Changed

- `AGENT.md`
- `AGENTS.md`
- `PLAN.md`
- `autofix_config.json`
- `README.md`
- `src/agent.rs`
- `src/completion.rs`
- `src/config.rs`
- `src/tools.rs`
- `src/workers.rs`
- `src/ui.rs`
- `memory.json`
- `skills/context-management.md`
- `skills/kernel-backporting.md`
- `skills/plan-mode.md`

## Next Steps

- Use `/agents spawn <name> | <task>` for side tasks that should live in a separate worktree.
- Encourage the model to batch related reads and edits into one assistant turn instead of burning tool rounds one call at a time.
- Keep `PLAN.md` updated after each meaningful state change.
- Run focused validation before reporting a backport or code change complete.
