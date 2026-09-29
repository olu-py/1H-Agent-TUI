#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root_doc="$repo_root/AGENTS.md"
legacy_name="AGENT"".""md"
legacy_doc="$repo_root/$legacy_name"
root_line_limit=85
guide_line_limit=50

fail() {
    echo "agent docs check failed: $*" >&2
    exit 1
}

line_count() {
    wc -l < "$1" | tr -d ' '
}

# 版本比较前先规范化：Cargo.toml 写 1.88、CI 工具链写 1.88.0，两者等价。
norm_version() {
    local major minor patch
    IFS='.' read -r major minor patch <<< "${1#v}"
    printf '%s.%s.%s' "${major:-0}" "${minor:-0}" "${patch:-0}"
}

test -f "$root_doc" || fail "AGENTS.md is missing"
test ! -e "$legacy_doc" || fail "legacy singular agent document must not exist"
grep -Fq '[AGENTS.md](AGENTS.md)' "$repo_root/README.md" || fail "README.md does not link AGENTS.md"
test "$(line_count "$root_doc")" -le "$root_line_limit" || fail "AGENTS.md exceeds $root_line_limit lines"

guides=(provider cluster runtime tui release tools storage ui-contract push-workflow)
sections=("## 适用范围" "## 入口" "## 不变量" "## 诊断" "## 验证")
for guide in "${guides[@]}"; do
    relative=".agents/guides/${guide}.md"
    path="$repo_root/$relative"
    test -f "$path" || fail "$relative is missing"
    test "$(line_count "$path")" -le "$guide_line_limit" || fail "$relative exceeds $guide_line_limit lines"
    grep -Fq "($relative)" "$root_doc" || fail "AGENTS.md does not route to $relative"
    for section in "${sections[@]}"; do
        grep -Fxq "$section" "$path" || fail "$relative is missing section: $section"
    done
done

guide_link_count="$(grep -oE '\.agents/guides/[^)[:space:]]+\.md' "$root_doc" | wc -l | tr -d ' ')"
test "$guide_link_count" -eq "${#guides[@]}" || fail "AGENTS.md must route exactly once to each configured guide"

if grep -R -n -F "$legacy_name" \
    "$repo_root/README.md" "$root_doc" "$repo_root/.agents" \
    "$repo_root/scripts" "$repo_root/.github"; then
    fail "legacy singular agent document reference found"
fi

extracted_core_path='crates/protium''-core'
legacy_core_test='cargo test -p protium''-core'
if grep -R -n -F "$extracted_core_path" \
    "$repo_root/README.md" "$root_doc" "$repo_root/.agents" \
    "$repo_root/docs" "$repo_root/.github"; then
    fail "consumer-local core path reference found"
fi
if grep -R -n -F "$legacy_core_test" \
    "$repo_root/README.md" "$root_doc" "$repo_root/.agents" \
    "$repo_root/docs" "$repo_root/.github"; then
    fail "consumer workspace core test command found"
fi

grep -Fq 'https://github.com/olu-py/1H-Agent-core' "$repo_root/README.md" \
    || fail "README.md does not link the core repository"
grep -Fq 'git = "https://github.com/olu-py/1H-Agent-core.git"' "$repo_root/Cargo.toml" \
    || fail "Cargo.toml does not use the canonical core Git dependency"
grep -Fq 'source = "git+https://github.com/olu-py/1H-Agent-core.git?rev=' "$repo_root/Cargo.lock" \
    || fail "Cargo.lock does not pin a core Git commit"
grep -Fq 'patch."https://github.com/olu-py/1H-Agent-core.git"' "$repo_root/README.md" \
    || fail "README.md does not document the local core path patch workflow"
grep -Fq '本地 path patch' "$root_doc" \
    || fail "AGENTS.md does not require the local core integration workflow"

core_patch_key='patch."https://github.com/olu-py/1H-Agent-core.git"'
if [[ -d "$repo_root/.cargo" ]] \
    && grep -R -n -F "$core_patch_key" "$repo_root/.cargo"; then
    fail "local core path patch remains in repository Cargo config"
fi

