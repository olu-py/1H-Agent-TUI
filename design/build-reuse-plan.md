# 构建产物复用方案（workbase 三个 Rust 仓库）

> 状态：**已实施并实测通过**（§11 为完整实测数字）。只解决"同一份中间产物被重复编译"，不含任何以压缩体积为目的的手段。

## 0 一句话

问题不是"缓存太大"，而是**同一张依赖图被编译了很多遍**：历史上 `D:\workbase` 下出现过 10 个互不共享的 target 目录。办法分两层：① 每个仓库只留一个 target 目录（零成本，恢复仓库内复用）；② 用 cargo 的 `build.build-dir` 把中间产物从 target 目录里抽出来放进一个共享缓存，使三个仓库共用同一份中间产物。

## 1 目标与非目标

目标：让同一份中间产物只编译一次，并被多个仓库、多个 target 目录、多种用途复用；给出可证伪的验收判据（§7）。

非目标（明确不做）：

- 不动 `[profile.dev]` 的 `debug` 级别、不加 `strip`/压缩类瘦身手段。
- 不改 CI 缓存策略（CI 已有 `Swatinem/rust-cache`，Rust workflow 全覆盖）。
- 不改 core 的依赖形态、不合并仓库、不引入 workspace 级联。
- 不引入第三方构建工具（sccache 降级为可选，理由见 §5）。

## 2 现状证据（实测）

