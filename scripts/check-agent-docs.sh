#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
root_doc="$repo_root/AGENTS.md"
workflow="$repo_root/.github/workflows/ci.yml"
shared_root="$repo_root/.."
fail() { echo "agent entry check failed: $*" >&2; exit 1; }
test -f "$root_doc" || fail "AGENTS.md is missing"
grep -Fq "[AGENTS.md](AGENTS.md)" "$repo_root/README.md" || fail "README.md does not link AGENTS.md"
grep -Fq "../AGENTS.md" "$root_doc" || fail "module entry does not route to workspace rules"
grep -Fq "../.agents/guides/tui/" "$root_doc" || fail "module entry does not route to shared tui guides"
grep -Fq "../.agents/skills/git-pr-local-sync/SKILL.md" "$root_doc" || fail "module entry does not route to shared Git Skill"
if find "$repo_root/.agents" -type f -name "*.md" -print -quit 2>/dev/null | grep -q .; then echo "Legacy module maintenance documents remain under .agents/." >&2; exit 1; fi
if [[ -f "$shared_root/.agents/guides/tui/release.md" ]]; then grep -Fq "required checks" "$shared_root/.agents/guides/tui/release.md" || fail "shared tui release guide must document required checks"; else echo "Shared tui release guide is outside this standalone checkout; workspace link/content check deferred."; fi
grep -Fq 'rust-version = "1.88"' "$repo_root/Cargo.toml" || fail "Cargo.toml MSRV changed; update standalone checker"
grep -Eq 'toolchain:[[:space:]]*"?1\.88\.0"?$' "$workflow" || fail "CI minimum Rust toolchain changed; update standalone checker"
grep -Fq "https://github.com/olu-py/1H-Agent-core" "$repo_root/README.md" || fail "README.md does not link Core"
grep -Fq "git = \"https://github.com/olu-py/1H-Agent-core.git\"" "$repo_root/Cargo.toml" || fail "Cargo.toml does not use canonical Core Git dependency"
grep -Fq "source = \"git+https://github.com/olu-py/1H-Agent-core.git?rev=" "$repo_root/Cargo.lock" || fail "Cargo.lock does not pin a Core Git commit"
echo "TUI module entry and standalone CI metadata checks passed."