# --- 事实闸门 1：MSRV 声明必须与 ci.yml 的 minimum-rust 档位一致 -----------------
# 体积闸门管不住"写下即错的版本号"（TUI 曾有 rust-version=1.85 + 依赖要求 1.88、
# 且无任何 job 覆盖）。两个文件必须逐字一致，改一个忘一个就会被拦下。
workflow="$repo_root/.github/workflows/ci.yml"
declared_msrv="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$repo_root/Cargo.toml" | sort -u)"
test -n "$declared_msrv" || fail "Cargo.toml does not declare rust-version"
test "$(printf '%s\n' "$declared_msrv" | wc -l | tr -d ' ')" -eq 1 \
    || fail "Cargo.toml declares conflicting rust-version values: $(printf '%s' "$declared_msrv" | tr '\n' ' ')"
ci_msrv="$(sed -n '/^  minimum-rust:/,/^  [a-z][a-z0-9-]*:/p' "$workflow" \
    | sed -n 's/^[[:space:]]*toolchain:[[:space:]]*"\{0,1\}\([0-9][^"]*\)"\{0,1\}[[:space:]]*$/\1/p' \
    | head -n1)"
test -n "$ci_msrv" || fail "ci.yml has no minimum-rust job toolchain"
test "$(norm_version "$declared_msrv")" = "$(norm_version "$ci_msrv")" \
    || fail "Cargo.toml rust-version ($declared_msrv) does not match ci.yml minimum-rust ($ci_msrv)"

# --- 事实闸门 2：ci.yml 的 check 名必须与 release 指南的 required checks 清单一致 --
# 新增 job 忘改文档（或文档承诺了不存在的 check）都会被拦下；matrix 模板按
# check-name 取值展开后再比对。
release_guide="$repo_root/.agents/guides/release.md"
jobs_block="$(sed -n '/^jobs:/,$p' "$workflow")"
ci_names="$(printf '%s\n' "$jobs_block" | awk '
    /^  [a-z][a-z0-9-]*:[[:space:]]*$/ { injob = 1; next }
    injob && /^    name:[[:space:]]/ { sub(/^    name:[[:space:]]*/, ""); print; injob = 0 }
')"
matrix_values="$(printf '%s\n' "$jobs_block" | sed -n 's/^[[:space:]]*-[[:space:]]*check-name:[[:space:]]*\(.*[^[:space:]]\)[[:space:]]*$/\1/p')"
expanded_names=""
while IFS= read -r name; do
    [ -n "$name" ] || continue
    case "$name" in
        *'${{ matrix.check-name }}'*)
            while IFS= read -r value; do
                [ -n "$value" ] || continue
                expanded_names="${expanded_names}${name//'${{ matrix.check-name }}'/$value}"$'\n'
            done <<< "$matrix_values"
            ;;
        *) expanded_names="${expanded_names}${name}"$'\n' ;;
    esac
done <<< "$ci_names"
# 取该行第一段"以顿号分隔的连续反引号串"——check 清单就是这种写法；行内别处的
# 反引号（如 core 行首的 `main`、行尾的 `core-upgrade.yml`）不会被卷进来。
doc_names="$(grep -F 'required checks' "$release_guide" \
    | grep -oE '(`[^`]+`、)+`[^`]+`' | head -n1 | grep -oE '`[^`]+`' | tr -d '`' | sort -u)"
test -n "$doc_names" || fail "release guide does not list required checks"
ci_sorted="$(printf '%s' "$expanded_names" | sed '/^$/d' | sort -u)"
missing_in_doc="$(comm -23 <(printf '%s\n' "$ci_sorted") <(printf '%s\n' "$doc_names"))"
missing_in_ci="$(comm -13 <(printf '%s\n' "$ci_sorted") <(printf '%s\n' "$doc_names"))"
[ -z "$missing_in_doc" ] \
    || fail "ci.yml checks missing from the release guide: $(printf '%s' "$missing_in_doc" | tr '\n' ' ')"
[ -z "$missing_in_ci" ] \
    || fail "release guide lists unknown checks: $(printf '%s' "$missing_in_ci" | tr '\n' ' ')"

# --- 被 AGENTS.md 路由、但不属于"专题指南"的文档：同样纳入行数预算 --------------
# 项目 Skill 没有"适用范围/入口/不变量/诊断/验证"固定小节，进不了 guides 数组，但一样
# 会悄悄膨胀。预算按当前行数设定：要加内容，先删同等量。
flat_docs=(".agents/skills/git-pr-local-sync/SKILL.md:60")
for entry in "${flat_docs[@]}"; do
    flat="${entry%:*}"
    limit="${entry##*:}"
    path="$repo_root/$flat"
    test -f "$path" || fail "$flat is missing"
    test "$(line_count "$path")" -le "$limit" || fail "$flat exceeds $limit lines"
done

echo "agent docs check passed"
