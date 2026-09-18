# TUI 功能对齐 WebUI 计划

> 状态：计划（未实施）
> 更新时间：2026-09-18 13:11:38 CST 星期五
> 范围：`1H-Agent-TUI` 消费端适配 protium-core 新接口，优先补齐功能层；不要求复刻 WebUI 视觉与布局。
> 结论：TUI 的事件流、会话、审批、分页历史、命令面板等基础能力已基本对齐；主要缺口集中在 Provider 设置、模型元数据、上下文计量，以及 core 依赖版本落后带来的新工具与恢复能力。

## 1. 基线与差距

### 1.1 当前版本基线

- TUI HEAD：`70d3aeb0a43529b437a39cb321111a48b182fdde`
- TUI `Cargo.lock` 中 `protium-core`：`80a41c4284549c32b46c245c0f3a328a77c24948`
- WebUI HEAD：`ad826cbbc94103b44d0fb4ae4943489c61b0c8c6`
- WebUI `Cargo.lock` 中 `protium-core`：`8c3413ca74782cf13a7df0b7e261ac53298d0b79`
- core HEAD：`7412962e43624d9db912c1270c179c8d664a50f0`

TUI 落后 core 的关键提交包括：

- Provider 设置视图与 `set_provider_profile` 语义。
- 四层模型元数据解析链：config / provider `/models` / models.dev / registry。
- `provider_models` 合并 models.dev 社区元数据。
- `set_provider_profile` 支持显式 context window 覆盖。
- 真实 usage 锚定计量与上下文溢出恢复。
- 可配置搜索后端 DuckDuckGo/Bing。
- `market_quote` 工具。
- Custom Responses endpoint 的 reasoning delta 修复。

### 1.2 功能差距矩阵

| 能力 | WebUI 当前实现 | TUI 当前实现 | 结论 |
| --- | --- | --- | --- |
| Provider 设置视图 | `AppHandle::provider_settings()` 返回 active/saved/connected | 打开设置时 `Config::load`，再用本地 config 和 key cache 组装 | TUI 需改为 core 权威视图 |
| 动态模型列表 | `AppHandle::provider_models(refresh)`，含窗口/最大输出元数据 | `ProviderPreset::selectable_models()` 静态列表 | TUI 需接入动态模型缓存 |
| Provider 编辑 | `set_provider_profile(preset, model, base_url, kind, context_window)` | `set_provider_config(full_profile)`，并手动保存 config | TUI 需采用 profile 语义，避免双写 |
| API Key 写入顺序 | 先写 keyring，再应用 profile | 先应用 profile，再写 keyring | TUI 需修正顺序，否则新 key 可能不立即生效 |
| 显式 context window | 可选覆盖，core clamp | 设置表单未暴露 | TUI 需补齐 |
| 连接状态 | `connected` 来自 core secret cache | TUI 自行调用 `api_key_cached_only` | TUI 应消费 core DTO |
| 上下文计量 | `context_updated` 为权威锚点，delta 只做 overlay | 处理 `context_updated`，但 `usage` 直接改 `context_used_tokens` 且使用 `max(limit)` | TUI 需修正语义 |
| 上下文溢出恢复 | core 新能力，WebUI 消费 `context_updated`/状态 | 依赖旧 core | 更新依赖后需验证 |
| 模型元数据来源 | `window_source` / `estimated` | 仅显示窗口与用量 | 功能已随 core 下发，TUI 可最小展示 |
| 新工具 | `market_quote`、可配置搜索后端 | 依赖旧 core | 更新依赖后自动可用，另补显示映射 |
| 会话命令 | `/new` `/rename` `/fork` `/delete` `/undo` `/redo` 等 | 已通过同一 core parser 支持 | 已对齐 |
| 消息分页 | `messages(before, limit)` | 已支持滚动加载旧页 | 已对齐 |
| 事件订阅/resync | `subscribe_from` + cursor 去重 + resync | 已支持 | 已对齐 |
| 审批 | 全局最旧审批 + approve/reject/always-session | 已支持 | 已对齐 |
| Todo | core todo events + `/todo` 命令 | 已支持 | 已对齐 |
| 未完成 assistant partial | snapshot 恢复 | 已支持 | 已对齐 |

