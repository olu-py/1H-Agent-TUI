#!/usr/bin/env bash
# 依赖声明的 MSRV 底线必须与 Cargo.toml 的 rust-version 一致。
#
# 为什么需要它：体积闸门（check-agent-docs.sh）管不住"声明值低于依赖底线"这种
# 写下即错的问题——1H-Agent-webUI 曾声明 1.85 而依赖要求 1.88，且没有任何 job
# 覆盖，直到人工用 cargo metadata 才发现。本脚本把这条底线变成断言。
#
# 需要 registry 元数据，因此放在已有的 minimum-rust job 内运行（那里工具链与
# rust-cache 都已就绪），不要放进跑在 rust-cache 之前的文档检查步骤。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
    echo "msrv floor check failed: $*" >&2
    exit 1
}

# 版本比较前先规范化：Cargo.toml 写 1.88、依赖写 1.88.0，两者等价。
norm_version() {
    local major minor patch
    IFS='.' read -r major minor patch <<< "${1#v}"
    printf '%s.%s.%s' "${major:-0}" "${minor:-0}" "${patch:-0}"
}

declared="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | sort -u)"
test -n "$declared" || fail "Cargo.toml does not declare rust-version"
test "$(printf '%s\n' "$declared" | wc -l | tr -d ' ')" -eq 1 \
    || fail "Cargo.toml declares conflicting rust-version values: $(printf '%s' "$declared" | tr '\n' ' ')"

# 必须探测"能真正执行"的解释器：Windows 上 python3 常是应用商店占位程序，
# command -v 找得到但它什么都不做（退出码 49），会静默骗过本地演练。
interpreter=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 \
        && "$candidate" -c 'import json, sys' >/dev/null 2>&1; then
        interpreter="$candidate"
        break
    fi
done
test -n "$interpreter" || fail "a working python3 (or python, with json) is required to read cargo metadata"

# 从 stdin 读字节再显式按 UTF-8 解码：不依赖平台默认编码，也不涉及临时文件路径
# （Git Bash 的 /tmp 与原生解释器看到的路径不是一回事）。
read_floor='
import json, sys


def key(value):
    parts = [int(part) for part in value.split(".")[:3]]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts)


metadata = json.loads(sys.stdin.buffer.read().decode("utf-8"))
members = set(metadata["workspace_members"])
versions = [
    p["rust_version"]
    for p in metadata["packages"]
    if p.get("rust_version") and p["id"] not in members
]
print(max(versions, key=key) if versions else "")
'
metadata="$(cargo metadata --locked --format-version 1)" \
    || fail "cargo metadata failed; see the cargo error above for the manifest problem"
floor="$(printf '%s' "$metadata" | "$interpreter" -c "$read_floor")" \
    || fail "could not parse cargo metadata output"
test -n "$floor" || fail "no dependency declares rust_version; cannot determine the floor"

test "$(norm_version "$declared")" = "$(norm_version "$floor")" \
    || fail "Cargo.toml rust-version ($declared) != highest dependency rust_version ($floor); bump or lower the declaration"

echo "msrv floor check passed (rust-version = $floor)"