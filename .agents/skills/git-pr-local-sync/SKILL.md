---
name: git-pr-local-sync
description: Use for end-to-end Git delivery: commit and push changes, create or merge GitHub PRs, and reconcile local branches after merge.
---

# Git PR and Local Sync

Use this skill for end-to-end code delivery involving local commits, remote branches, GitHub pull requests, merges, or post-merge synchronization. The project workflow document is the detailed policy source; read it before acting: [PR workflow](../../guides/push-workflow.md)

## Environment

- The repository directory (including `.git`) is read-only inside the sandbox. `commit`, `push`, `branch`, `checkout` and `add` must run **outside the sandbox (escalated)**.
- Inside the sandbox, TLS reports `schannel ... SEC_E_NO_CREDENTIALS` and `gh auth status` **falsely reports an invalid token**. Any credential conclusion must be re-checked outside the sandbox before it is trusted.

## Local Git vs. GitHub

- Use local Git for inspection, diffing, staging, committing, pushing, fetching, switching and pulling.
- Use an already-authorized GitHub interface (connector, or an installed and authenticated CLI) to read repository/PR/CI state and to merge. It never updates the local checkout.
- Never search for or expose credentials, and never claim a remote action succeeded without reading its result.

## Workflow

1. Confirm repository root, remote, current branch, default branch and worktree status before changing anything; preserve pre-existing changes.
2. Review the full diff, run the validation the project requires, stage only intended files, inspect the staged diff, commit, then verify the new commit and worktree state.
3. Push with `scripts/push.ps1 -Branch <name> [-ExpectedSha <sha>]` and verify the local commit matches its upstream.
4. Create or inspect the PR with the intended head and base; review diff, checks, reviews, mergeability and merge policy. Do not merge with failing checks, unresolved required reviews, unexpected changes or an uncertain base.
5. Merge only when the user asked for it or clearly authorized it. Refresh the PR state first; follow the repository's allowed merge method. If permission or policy blocks it, report the exact blocker.
6. After merge, confirm the PR is merged and record its merge commit; then fetch, switch to the default branch and fast-forward only. Verify `HEAD` equals `origin/<default-branch>` and the worktree is clean.
7. For core plus consumer changes, deliver and merge core first; then update each consumer repository, lockfile and generated bindings separately, and validate and deliver each consumer PR independently.
8. Report repository, branch and commit SHA, PR URL and merged state, merge SHA, local/remote default-branch equality, worktree state, validation results and anything incomplete.

## Stacked branches and SHA assertions

- Delivering several branches: never loop with `HEAD:refs/heads/<name>`. Check out or address each branch explicitly, assert its SHA with `git rev-parse refs/heads/<name>`, then push that exact ref.
- Push only with an explicit refspec (`refs/heads/<branch>:refs/heads/<branch>`). Never use the `HEAD:` form.
- If a PR's commit set does not match what was intended, stop and ask before pushing anything else.

## Direct pushes to main

Direct `main` pushes are the exception, allowed only when **all** hold:

1. The user explicitly requested a direct `main` push in the current task.
2. Every changed file is `*.md` — the guard accepts nothing else in a direct push; comment-only or line-ending-only edits to other files still require a PR.
3. No `.github/workflows/**`, `Cargo.toml`, `Cargo.lock`, `src/**`, `crates/**` or `web/src/**` is touched — a one-line script change still requires a PR.
4. Local checks pass: `git diff --check`; plus `bash scripts/check-agent-docs.sh` when repository docs changed.
5. The push goes through `scripts/push.ps1 -Branch main -AllowMain` (fetch first, require local `main == origin/main`, stop if the remote moved).
6. After pushing, read the CI result for the new head; on failure revert with a **new commit** (never force) and say so in the report.

Always forbidden: `--force`, bypassing protection rules, and continuing while CI is unconfirmed. A direct push to `main` must report the old SHA, new SHA and CI conclusion.

## Stop and ask

Pause and offer clear options when the repository, branch, PR base or merge policy is ambiguous; when existing changes cannot be safely separated from this task; or when resolving divergence would overwrite local commits, rewrite shared history, delete unique work or bypass protections. Do not use `reset --hard`, `branch -D` or a force push as a synchronization shortcut.

Treat explicit user instructions as controlling. A request to commit or push alone does not authorize creating a PR; a request to create a PR alone does not authorize merging it. Do not repeat questions the user already answered in the active task.