## 2. 设计原则

1. **core 是唯一状态权威**：TUI 不再通过 `Config::load` 和本地 key cache 推导 Provider 列表、连接状态、模型元数据或上下文容量。
2. **不复制 WebUI UI**：保留 TUI 现有键盘/鼠标交互与布局，只补必要控件和状态提示。
3. **命令语义单一来源**：所有 slash command 继续走 `commands::parse` 与 `AppHandle::execute_command`，禁止 TUI 另建一套命令语义。
4. **协议只消费不扩展**：TUI 只消费 `protocol::Event`/DTO；未知事件按加法演进策略忽略，不解析 core 内部事件。
5. **网络元数据刷新不冻结终端**：动态模型刷新必须有加载态、失败降级和超时边界，不能阻塞主事件循环的可感知交互。
6. **密钥永不回显**：API Key 只写系统钥匙串/运行期 cache，不进入 config、日志、状态栏或模型上下文。

## 3. 实施阶段

### Phase 0：准备与基线确认

- [ ] 从 `main` 建独立分支。
- [ ] 确认 core `7412962e...` 已推送且可被 Git dependency 解析。
- [ ] 本地联调如需 path patch，仅使用命令行 `--config` 覆盖，不修改 `Cargo.toml`，验收前移除。
- [ ] 记录当前 TUI 测试基线，后续只回归相关过滤器，避免迭代期全量测试噪音。

**验收**：工作区无未预期改动；`Cargo.lock` 仍指向旧 core，尚未实施功能改动。

### Phase 1：升级 protium-core 依赖

- [ ] 将 TUI 的 `protium-core` Git dependency 更新到 core HEAD `7412962e...`。
- [ ] 只做编译适配，不同时混入行为重构。
- [ ] 确认 `Cargo.lock` 中 `protium-core` source 指向预期 commit。
- [ ] 回放 core conformance corpus，确认 TUI projection 对新增/既有事件无回归。
- [ ] 手工冒烟：
  - Custom Responses endpoint reasoning delta。
  - `web_search` 配置 DuckDuckGo/Bing。
  - `market_quote` 工具调用与审批展示。
  - 真实 usage 后收到 `context_updated`。

**此阶段自动获得的能力**：

- 四层模型元数据解析。
- models.dev 社区元数据合并。
- 真实 usage 锚定与上下文溢出恢复。
- 可配置搜索后端。
- `market_quote` 工具。
- Custom Responses reasoning 修复。

**验收**：TUI 能用新 core 编译并通过 conformance 相关测试；无 path patch。

### Phase 2：接入 Provider 设置权威视图

- [ ] 在 TUI facade 增加 Provider 设置状态，来源改为 `AppHandle::provider_settings()`：
  - `active`：当前 profile。
  - `saved`：已保存 profile。
  - `connected`：core 当前可解析 API Key 的 preset。
- [ ] `provider_choices()` 改为：
  - 以 core registry 顺序为基础；
  - 合并 `connected` 与 `saved`；
  - 保留 active preset；
  - 不再依赖 `app.config.providers` 作为显示权威。
- [ ] Provider 列表显示连接状态：
  - 已连接：正常可选；
  - 未连接：仍可进入编辑，但明确提示需要 API Key；
  - `connected` 缺失只表示当前未解析到 key，不判定“从未配置”。
- [ ] 打开设置时先读 cache，不强制网络刷新。
- [ ] 移除或降级 `Config::load` 在 Provider 设置中的职责；config 仍可用于 UI 偏好、启动参数与高级 TUI 设置。

**验收**：

- 与 WebUI 对同一 core 状态看到相同的 active/saved/connected 集合。
- TUI 不再自行推导连接状态。
- 无 API Key 泄漏到渲染或日志。

### Phase 3：接入动态模型列表与元数据

- [ ] 新增 TUI 侧 `ProviderModelsState`：
  - `models: Vec<ProviderModelDto>`
  - `fetched_at: Option<i64>`
  - `loading: bool`
  - `last_error: Option<String>`
