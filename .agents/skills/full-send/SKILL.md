---
name: full-send
description: Commit every repository change as coherent commits and push the resulting history directly to origin/main.
disable-model-invocation: true
---

# Full Send

Treat invocation as authorization to commit all non-ignored worktree changes and push them directly to `origin/main`. Proceed without an additional confirmation prompt.

1. Find the repository root and inspect `git status`, staged and unstaged diffs, untracked files, recent commit style, current branch, and remotes. Abort with a clear report if a merge/rebase is unresolved, `origin` or `origin/main` is unavailable, or any change appears to contain credentials, private keys, tokens, or generated secrets.
2. Fetch `origin main`. Account for every non-ignored changed and untracked file. Preserve the work as found: make only small corrections required for validation or commit integrity, and never discard changes.
3. Run the repository's relevant formatting, static checks, and tests. Fix failures caused by the pending work and rerun until green. Report and stop on failures that cannot be safely fixed.
4. Partition all changes into the smallest coherent commits that leave the repository valid. Order prerequisite/refactor commits before dependent behavior. Keep implementation with its tests and directly coupled documentation; do not split commits merely by file type. Match the repository's existing commit-message style.
5. Stage each partition deliberately, inspect its staged diff, and commit it. Continue until `git status --porcelain` is empty. Include this skill and every other non-ignored pending change.
6. Rebase the resulting local history onto the latest `origin/main` when needed. Resolve conflicts by preserving both upstream intent and the committed work, then rerun affected validation. Never force-push.
7. Push with `git push origin HEAD:main`. If rejected because `main` advanced, fetch, rebase, revalidate, and retry once.
8. Verify local `HEAD` equals `origin/main` and the worktree is clean. Report the commits pushed and validation performed.
