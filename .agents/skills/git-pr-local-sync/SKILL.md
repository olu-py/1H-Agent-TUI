---
name: git-pr-local-sync
description: Use for end-to-end Git delivery: commit and push changes, create or merge GitHub PRs, and reconcile local branches after merge.
---

# Git PR and Local Sync

Use this skill for end-to-end code delivery tasks involving local commits, remote branches, GitHub pull requests, merges, or post-merge synchronization. It provides the execution sequence; the project workflow document is the source of detailed repo policy and recovery steps: [PR workflow](../../guides/push-workflow.md). Read it before acting.

## Separate local Git from GitHub actions

- Use local Git in the correct checkout for inspection, diffing, staging, committing, pushing, fetching, switching branches, and pulling.
- When the GitHub plugin is available and connected, use it to inspect repository and PR state, checks, reviews, and merge results. It does not update local checkouts.
- If the plugin is unavailable, use another already-authorized GitHub interface. Do not search for or expose credentials, and do not claim a remote action succeeded without reading its result.

## Workflow

1. Read the project’s `AGENTS.md` and the linked workflow document. Confirm the repository root, remote URL, current branch, default branch, and worktree status before changing anything.
2. Preserve pre-existing changes. Review the full diff, run the validation required by the project, stage only intended files, inspect the staged diff, commit, then verify the new commit and worktree state.
3. Push the intended branch and verify the local commit matches its upstream. If push is rejected, fetch and inspect divergence before deciding how to proceed. Never use `--force` to hide divergence; only consider `--force-with-lease` for a confirmed private feature branch when the user’s request and project policy allow rewriting it.
4. Create or inspect the PR with the intended head and base. Review its diff, CI, required checks, reviews, mergeability, and repository merge policy. Do not merge a PR with failing checks, unresolved required reviews, unexpected changes, or an uncertain target branch.
5. Merge only when the user has asked for merging or clearly authorized it. Before merging, refresh the PR state. Follow the repository’s allowed merge method and protection rules; if permission or policy blocks the operation, report the exact blocker.
6. After merge, confirm the PR is merged and record its merge commit. In the same local repository, fetch, switch to the actual default branch, and fast-forward only. Verify `HEAD` equals `origin/<default-branch>` and that the worktree is clean.
7. For core plus consumer changes, finish and synchronize core first. Update each consumer repository, lockfile, and generated bindings separately, then validate and deliver each consumer PR independently.
8. Report the repository, branch and commit SHA, PR URL and merged state, merge SHA, local/remote default-branch equality, worktree state, validation results, and anything left incomplete.

## Stop and ask

Pause and offer clear options when the repository, branch, PR base, or merge policy is ambiguous; when existing changes cannot be safely separated from this task; or when resolving divergence would overwrite local commits, rewrite shared history, delete unique work, or bypass repository protections. Do not use `reset --hard`, `branch -D`, or a force push as a synchronization shortcut.

Treat explicit user instructions as controlling. A request to commit or push alone does not authorize creating or merging a PR; a request to create a PR alone does not authorize merging it. Do not repeat questions that the user has already answered in the active task.
