#!/usr/bin/env bash
set -euo pipefail

rev="${1:-}"
if [[ ! "$rev" =~ ^[0-9a-fA-F]{40}$ ]]; then
  printf 'Usage: %s <40-character core SHA>\n' "$0" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/Cargo.toml"
lock="$root/Cargo.lock"
cd "$root"

if ! git diff --quiet -- Cargo.toml Cargo.lock || [[ -n "$(git ls-files --others --exclude-standard -- Cargo.toml Cargo.lock)" ]]; then
  printf 'Cargo.toml or Cargo.lock is dirty; commit or preserve those changes before updating core.\n' >&2
  exit 1
fi

manifest_backup="$(mktemp)"
lock_backup="$(mktemp)"
cp Cargo.toml "$manifest_backup"
cp Cargo.lock "$lock_backup"
cleanup() {
  rm -f "$manifest_backup" "$lock_backup"
}
rollback() {
  cp "$manifest_backup" Cargo.toml
  cp "$lock_backup" Cargo.lock
  cleanup
}
trap cleanup EXIT
trap rollback ERR

CORE_REV="$rev" perl -0pi -e 's/(protium-core\s*=\s*\{\s*git\s*=\s*"[^"]+"\s*,\s*)(?:branch\s*=\s*"main"|rev\s*=\s*"[0-9a-fA-F]{40}")/$1 . "rev = \"$ENV{CORE_REV}\""/e' Cargo.toml
cargo update -p protium-core --precise "$rev"

source="$(cargo metadata --locked --format-version 1 | perl -ne 'if (/"name":"protium-core".*?"source":"([^"]+)"/) { print "$1\n"; exit }')"
if [[ -z "$source" || "$source" == path+file* || "$source" != *"#$rev" ]]; then
  printf 'protium-core metadata source is not the requested Git revision: %s\n' "$source" >&2
  exit 1
fi

printf 'Updated protium-core to %s. Review the diff; this script does not commit or push.\n' "$rev"
