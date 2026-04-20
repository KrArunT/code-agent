# kernel-backporting

Backport Linux kernel changes into an older target tree with minimal drift.

## Workflow

1. Identify the exact upstream commit, target branch, and dirty state.
2. Read the upstream commit message, touched files, and prerequisite commits.
3. Inspect the target-tree code around every affected symbol before editing.
4. Adapt the change to the target APIs instead of forcing a mechanical apply.
5. Keep the patch small, local, and style-compliant.
6. Verify with the narrowest useful check available in the current environment.

## Rules

- Preserve target-tree APIs, locking, and error handling.
- Prefer semantic conflict resolution over patch replay.
- Do not introduce unrelated refactors, cleanups, or renames.
- Do not discard unrelated local changes.
- Call out any missing prerequisite commit or helper.

## Verification

- Prefer compile checks, targeted selftests, or small grep-based validation.
- If verification cannot run, say why and give the next concrete command.

## Reporting

- State what changed, why it changed, and what was verified.
- Name the files and symbols that matter.
- If the backport is incomplete, report the blocker directly.