- [ ] 打开模型菜单/设置页时调用 `provider_models(false)`，优先使用 core cache。
- [ ] 模型候选合并顺序：
  1. core 动态模型列表；
  2. preset 静态 fallback；
  3. 当前模型；
  4. saved profile 中的模型。
- [ ] 模型显示建议：
  - `model-id`
  - 有窗口时附 `128k` / `1m` 等短标签；
  - 有最大输出时可在详情/状态行展示；
  - 去重以 model id 为准。
- [ ] 增加手动刷新动作（建议键位 `r`）：
  - 调用 `provider_models(true)`；
  - 显示加载中；
  - 失败时保留旧列表与静态 fallback，只提示“模型刷新失败”；
  - 不阻塞 slash command、审批、取消等关键输入。
- [ ] 网络刷新放入后台任务，通过 TUI 内部任务结果 channel 回灌事件循环；不要在终端事件循环中长时间 `await` 网络。
- [ ] Provider 切换或模型切换后清空旧 provider 的动态模型缓存，重新读取新 active provider。

**验收**：

- Provider `/models` 与 models.dev 元数据能出现在 TUI 模型选择中。
- 刷新失败不破坏现有模型选择。
- 刷新期间终端仍可取消/审批/输入。
- 当前模型即使不在列表中也始终可选。

### Phase 4：Provider 编辑语义对齐

- [ ] 基础 Provider 编辑改走：
  `AppHandle::set_provider_profile(preset, model, base_url, kind, context_window_tokens)`。
- [ ] TUI 设置表单补齐字段：
  - Provider preset：只读身份，不通过切换字段绕过一个 preset 一个 profile 的规则；
  - Model；
  - Base URL；
  - Protocol：Responses / Chat Completions；
  - Context window override：空值表示继承合并 profile，填数字表示显式覆盖；
  - API Key：write-only。
- [ ] API Key 写入顺序修正为：
  1. 若输入非空，先 `secrets::store_api_key_cached(preset, key)`；
  2. keyring 写失败时保留本次运行有效 key，并显示降级警告；
  3. 再调用 `set_provider_profile`。
- [ ] 应用成功后：
  - 刷新 snapshot；
  - 重新读取 `provider_settings()`；
  - 重新读取 `provider_models(false)`；
  - 等待 `context_updated` 更新上下文容量。
- [ ] 移除 TUI 手动 `app.config.upsert_provider()` 与 `app.config.save()` 双写；core 的 `set_provider_profile`/`set_provider_config` 已负责持久化。
- [ ] 保留 TUI 高级能力：
  - Thinking level/budget 仍可通过现有 thinking 菜单编辑；
  - 如需完整 `ProviderConfig` 提交，仅用于 TUI 高级设置，并从刷新后的 config/snapshot 重新种子化，避免用 stale profile 覆盖 core。
- [ ] `remove_provider` 后改从 `provider_settings()` 收敛列表，不再依赖本地 config reload 作为权威。

**验收**：

- 新输入 API Key 后无需重启即可发起请求。
- keyring 写失败时请求可用但状态栏明确提示“仅本次运行有效”。
- Provider/base URL/protocol/model/context window 修改后 config 由 core 保存一次。
- WebUI 与 TUI 对同一 profile 的保存结果一致。

### Phase 5：上下文计量与 usage 语义修正

- [ ] `ContextUpdated` 是唯一权威锚点：
  - 更新 `context_budget`；
  - 更新 `context_window_tokens`；
  - 更新 `used_tokens`；
  - 重置本地 overlay。
- [ ] `Usage` 事件只更新 usage 展示，不再直接推导 `context_used_tokens`。
- [ ] 删除当前 `input_tokens.max(limit)` 逻辑；该逻辑既不符合 WebUI 语义，也可能把小上下文误显示为满载。
- [ ] 可选实现 TUI overlay：
  - text/reasoning delta 期间用有界粗估增加显示用量；
  - 收到 `context_updated`、completed、failed、cancelled、compaction 终态时重置；
  - overlay 只影响显示，不参与请求裁剪或核心状态。
- [ ] 上下文来源最小展示：
  - `config`：显式配置；
  - `provider`：Provider `/models`；
  - `community`：models.dev；
  - `registry`：内置注册表；
  - `unknown`：未知窗口；
  - `estimated=true` 时标注“估算”。