| 事实 | 数值 | 来源 |
| --- | --- | --- |
| 历史 target 目录 | 根下 7 个 `.cargo-target-*`（30.2 GB）+ 三个仓库内 `target/`（28.8 GB） | 逐目录实测 |
| 安装的 Rust 工具链 | `1.85.0`（0.41 GB）、`stable` = cargo 1.98.1（0.60 GB） | rustup 目录 |
| 依赖图规模 | core 339 / TUI 415 / webUI 359 个 package | 解析三份 `Cargo.lock` |
| TUI ∩ webUI（同名同版本） | **339**；三者共同 307 | 同上 |
| protium-core 来源 | TUI 与 webUI 锁定**同一个 git rev**（`d9d9e0c`） | `Cargo.lock` |
| clippy 产物 | 走 `RUSTC_WORKSPACE_WRAPPER`，"会影响文件名哈希，使 wrapper 产物单独缓存" | [Cargo Book](https://doc.rust-lang.org/cargo/reference/environment-variables.html) |
| 生效的机器级 cargo 配置 | `$CARGO_HOME/config.toml` = `D:\Rust\cargo\config.toml`（`CARGO_HOME` 是持久用户级变量）；`%USERPROFILE%\.cargo\config.toml` **不会被读取**；三个仓库内均无 `.cargo/` | 实测 |

## 3 被浪费的四条复用轴

1. **工具链往返**（1.85 ↔ stable）：历史做法是各开一个目录（`-core-185`/`-core-msrv` 对 `-core-stable`）。但 cargo 指纹里含 rustc 版本，**同一目录内两套产物可以共存、互不驱逐**，分目录纯属白丢复用。实测见 §11 的 K3–K5。
2. **path patch 往返**（本地 core ↔ git core）：`--config patch."https://github.com/olu-py/1H-Agent-core.git"...` 会改变 protium-core 的 SourceId 并改写锁文件，属于又一次全量；两种 SourceId 的产物同样可在同一目录共存（§11 的 S4–S6）。
3. **clippy ↔ test 交替**：clippy 通过 `RUSTC_WORKSPACE_WRAPPER` 只把 **workspace 成员**换成 clippy-driver，依赖仍复用 rustc 产物（实测 TUI 首次 clippy 只编 14 个单元、第二次 0）。所以这条轴的重复量是本仓 crate 的"两套产物"，看着小，但只要分目录就会各存一份、且第二次仍要全编。
4. **"为用途另开目录"**：7 个 `.cargo-target-*` 里至少 5 个是同一张图的重复（msrv/stable/final/tui/webui）。

## 4 阶段 1：一仓一目录（零成本）

**规则**：构建产物只落在仓库默认 `target/`；任何场合都**不得为了隔离工具链或用途而设置 `CARGO_TARGET_DIR`**。IDE 的 rust-analyzer 允许另设**一个**目录，属于有意例外。

为什么安全：cargo 指纹包含 rustc 版本与 SourceId，同一目录内多套变体互不驱逐（§11 实测 K3–K5 与 S4–S6 两个方向都是 0 重编）。

已知代价：单一 target 目录意味着**单一构建锁**，两个 cargo 进程会互相等待；AGENTS.md 已有"不要因 Cargo 锁或冷缓存终止正常构建"的条目，方向一致。

## 5 阶段 2：共享中间产物缓存（`build.build-dir`）

**机制**：cargo 1.98 支持 `[build] build-dir`，把"中间产物（build cache）"与"最终产物（target 目录）"分离。多个 target 目录指向同一个 build-dir 时，中间产物就只存一份。

**机制探针**（临时 crate，12 个编译单元，测完已删）：

| 实验 | 结果 |
| --- | --- |
| 同项目换 target 目录，共用同一个 build-dir | 第二次 `Compiling` = **0**（对照：不设 build-dir 时 = 12） |
| 不同项目（另一个 package 名、另一份源码）共用 build-dir | `Compiling` = **1**（只编自己；11 个依赖全部复用） |
| 设了 build-dir 后 target 目录的内容 | 只剩 5 个文件；202 个中间产物全在共享目录 |
| cargo `1.85.0`（MSRV） | `warning: unused config key` → **不识别，自动退回旧布局** |
| `cargo clean`（整目录） | **把共享缓存整片清空**（221 → 0 个文件） |
| `cargo clean -p serde` | 精准：只移除该包 23 个文件（202 → 179） |

**落地位置**：机器级 cargo 配置。本机 `CARGO_HOME=D:\Rust\cargo`（用户级持久变量），因此生效文件是 **`D:\Rust\cargo\config.toml`**（`$CARGO_HOME` 未设时才轮到 `%USERPROFILE%\.cargo\config.toml`，本机那份不存在且不会被读）。**不提交到任何仓库**，CI 完全不受影响：

```toml
[build]
build-dir = "D:/workbase/.cargo-build-cache"
```

### 为什么不用"一个全局共享 target 目录"

- 只能靠提交 `.cargo/config.toml` 实现，绝对路径机器相关；且 CI 的 `rust-cache` 只认 `target/`，会把 CI 缓存打失效。
- 或者靠每条命令设 `CARGO_TARGET_DIR` —— 这正是本次要根除的坏习惯。
- target 目录一旦共享，`cargo clean` 就是全局的，比 build-dir 更危险。

### 为什么不选 sccache

sccache 确实能跨目录、跨工具链命中，但代价明确且本仓不划算：它要求**关掉增量编译**，并且**"会调用系统链接器的 crate 一律不进缓存"，点名 `bin`/`dylib`/`cdylib`/`proc-macro`**（[sccache docs/Rust.md](https://github.com/mozilla/sccache/blob/main/docs/Rust.md)）；再加第三方工具与 10–25 GB 缓存，与"少占盘"互相抵消。**降级为可选**：本方案的共享缓存已覆盖"同工具链下的跨目录、跨仓库"复用，sccache 只能补"跨工具链"与"clean 之后"两个场景，暂不引入。

### MSRV 的现实（原假设被实测推翻）

- 该键在 1.85 上被忽略，只多一行 warning（实测）。
- **TUI 用 1.85 根本编不过**：`error: rustc 1.85.0 is not supported by the following packages: darling@0.24.1、globset@0.4.20、icu_*@2.3.0 requires rustc 1.88`。
- 而且 TUI 的 `ci.yml` 里**没有** MSRV job——`minimum-rust`（toolchain 1.85.0）在 **core 仓库**的 `ci.yml` 里。所以历史那些 `-core-185`/`-core-msrv` 目录是 core 的 MSRV 复现，这条轴属于 core，不属于 TUI。
- 结论：core 的 MSRV 构建写进 core 默认 `target/`，与 stable 产物同目录、不同指纹、互不驱逐，repeat 往返均 0 重编（§11 K3–K5）。
- **附带发现（不在本方案范围）**：TUI 声明的 `rust-version = "1.85"` 已与自己的 `Cargo.lock` 不兼容，且无 CI 覆盖，属于声明腐化。修法二选一——把 `rust-version` 提到 1.88，或在 TUI 的 ci.yml 补一个 `minimum-rust` job 把它变成真约束。**后续（已施工）**：两件都做了——`rust-version` 改为 `1.88`（先在本机核实过底线：`cargo +1.88.0 test --all-features --locked` 编译 249 个单元、26.5s、116 + 3 全通过），并在 TUI 的 ci.yml 补上 `minimum-rust` 档位。

## 6 阶段 3：约定落地（已按轻量方案完成）

- **AGENTS.md**（80 → 82 行，上限 85）：在"实施与验证"补一段——构建产物只落默认 `target/`、不得用 `CARGO_TARGET_DIR` 隔离、中间产物由 `$CARGO_HOME/config.toml` 的 `build.build-dir` 共享、**不要运行 `cargo clean`**。
- 未新增 `.agents/guides/build.md`：`check-agent-docs.sh` 强制"每个配置的指南恰好一次"路由，且每个指南需 5 个固定小节；`release.md` 已 49/50、`tui.md` 已 50/50 行，都塞不下。细节留在本文件。
- **已顺手修**：`README.md` 与 AGENTS.md 的 path patch 示例原先指向 `../protium-core`，与真实检出名不符，已改为 `../1H-Agent-core`（`check-agent-docs.sh` 只校验 patch 键与"本地 path patch"字样，改路径不影响校验）。
- **跨仓库**：core 与 webUI 各自的同类文档需走各自 PR 流程，本轮未做。

## 7 验收指标与实际结果

| 编号 | 判据 | 结果 |
| --- | --- | --- |
| A1 | 根下 `.cargo-target-*` 数量 = 0，每个仓库只有默认 `target/` | **通过**：0 个 |
| A2 | stable 构建的中间产物落在共享缓存，target 目录只剩最终产物 | **通过**：TUI `target/` 3 个文件（跑完全部闸门后 8 个 = 2 个 bin + 2 个 PDB + 锁/标记），共享缓存 2538 个 |
| A3 | 跨仓库复用：后建仓库的 `Compiling` 显著低于其依赖总数 | **通过**：webUI 首次 75（锁文件 359 包）、core 首次 62（锁文件 339 包） |
| A4 | 工具链往返：`+1.85.0` → `+stable` → `+1.85.0`，第三轮 = 0 | **通过**：在 core 上测得 178 → 0 → 0（TUI 不适用，原因见 §5） |
| A5 | patch 往返：带 / 不带 patch，第三轮 = 0 | **通过**：2 → 0 → 0，且 `Compiling protium-core v0.5.0 (D:\workbase\1H-Agent-core)` 证明 patch 生效 |
| A6 | 三个仓库跑完验证档位后 `D:\workbase` ≤ 25 GB | **通过**：**5.28 GB**（清理前 60.2 GB，约 11×） |
| A7 | fmt / clippy `-D warnings` / 全量测试 / 文档校验 / 空白检查全绿 | **通过**：116 + 3 全过、clippy 0、docs check passed、`git diff --check` 0 |

## 8 风险与回滚

| 风险 | 影响 | 处置 |
| --- | --- | --- |
| `cargo clean` 清空共享缓存（实测 221→0） | 三个仓库一起冷启动 | AGENTS.md 已明令禁止；回收空间改为直接删 `.cargo-build-cache`；单包清理用 `clean -p`（实测精准） |
| 1.85 不识别该键 | core 的 MSRV 构建各存一份 + 一行 warning | 已知成本；TUI 侧无影响（它本来就编不过 1.85） |
| 单一构建锁 | rust-analyzer 与手动/代理构建互相等待 | 允许 IDE 另设一个目录；与 AGENTS.md 现有条目一致 |
| `build-dir` 被官方标注"实现细节，可能无预警变更"（[cargo PR #15833](https://git.codeproxy.net/rust-lang/cargo/pull/15833#3)） | 升级工具链后行为可能变 | 升级后重跑 A2/A3；把它当成"两行可回退配置" |
| 共享缓存无上限增长 | 盘占用随项目增多上升 | 本阶段不做自动回收；需要时整体删除 |
| 共享缓存成为唯一副本 | 删掉它 = 全部冷启动 | 属预期；这也是"复用的代价是留着产物"的直接体现 |

**回滚**：删掉机器级配置里的 `build-dir` 那行 + 不再创建 `.cargo-target-*`，即可立即回到 cargo 默认行为；**不需要改任何仓库文件、不需要提交任何东西**。

## 9 实施顺序（已全部完成）

1. ✅ 阶段 1 规则写入 AGENTS.md。
2. ✅ 机器级 `build-dir` 配置（`D:\Rust\cargo\config.toml`）。
3. ✅ 跑 A1–A7 并记录数字（§11）。
4. ✅ 结论：不需要新指南；sccache 暂不引入（§5）。
5. ⬜（可选，跨仓库）core 与 webUI 的同类文档。

## 10 与复用无关、已一并修掉的一项

`README.md:105` 与 AGENTS.md 的 patch 示例原先写成 `path="../protium-core"`，而 core 检出实际叫 `1H-Agent-core`（`git = ".../1H-Agent-core.git"`）。照文档执行必然找不到目录，只能现场摸索——这是历史上"每次联调都临时发明目录"的诱因之一。现已统一为 `../1H-Agent-core`。

## 11 实施记录（全部实测）

环境：`CARGO_TARGET_DIR` 全程**未设置**（实测确认），`build-dir = D:/workbase/.cargo-build-cache`，全部构建带 `--offline`。

TUI（`cargo test --lib --all-features --locked`）：

| 步骤 | Compiling | 耗时 | 结果 |
| --- | --- | --- | --- |
| S1 首次（冷） | 249 | 23.9s | 116 passed |
| S2 重复 | **0** | 3.4s | 116 passed（比冷启快约 7×） |
| S3 webUI 首次 `test --workspace` | 75 | 12.7s | 2 passed（锁文件 359 包） |
| S4 带 path patch 首次 | 2 | 11.7s | 116 passed；`Compiling protium-core v0.5.0 (D:\workbase\1H-Agent-core)` |
| S5 去掉 patch | **0** | 2.2s | 116 passed |
| S6 再带 patch | **0** | 2.8s | 116 passed |
| S7 TUI + 1.85 | — | 0.4s | **error**（darling/globset/icu 需 1.88）+ `unused config key` warning |
| S8 回 stable | **0** | 1.6s | 116 passed |
| C1 clippy 首次 | 14 | 10.6s | `-D warnings` 通过 |
| C2 clippy 重复 | **0** | 0.5s | 通过 |
| C3 `fmt --check` | 0 | 0.3s | 通过 |
| C4 `test --workspace` | 1 | 7.6s | 116 + 3 全过 |
| A7 文档/空白 | — | — | `check-agent-docs`=0、`git diff --check`=0 |

core（`cargo check --all-features --locked`，验证第三个仓库与工具链轴）：

| 步骤 | Compiling | 耗时 | 结果 |
| --- | --- | --- | --- |
| K1 stable 首次 | 62 | 6.9s | 其余全部命中 TUI 播种的共享缓存 |
| K2 stable 重复 | **0** | 0.4s | — |
| K3 1.85 首次（冷） | 178 | 10.5s | 写入 core 默认 `target/`（1.85 忽略 build-dir）+ 一行 warning |
| K4 回 stable | **0** | 0.4s | 两套工具链产物在同一目录共存 |
| K5 再回 1.85 | **0** | 0.4s | **A4 判据达成** |

盘占用：

| 项 | 值 |
| --- | --- |
| 共享缓存 `.cargo-build-cache` | 4.11 GB（6484 个文件） |
| TUI `target/` | 0.36 GB（2 个 bin + 2 个 PDB 共 380 MB，链接产物不进缓存，属每仓库不可消除的成本） |
| core `target/` | 1.85 档位产物 1582 个文件 |
| webUI `target/` | 3 个锁/标记文件 |
| **`D:\workbase` 合计** | **5.28 GB**（清理前 60.2 GB） |

两个观察值得记住：

1. **链接产物只能待在各自的 `target/` 里**（TUI 的两个 PDB 各 163 MB）。所以"每仓一个 target 目录"不会被消除，只是它从"几 GB 的中间产物仓库"变回"几百 MB 的成品目录"。
2. **播种顺序有一次性收益**：先建 TUI（249）会为 webUI 省下约 284 个单元、为 core 省下约 277 个单元。反过来先建 core 也行——共享缓存谁先建谁受益，与仓库顺序无关。