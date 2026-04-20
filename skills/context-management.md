# context-management

- Keep the active context narrow and only load what is needed for the current task.
- Prefer short summaries over repeated full histories.
- Use `config.json`, `memory.json`, and `skills/` to persist durable context instead of restating it in every prompt.
- Reload context layers when the operator changes config, memory, or the active skill set.
- Prefer one focused tool round at a time and stop adding context once the task can be completed.