- [ ] 验证上下文溢出恢复：
  - core 触发恢复后 TUI 收到权威 `context_updated`；
  - 状态栏不再停留在旧用量；
  - 不用 TUI 本地字符数猜测容量。

**验收**：

- TUI 上下文条与 WebUI 对同一 session 的权威预算一致。
- usage 事件不再造成上下文条跳变或错误满载。
- 未知窗口显示安全提示，而不是猜测容量。

### Phase 6：新工具与配置展示补齐

- [ ] 为 `market_quote` 增加 TUI 显示映射：
  - 中文名：建议“实时行情”；
  - compact summary：显示 symbol/query 参数；
  - argument label：`symbol` -> “代码”，`query` -> “查询”；
  - 风险等级按 core security 分类展示。
- [ ] 确认 `web_search` 在 DuckDuckGo/Bing 配置下的事件展示不变。
- [ ] 在帮助或设置提示中最小说明搜索后端来自 config，不在 TUI 内另建配置存储。
- [ ] 不为 `market_quote` 添加 WebUI 式专门卡片；工具卡片统一渲染即可。

**验收**：新工具调用、审批、结果、失败都能在现有工具卡片中可读展示。

### Phase 7：功能层 parity 审计

- [ ] 命令 parity：
  - `/help` `/new` `/rename` `/delete` `/fork`
  - `/undo` `/redo` `/compact` `/uncompact` `/export` `/diff`
  - `/plan` `/build` `/explore` `/cluster`
  - `/model` `/provider` `/agent`
  - `/todo`
- [ ] 确认所有命令都经 core parser 和 `execute_command`，无 TUI 私有命令分支。
- [ ] 会话 parity：
  - active session 切换；
  - 非当前 session 的 fork/delete 若存在入口，必须先 activate 并确认收敛后再执行，防止命令打到错误 session；
  - 后台 session 不因 TUI 切换而被取消。
- [ ] 事件 parity：
  - 对 core conformance 全事件 corpus 回放；
  - `ResyncRequired` 后 snapshot + messages 重建；
  - `TranscriptInvalidated` 后丢弃旧分页缓存。
- [ ] 错误 parity：
  - `ApiError` message 展示；
  - secret redaction；
  - 404 session 清理语义与 WebUI 一致。
- [ ] 密钥安全审计：
  - `rg "api_key"` 检查无日志/状态泄漏；
  - UI 只显示 mask，不回显真实 key。

**验收**：除视觉差异外，用户可完成 WebUI 的核心功能闭环。

## 4. UI 决策点

默认采用“最小必要 UI”方案；如未另行确认，按推荐项执行。

### 4.1 Provider 设置界面

- **推荐：保留现有 TUI 设置页结构，仅改数据源与字段**
  - 优点：改动小、键盘路径稳定、功能可完整对齐。
  - 变化：增加 Context window 字段、动态模型列表、连接状态、刷新动作。
- 备选 A：复刻 WebUI 的 Provider/Model 分组弹窗。
  - 优点：交互更接近 WebUI。
  - 缺点：TUI 布局和 hit-test 改动大，超出功能层目标。
- 备选 B：只提供 `/provider` 与 `/model` 命令，不做可视化编辑。
  - 优点：UI 改动最小。
  - 缺点：无法完整编辑 base URL/protocol/context window/key，达不到功能 parity。

### 4.2 模型菜单

- **推荐：现有模型菜单内显示动态列表与短窗口标签**
  - 例如：`deepseek-v4-flash · 128k`
  - 刷新键：`r`
  - 加载态：`模型列表刷新中…`
- 备选：仅在设置页显示动态模型，顶部模型菜单继续用静态列表。
  - 缺点：同一功能两个数据源，容易漂移。

### 4.3 上下文计量

- **推荐：保留现有上下文条，增加来源/估算短标注**
  - 例如：`上下文 42% · 估算`
  - 未知窗口：`上下文未知`
- 备选：完整 tooltip/详情面板。
  - 缺点：TUI 需要新增浮窗与鼠标 hit-test，收益有限。

### 4.4 新工具展示

- **推荐：统一工具卡片 + 中文名与参数摘要**
  - 不做 WebUI 专属视觉复刻。
- 备选：为 `market_quote` 做专用行情卡片。
  - 仅在统一卡片可读性不足时再考虑。

## 5. 测试计划

### 5.1 单元与集成过滤器

| 改动 | 建议过滤器 |
| --- | --- |
| core 依赖升级后事件回放 | `cargo test --lib --all-features --locked conformance` |
| Provider 设置状态 | `app::tests::provider` |
| 动态模型列表 | `app::tests::model` / `provider_models` |
| Provider apply | `app::tests::settings` / `set_provider_profile` |
| 上下文计量 | `projection::tests::context` / `usage` |
| 工具显示 | `ui::tests::tool` / `projection::tests::tool` |
| 会话与命令 | `app::tests::command` / `session` |

### 5.2 必测行为

- [ ] `provider_settings()` 返回的 active/saved/connected 正确进入 TUI 状态。
- [ ] 动态模型列表与静态 fallback 合并且去重。
- [ ] `provider_models(true)` 失败时保留旧列表。
- [ ] 模型刷新不阻塞取消/审批。
- [ ] API Key 先写 keyring/cache，再应用 profile。
- [ ] keyring 写失败时本次运行仍可用并提示降级。
- [ ] `set_provider_profile` 后 config 只持久化一次。
- [ ] context window 空值继承合并 profile。
- [ ] context window 数字值由 core clamp。
- [ ] `Usage` 不再改写上下文用量。
- [ ] `ContextUpdated` 重置 overlay。
- [ ] `ResyncRequired` 后重建 projection。
- [ ] `market_quote` 工具显示与审批正常。
- [ ] 搜索后端切换后 `web_search` 正常。
- [ ] 所有真实 key 不出现在 buffer、日志、错误消息或导出内容中。

### 5.3 完成阶段验证

```bash
cargo fmt --all -- --check
cargo test --lib --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
bash scripts/check-agent-docs.sh
git diff --check
```

说明：Phase 1 可先跑 conformance 与相关编译测试；Phase 4/5 涉及协议消费、配置持久化和跨模块状态，应升级到完整 Clippy 与 workspace 测试。

## 6. 实施顺序与依赖关系

```text
Phase 1 core 依赖升级
  └─ Phase 2 provider_settings 权威视图
      └─ Phase 3 provider_models 动态列表
          └─ Phase 4 set_provider_profile 编辑语义
              └─ Phase 5 context/usage 修正
                  └─ Phase 6 新工具显示
                      └─ Phase 7 parity 审计与完整验证
```

不建议并行调整 Phase 4 与 Phase 5：Provider profile 会影响 context window，而 context 修正依赖新 core 的 `ContextUpdated` 语义。Phase 6 可与 Phase 5 并行，但应在 Phase 1 完成后进行。

## 7. 非目标

- 不复刻 WebUI 的 React 组件、圆角、弹窗动画或视觉层级。
- 不在 TUI 内实现 HTTP/SSE transport；继续使用进程内 `AppHandle`。
- 不修改 core 状态机、Storage、Provider 私有协议或审批 oneshot。
- 不引入运行时 Node、Electron、浏览器或动态插件。
- 不把 API Key 写入 TOML/SQLite/日志。
- 不用 TUI 本地估算替代 core 的上下文容量与溢出恢复决策。

## 8. 完成定义

满足以下条件时，可认为 TUI 功能层已跟上 WebUI：

1. TUI 锁定 core `7412962e...` 或更新版本，且无本地 path patch。
2. Provider active/saved/connected 与动态模型列表均来自 core 权威接口。
3. Provider 编辑支持 model、base URL、protocol、context window override 和 write-only API Key，并按正确顺序应用。
4. 上下文计量消费 core 权威 `ContextUpdated`，`Usage` 只作 usage 展示。
5. core 新工具与新搜索后端在 TUI 中可用且可读。
6. 会话、命令、审批、分页历史、事件 resync 行为与 WebUI 语义一致。
7. 相关单元、conformance、Clippy 和 workspace 测试全部通过。










































