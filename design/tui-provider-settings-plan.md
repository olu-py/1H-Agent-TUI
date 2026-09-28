# TUI 供应商界面制作计划

> 状态：Phase 1 已实施；Phase 2+ 计划（未实施）
> 更新时间：2026-09-28
> 范围：`1H-Agent-TUI` 消费端。第一优先级是**让供应商面板有醒目入口**；其次才是面板本身与 WebUI 的功能对齐。
> 参照实现：`1H-Agent-webUI`（`web/src/components/ProviderSettingsModal.tsx`、`ProviderSwitcher.tsx`、`web/src/lib/providers.ts`）。
> 结论：供应商面板**功能已存在**（Ctrl+S / `/provider`，`dec64c6` 落地），但**界面上没有任何入口指向它**——用户看不到、也就等于没有。本计划先补入口，再补面板形态。

## 1. 探索结论

### 1.1 TUI 现有供应商相关实现

| 部件 | 入口 | 实现 | 现状 |
| --- | --- | --- | --- |
| 供应商列表 + 模板选择 + 字段表单 | Ctrl+S、`/provider`、命令面板 | `src/app/settings.rs`、`src/ui/modal.rs` `draw_settings` | 可用。字段：提供商(只读)/名称/协议/模型/接口地址/思考能力/上下文窗口/API Key |
| 页脚供应商切换器 | Alt+P 或点击页脚供应商文字 | `src/app/provider.rs` `provider_choices`、`src/ui/status.rs` `draw_provider_menu` | 可用，但外观与普通文本无异 |
| 页脚模型切换器 | Alt+M 或点击页脚模型文字 | `model_choices`、`draw_model_menu` | 已接 core 动态列表 + 静态 fallback + 窗口短标签，`r` 刷新 |
| 首页供应商/模型控件 | 启动时 | `src/home.rs`、`src/app/event_loop.rs` | 可用，数据源是本地 `config`，无连接状态 |
| 供应商数据源 | — | `AppHandle::provider_settings()` | 已是 core 权威（active / saved / connected） |
| 供应商保存语义 | — | `set_provider_profile` / `remove_provider` | 已对齐 WebUI |

### 1.2 入口可发现性审计（本次问题的根因）

主界面页脚两行是全部可见信息，逐段核对：

| 可见元素 | 现状 | 是否指向供应商面板 |
| --- | --- | --- |
| 输入框标题：` 输入 · 构建 ` | `draw_input`（`ui_view_model.rs` 的 `InputView.title`） | 否，但**这是 mode 的正式位置**，且已经可点击切模式（`input_mode_rect` → `next_mode`） |
| 主行左：`○ 就绪` | `ui_view_model.rs` `activity_view` | 否 |
| 主行右：`Enter 发送  Ctrl+P/X 命令` | `contextual_shortcuts` | **否——所有分支都没有 Ctrl+S** |
| 次行左：`构建 · DeepSeek · deepseek-v4-flash` | `metadata_segments` | 点击可开切换器，但**无 `▾`、无色差、无图标**，看不出可点；且 `构建 · ` 与输入框标题**重复** |
| 次行右：`上下文 42% 可用120k · 内置注册表 · 思考 Auto ▾` | `thinking_segments` | 只有"思考"带 `▾`，视觉上唯一像控件的东西 |
| 首页页脚：`DeepSeek / deepseek-v4-flash   Tab 最近会话  Enter 开始  Ctrl+C 退出` | `home.rs` `draw_footer` | 否 |
| `/help` 文案 | `commands.rs` `Command::Help` | **否——目前完全没提 Ctrl+S / 供应商设置** |
| 命令面板 | core `PALETTE_ITEMS` 有 `Provider 设置 · Ctrl+S` | 是，但要先按 Ctrl+P 再读列表 |

结论：**供应商面板只存在于"知道快捷键的人的脑子里"**。加上切换器本身也没有入口进面板（WebUI 的切换器底栏有"Provider 设置"一行），所以整条路径对外不可见。

> 本节记录 Phase 1 实施前的状态；除"未连接供应商行置灰"外，列出的缺口均已在 Phase 1 修掉。保留原文以便复查改动是否覆盖了每一处。

### 1.3 WebUI 的功能参照

`ProviderSettingsModal`（544 行）与 `ProviderSwitcher`（407 行）：

1. **切换器**只列 `connected` 供应商并按 id 分组（组标题 = 自定义名或 preset 标签），每组列出模型（`128k ctx · 8,192 out`），点选即切供应商 + 模型；**底栏固定一行 `⚙ Provider 设置`** 打开完整弹窗。
2. **面板**：4 个内置预设（含未保存的）+ 全部已保存自定义 + "添加自定义供应商"行；行上有"已配置"tag、密钥就绪圆点、当前勾。
3. **字段**：名称（自定义必填，空则禁用应用）；模型（下拉：动态优先 + 静态补全 + 当前值兜底，带窗口短标签与元数据 tooltip）；上下文窗口（**仅当窗口未知或已显式填写时出现**，附"获取"按钮与 4096–10000000 提示）；Base URL；协议（下拉）；API Key（只写、`已配置/未配置` 徽标、密钥仅入钥匙串的说明）。
4. **动作**：删除（二次确认，提示密钥保留在钥匙串）、取消、应用（保存中禁用）；错误内联。
5. **动态列表作用域**：仅当编辑的就是 active profile 且 base_url 一致时才展示抓取结果与"获取"，否则退回静态 preset 列表。

### 1.4 功能差距矩阵

| 能力 | WebUI | TUI 现状 | 结论 |
| --- | --- | --- | --- |
| **可见入口进供应商面板** | 切换器底栏 + composer 触发器 | 无 | **需补（首要）** |
| 快捷键提示暴露 Ctrl+S | — | 无 | 需补 |
| 供应商控件有可点外观 | 按钮样式 + chevron | 纯文本 | 需补 |
| 页脚 mode 与输入框标题重复 | 无（mode 只在输入框） | 重复显示 `构建 · ` | 需补 |
| 窄终端（< 70 列）保留供应商控件 | 是 | 否（Compact 只剩 mode 文本，控件矩形为 `None`） | 需补 |
| 未配置密钥的可见提醒 | 面板内状态 | 无 | 需补 |
| 首屏引导 | — | 无 | 需补 |
| 切换器按供应商分组模型 | 是 | 否（扁平） | 需补 |
| 切换器内"供应商设置"入口 | 是 | 否 | 需补 |
| 未保存内置预设出现在列表 | 是（4 行固定） | 否 | 需补 |
| 删除二次确认 | 是 | 否（Ctrl+D 直接删） | 需补 |
| 应用禁用态 + 内联错误 | 是 | 否（写页脚状态行） | 需补 |
| 名称必填实时校验 | 是 | 仅应用时 bail | 需补 |
| 模型下拉（动态 + 元数据） | 是 | 否（`←/→` 循环静态列表） | 需补 |
| 上下文窗口条件显示 + "获取" | 是 | 否 | 需补 |
| Key 已配置/未配置徽标 | 是 | 部分（掩码） | 需补 |
| Key 明文显示/隐藏 | 是 | 否 | **有意不补**（见 §2.5） |
| 表单字段鼠标点击 | 是 | 否 | 需补 |
| 首页数据源 = core 权威 | 是 | 否 | 需补 |
| `enabled_models` 可编辑 | 否 | 否 | 非目标 |

## 2. 设计原则

1. **入口先于形态**：先把"能看到、能点到"做出来（Phase 1），再谈面板内部对齐。
2. **core 是唯一权威**：列表、连接状态、模型元数据、窗口都来自 `provider_settings()` / `provider_models()`；本地 `config` 只作首次 core 读取前的兜底。
3. **不复制 WebUI 视觉**：保留 TUI 的键盘/鼠标契约、`PickerGeometry`、`VisualRole` 主题与中文标签。
4. **渲染与命中复用同一份文本**：`provider_model_rects` 从 `view.footer.secondary.left` 的文本前缀反推矩形；给 pill 加 `⚙`/`▾` 时必须同步改这段换算，并由测试锁住。
5. **密钥永不回显**：TUI 不提供明文显示切换（WebUI 的 `type=text` 会落进 scrollback、录屏与网络终端）。只显示掩码与"已配置/未配置"；状态栏、日志、错误一律 `secrets::redact`。
6. **不冻结终端**：网络动作（刷新模型、获取窗口）走 `spawn_model_refresh` 式后台任务 + 内部 channel 回灌；事件循环内只允许 cache-only `await`。
7. **不引入 core 改动**：核心 rev 保持 `d9d9e0c`；`protium_core::settings` 只读复用。
8. **首页约束**：首页阶段 `App`/`settings` 尚不存在（`event_loop.rs` 先跑 `home_event_loop` 再 `build_app`），因此首页**不做**面板入口，只做"选供应商"；"管理供应商"留在主界面。

## 3. 入口设计（Phase 1 交付物）

### E1 页脚供应商 pill 变成明确的控件（并去掉重复的 mode 前缀）

mode 已经写在输入框标题上（` 输入 · 构建 `，且该文字本身可点击切模式），页脚再重复一次 `构建 · ` 属于冗余，一并去掉：

```text
现在：  构建 · DeepSeek · deepseek-v4-flash
改为：  ⚙ DeepSeek ▾ · deepseek-v4-flash
        └────────────┘
        可点击区域（含两端符号）
```

- `metadata_segments` 去掉 `mode` 参数：`Density::Wide` / `Standard` 渲染 `{provider} · {model}`，不再带 mode 前缀。
- provider 段文本改为 `⚙ {label} ▾`，角色从 `Secondary` 提到 `Accent`（与首页 provider 一致）。
- **`Density::Compact`（宽度 < 70）必须改为渲染 provider 段本身**。现在该分支只渲染 `mode_label`（一个孤零零的 `构建`），去掉 mode 后会变成空行，且 `provider_model_rects` 依赖 `{mode} · ` 前缀判断，导致窄终端**今天就已经拿不到可点的供应商/模型控件**——恰恰是本次要修的可发现性问题的最坏场景。Compact 下渲染 `⚙ {label} ▾`（省略模型名），让入口在 70 列以下也活着。
- `provider_model_rects` 不再用字符串前缀反推：把 `metadata_segments` 的左侧拆成 **provider 段 / 分隔符段 / 模型段** 三个 `UiSegment`，矩形由真实段宽累加得出。这样去掉 mode 前缀只是删一个段，同时消除"渲染文本与命中矩形各算一遍"的漂移风险（`.agents/guides/tui.md` 不变量）。
- 点击行为不变（打开切换器），但切换器底栏新增入口（E6/P3）后形成完整路径。

### E2 页脚常量快捷键提示「Ctrl+S 供应商」

`contextual_shortcuts` 目前没有任何分支提到供应商面板。改为：

| 状态 | 提示（按保留优先级排序） |
| --- | --- |
| 空闲、输入框为空 | `Enter 发送` → `Ctrl+S 供应商设置` → `Ctrl+P/X 命令` |
| 空闲、输入框非空 | `Enter 发送` → `Ctrl+S 供应商设置` → `Shift+Enter 换行` |
| 滚动未跟随 | `Ctrl+L 回到底部` → `Enter 发送` → `Ctrl+S 供应商设置` |

- `hint("Ctrl+S", "供应商设置", 2)`：`fit_shortcuts` 按 priority 从大到小裁剪，所以它只在窄屏让位给 `Enter 发送`。
- **忙碌、审批与设置面板打开时不出现该提示**：`Ctrl+S` 的全局处理器在 `app.current.busy` 时拒绝打开面板，提示里绝不能广告一个按了没反应的键。
- "Ctrl+S 供应商"里的 `Ctrl+S` 用 `VisualRole::Shortcut`，与其它快捷键同款高亮。

### E3 供应商未就绪的显式提醒

数据源是 core `provider_settings().connected`：

- 当 `active_provider_id()` 不在 `connected` 中：
  - 页脚 provider pill 用 `VisualRole::Warning`，文本追加 ` · 需要 API Key`。
  - 主行活动区（`activity_view`）在 Idle 时显示 `! 供应商未配置密钥`，替代 `○ 就绪`；忙碌中的真实状态优先，不被覆盖。
- 纯展示，不改 agent 行为，不阻塞提交（core 会在请求时自行报错）。

### E4 首屏引导条目

- 条件：`entries` 为空、Idle、非 busy、无待审批、`active ∉ connected`。
- 动作：向 transcript push 一条 `DisplayKind::System` Markdown 条目：

  ```markdown
  ## 供应商未就绪

  当前供应商 **{label}** 还没有可解析的 API Key。

  - 按 **Ctrl+S** 打开供应商设置（也可输入 `/provider`）
  - 密钥只写入系统钥匙串，不会进入配置文件、日志或模型上下文
  ```

- 用 `App` 上新增的 `provider_hint_shown: bool` 保证每个会话只推一次；`connected` 满足后不再推。
- 该条目是普通 transcript 条目，随会话滚动、可复制，不新增渲染分支。

### E5 `/help` 与命令面板

- `Command::Help` 文案补一行：`Ctrl+S 或 /provider 打开供应商设置（供应商、密钥、模型与上下文窗口）`。
- 命令面板条目由 core 的 `PALETTE_ITEMS` 提供，已含 `Provider 设置 · Ctrl+S`，TUI 不改 core；打开面板时状态行补一句指向。

### E6 切换器底栏入口

页脚切换器底部固定一行（不可被列表滚动挤掉）：

```text
┌ DeepSeek · deepseek-v4-flash ──────────────┐
│ DeepSeek                        当前       │
│   deepseek-v4-flash · 128k        ✓        │
│   deepseek-v4-pro · 128k                   │
│ 公司网关                                   │
│   deepseek-v4-flash · 128k                 │
│ 内网网关2                    需要 API Key  │
│ ─────────────────────────────────────────  │
│ ⚙ 供应商设置…                        Ctrl+S │
└────────────────────────────────────────────┘
```

- 该行是独立可点击目标，写入自己的 `Rect`（`PickerGeometry::action`），绘制与命中读同一个槽位；键盘用 `Ctrl+S` 触发（与面板内"应用"同键，是全局约定）。
- 决策（已确认）：未配置密钥的供应商**保留但置灰**，选中时不切换、改为打开供应商设置并定位到该行——比 WebUI 的隐藏更易发现。**Phase 2 完成后**随面板改造一并落地。

## 4. 界面与交互设计（Phase 2+）

### 4.1 供应商面板（双栏）

尺寸 `centered_rect(92, 26, area)`，< 70 列时退化为单栏纵向堆叠：

```text
┌ 供应商管理 ─────────────────────────────────────────────────────────────┐
│ 供应商                    │ 编辑 公司网关                               │
│  › OpenAI        已配置   │   名称        公司网关        必填           │
│    DeepSeek      当前 ●   │   模板        Custom compatible（只读）      │
│    Qwen / Bailian         │   协议        Chat Completions        ←/→   │
│    Volcano Ark            │   模型        deepseek-v4-flash · 128k Enter │
│    公司网关      已配置   │   接口地址    https://gw.example.com/v1      │
│    内网网关2              │   上下文窗口  （继承）          Ctrl+G 获取 │
│  ＋ 添加自定义供应商      │   思考能力    Auto                    ←/→   │
│                           │   API Key     已配置 ********（只写）        │
│                           │   接口与内置注册表未报告该模型窗口；显式值   │
│                           │   优先生效（4096–10000000）。                │
├───────────────────────────┴─────────────────────────────────────────────┤
│ Tab 切换窗格  ↑/↓ 选择  ←/→ 修改  Enter 打开选择器  Ctrl+S 应用          │
│ Ctrl+D 删除（需确认）  Esc 返回/关闭                                     │
└─────────────────────────────────────────────────────────────────────────┘
```

- 左栏行序：内置四家（恒显示）→ 已保存自定义（按 `saved` 顺序）→ `＋ 添加自定义供应商`。标记：`当前` / `已配置`（在 `saved` 中）/ `●`（在 `connected` 中）。
- 右栏字段序：名称 → 模板（只读）→ 协议 → 模型 → 接口地址 → 上下文窗口 → 思考能力 → API Key。
- 底栏动作行含 `应用` / `删除` / `取消` 三个独立可点 `Rect`。

### 4.2 键位与鼠标

| 输入 | 行为 |
| --- | --- |
| `Tab` / `Shift+Tab` | 左右窗格切换 |
| `↑` / `↓` | 当前窗格内移动 |
| `Enter`（左栏行） | 载入 profile（未保存内置 = 该 preset 的 `defaults()` 播种） |
| `Enter`（`＋ 添加` 行） | 新建自定义：`id=""`、`preset=Custom`、`kind=ChatCompletions`、`model=""` |
| `Enter`（模型字段） | 打开模型选择器 |
| `←` / `→` | 协议、思考能力枚举切换 |
| 键入 / `Backspace` / `Delete` | 文本字段编辑 |
| `Ctrl+U` | 清空当前文本字段 |
| `r`（模型字段/选择器内） | 后台刷新动态模型列表；**实施时改为 `Ctrl+R`**（见 Phase 2 实施记录 1） |
| `g`（上下文窗口字段） | 获取：后台刷新后回填 active provider 同名模型的窗口；**实施时改为 `Ctrl+G`**（同上） |
| `Ctrl+S` | 应用 |
| `Ctrl+D` | 删除 → 二次确认态（`y` 确认 / `n`、`Esc` 取消） |
| `Esc` | 确认态取消 → 表单态回左栏（保留缓冲）→ 左栏态关闭界面 |
| 鼠标左键 | 左栏行、右栏字段、动作行的点击全部生效 |

### 4.3 模型选择器

复用 `PickerGeometry`，行序：动态列表（仅当编辑的就是 active profile 且 base_url 一致）→ preset 静态列表 → 当前值 → 该 profile 已保存模型，按 id 去重；行文本 `id · 128k`，选中行右侧显示 `8,192 out`；末行 `（自定义模型名…）` 把焦点交回文本框。

### 4.4 上下文窗口字段

恒可编辑，默认显示 `（继承）`；窗口未知时附加提示 `接口与内置注册表未报告该模型窗口；显式值优先生效（4096–10000000）。`。"未知"判定按 WebUI 同口径：动态列表中该模型有 `context_window_tokens`，或 preset 静态列表包含该模型，则视为已知。`Ctrl+G 获取` 仅在动态列表适用时可用。

### 4.5 首页

- 供应商/模型控件加 `▾`；数据源改为 `provider_settings()` + `provider_models(false)`，失败退回本地 `config`。
- 供应商行显示 `已连接 / 需要 API Key`，模型行显示 `id · 128k`。
- `apply_home_selection` 从 `set_provider_config(整份 profile)` 改为 `set_provider_profile`，且仅在确有修改时调用。
- **不在首页做面板入口**（见 §2.8）。

## 5. 实施阶段

### Phase 0：基线与决策

- [ ] 从 `main` 建独立分支。
- [ ] 记录 `cargo test --lib --all-features --locked app::tests::provider` 基线。

### Phase 1：入口显著化（本计划的核心交付）——已实施

- [x] E1 `metadata_segments` 去掉 mode 前缀并拆成 provider / 分隔符 / 模型三个 `UiSegment`（`⚙ {label} ▾` + ` · ` + `{model}`）；`provider_model_rects` 改为按真实段宽累加；Compact 宽度下渲染 `⚙ {label} ▾` 保住入口。
- [x] E1b 确认 mode 只出现在输入框标题 ` 输入 · 构建 ` 一处，且该处仍可点击切模式；页脚不再出现 mode 字样。
- [x] E2 `contextual_shortcuts` 三个空闲分支加入 `Ctrl+S 供应商设置`；忙碌/审批/面板打开时不显示。
- [x] E3 `connected` 判定接入页脚：provider pill 的 Warning 角色 + `需要 API Key` 后缀（Compact 密度省略后缀，见实施记录）；`activity_view` 的 Idle 分支接未配置提醒。
- [x] E4 首屏引导条目 + `App::provider_hint_shown`；密钥解析成功后自动撤回该条目。
- [x] E5 `Command::Help` 文案补 Ctrl+S。
- [x] E6 切换器底栏 `⚙ 供应商设置…` 行：`PickerGeometry::new_with_action` 保留一行、`action` 矩形独立于 item 窗口，鼠标点击与 `Ctrl+S` 都能打开面板。
- [ ] 未连接供应商行置灰 + 选中跳面板：按 §3 E6 的决策留给 Phase 2（需要先有"定位到某一行"的编辑器状态）。

**实施记录**：`metadata_segments` 的 mode 参数被删除后，`provider_model_rects` 不再做字符串前缀反推，改为复用与 `footer_line` 同一个 `clip_segments_with_fit`（新增"是否整段画出"标志），因此被裁断的 pill 不会留下可点矩形。`Ctrl+S` 必须在切换器内由 `provider_menu_key_handled` 先认领，否则会被"未处理按键即关闭"规则吞掉。

把实际帧打印出来核对时抓到两个只看代码看不出的问题，已在同一改动内修掉：

1. **Compact 下 `需要 API Key` 后缀会把入口挤掉**：69 列 + 未配置密钥时 pill 变成 27 列宽，超出左侧预算被裁断，按"整段画出才给矩形"的规则 `provider_control_rect` 落回 `None`——正是本计划要修的失效模式。改为 Compact 省略后缀（活动行的 `! 供应商未配置密钥` 已说明原因，pill 仍用 Warning 角色），pill 收窄到 12 列并保住入口。
2. **切换器宽度少算 1 列**（既有缺陷）：行文本按 `{marker} {label:<14} {state}` 绘制，`content_width` 只算了 `+3`（行宽 + 两条边框），漏掉 `› ` 标记列，最长的 `Custom compatible 需要 API Key` 被截成 `…Key`。改为 `+4`。

**验收**：新用户不查文档就能从主界面看到并使用供应商面板入口；窄屏（69 列）下入口仍可点；连接状态变化实时反映在页脚。

### Phase 2：面板骨架与字段对齐 —— 已实施

- [x] TUI 侧 `ProviderEditor` 状态（`src/app/provider_editor.rs`）：`rows`、`selected_row`、`pane`、`form`（core 的 `SettingsForm` 草稿）、`field_index`、`delete_confirm`、`window_note`、`error`、以及全部绘制矩形。
- [x] `App` 增加 `provider_editor: Option<ProviderEditor>`；矩形随状态一起创建/销毁，**不再**单独挂在 `App` 上，因此 resize 前的旧矩形不可能被命中。
- [x] `draw_provider_editor` 替换 `draw_provider_list` + `draw_provider_templates` + `draw_provider_form`；左栏恒列内置四家 + 已保存的 custom + `＋ 添加自定义供应商`；`src/app/settings.rs` 整体删除。
- [x] 模型选择器（§4.3）、上下文窗口提示与 `Ctrl+G 获取`（§4.4）、API Key 徽标与说明行、名称必填实时校验。
- [x] 打开界面时 `load_provider_models(false)`；`Ctrl+R` 走后台刷新。
- [x] 应用走 `set_provider_profile`；新建用空 `id` + `custom` 模板；密钥先 `store_api_key_cached` 再应用；应用后从 core 重读，config 只持久化一次。
- [x] 表单字段补鼠标点击（旧的 `SettingsState::Form(_) => {}` 不做任何事）。
- [x] 未连接供应商行置灰 + 选中跳面板：`apply_provider_choice` 对未连接目标改为 `open_settings_at(id)`，状态栏说明原因（Phase 1 遗留项，在此完成）。

**实施记录**：草稿直接复用 core 的 `SettingsForm`——校验、枚举循环、密钥掩码与粘贴语义都在 core 里，TUI 再写一份必然漂移；`ProviderEditor` 只承担表现与导航。选择器复用 `PickerGeometry`，新增 `floating()`：面板内的选择器没有页脚控件可依附，改为贴着给定区域底部浮动，并且**只覆盖正文区**，否则它的边框会压在动作行上。

实际帧打印又抓到三个只看代码看不出、且都在真实使用中会致命的问题：

1. **左栏一行的文字一长，分隔线就错位一格**（用户实测反馈）：原来两栏被拼成同一行 `Line`，只靠 `pad_spans` 把左栏补齐到固定宽度来"保证"分隔列不动；而状态列是按常量 12 预留的，`当前 已配置 ●` 实际占 13 格，于是那一行把分隔线右推一格，和上下行对不齐。改为**两栏各自渲染进自己的 `Rect`**（另加一个只画左边框的 1 列 `Block` 作分隔列），任何一栏的文字超宽只会在自己的矩形里被裁掉，永远推不动分隔线；状态列宽度也改为按当前行集合里最宽的状态动态预留，超宽时让名字先被截断（`...`）而不是让状态被挤掉——状态正是"这行还没配密钥"的唯一提示。
2. **单字母快捷键吞掉输入**：`r` / `g` 被绑定成刷新与取窗口，而 Name / Model / BaseUrl 都是文本行——模型名里的 `r`（`o1-preview`、`gpt-4o-realtime`）和供应商名里的任意 `r`/`g` 都打不进去。实测建名 `MyProvider` 存成 `MyPovide`。改为 `Ctrl+R` / `Ctrl+G`，并在提示文案与选择器标题里同步；单字母只留给确无冲突的模态场合。
3. **`apply_editor` 把刚重载的面板又覆盖回旧草稿**：`commit_editor` 成功后已装入"从 core 重读"的编辑器，旧代码却在之后无条件写回 take 出来的那一份，导致新建后选中行指向已失效的 `＋ 添加` 行、左栏看起来空掉。改为成功后丢弃旧草稿，仅在失败时写回。

另外两处窄屏与边框缺陷：
4. 窄终端栈式布局下右栏矩形宽度为 0，字段完全点不到：栈式分支改为整栏宽。
5. **面板左边框会缺一格（宽字符 skip）**：ratatui 的帧 diff 会给"前一个格子是宽字符"的格子打上跳过标记，于是当面板下面那层 UI（输入框标题 `输入 · 构建`）的宽字符正好落在面板边框左侧一格时，边框那格**永远不被 diff 出来**，屏幕上一直留着一个半个汉字的残影。修法是 `Clear` 比面板多清一圈（`expand_within(popup, 1, area)`），把那个邻居宽字符换成窄空格，边框就能重新参与 diff；同时把边框放在最后绘制，让任何内容都不可能盖住边框格。

**验收**：动态模型与元数据出现在选择器；窗口未知时提示与获取可用；密钥状态徽标与 core `connected` 一致；应用后 config 只持久化一次；每个画出的行都能被它自己的格子命中（单列/双栏两种布局都有测试）；**长名字、长接口地址与最宽状态都不会移动分隔线**（`provider_panel_paints_one_divider_column_for_every_row_shape` 断言每条正文行的 `│` 列集合完全相同）。

### Phase 3：危险操作与校验 —— 已随 Phase 2 一并实施

- [x] `Ctrl+D` 二次确认条（文案含"密钥会保留在系统钥匙串中"）；确认态为模态，只有 `y` / `n` / `Esc` 生效，其他按键被吞掉而不是当成"是"。
- [x] 应用错误内联到面板；`model` 为空或自定义名为空时"应用"禁用（`blocked_reason`）。
- [x] 删除失败同样内联；`remove_provider` 成功后重读 core 并回到当前活动行。

**验收**：删除必须确认；校验失败不发出 core 调用；错误经 `secrets::redact`。

### Phase 4：首页数据源对齐（可选）

按 §4.5 改造 `HomeState` 播种与 `apply_home_selection`。

**验收**：首页看到的 active/saved/connected 与面板一致；无 API Key 泄漏。

### Phase 5：文档与完整验证

- [ ] 更新本文件状态与勾选。
- [ ] 更新 `design/tui-webui-feature-parity.md`：Phase 2–4 标注为"已完成（dec64c6）"，并指向本文件。
- [ ] 若新增不变量（入口契约、pill 文本与矩形同步、选择器分组），同步 `.agents/guides/tui.md`。
- [ ] `bash scripts/check-agent-docs.sh`、`git diff --check`。
- [ ] `cargo fmt --all -- --check`、`cargo test --lib --all-features --locked`。
- [ ] 涉及协议消费/缓存/矩形跨模块时升级：`cargo clippy --all-targets --all-features --locked -- -D warnings`、`cargo test --workspace --all-features --locked`。

```text
Phase 1 入口显著化  ← 本计划核心，可独立交付
  └─ Phase 2 面板骨架与字段
      └─ Phase 3 危险操作
          └─ Phase 4 首页（可选）
              └─ Phase 5 文档 + 完整验证
```

Phase 1 不依赖后续阶段，可单独合并发布，立即解决"找不到入口"。

## 6. 测试计划

| 改动 | 过滤器 |
| --- | --- |
| 入口提示与页脚分段 | `ui_view_model` / `ui::tests::footer` |
| pill 矩形与命中 | `ui::tests::provider` / `app::tests::provider` |
| 未就绪提醒与首屏引导 | `app::tests::provider` |
| 编辑器状态机与行生成 | `app::tests::provider_editor` |
| 模型合并去重与元数据标签 | `app::tests::model` |
| 应用语义（新建/重命名/删除/窗口覆盖） | `app::tests::settings` |
| 面板渲染与命中 | `ui::tests::settings` |
| 切换器分组与滚动命中 | `ui::tests::provider` |
| 首页 | `home::tests` |
| 协议事件回放 | `cargo test --lib --all-features --locked conformance` |

必测行为（Phase 1，已落地）：

- [x] 空闲页脚在标准宽度下同时含 `Ctrl+S` 与 `供应商设置`（`footer_advertises_the_provider_entry_and_drops_the_repeated_mode`）。
- [x] 页脚次行不再出现 mode 字样；provider 与 model 仍在次行（同上）。
- [-] 输入框标题仍可点击切模式：由既有 `ui::tests::input_mode_rect_uses_utf8_width_and_rejects_narrow_inputs` 覆盖，未新增用例。
- [x] provider pill 文本含 `⚙` 与 `▾`，且 `provider_control_rect` 与绘制文本一致（同上）。
- [x] Compact 宽度（69 列）下 `provider_control_rect` 仍为 `Some` 且可点开切换器，未配置密钥时同样成立（`narrow_footer_keeps_the_provider_control_reachable` 的两个分支）。
- [x] 切换器底栏入口可用鼠标与 `Ctrl+S` 打开面板；该行不解析为供应商（`provider_picker_pins_a_settings_entry_that_opens_on_click_and_ctrl_s`、`a_pinned_action_row_never_resolves_an_item_and_never_covers_the_footer`）。
- [x] 切换器最宽一行完整可见（`provider_picker_rows_map_to_the_rendered_provider` 的实际帧断言；宽度 off-by-one 由同一次核对发现）。
- [x] `active ∉ connected` 时 pill 用 Warning 角色并含 `需要 API Key`；`activity_view` 报"供应商未配置密钥"。
- [x] 空会话 + 未连接时首屏出现一次引导条目；重复 sync 不叠加；密钥解析成功后自动撤回（`an_unresolved_key_is_surfaced_once_and_retracted_when_it_resolves`）。
- [-] `/help` 文案包含 `Ctrl+S`：文本改动，无断言。
- [ ] 未连接供应商行置灰 + 选中跳面板（Phase 2）。

必测行为（Phase 2+）：内置四家恒显示；`active`/`connected` 标记与 core 一致；模型合并顺序 动态 → 静态 → 当前 → saved 且按 id 去重；刷新失败保留旧列表且不阻塞取消/审批；窗口空值传 `None`、数字传 `Some` 由 core clamp；自定义名为空时应用禁用；新建自定义始终产生新行；滚动后点击命中的是绘制出的那一行；任何路径下真实 API Key 不出现在渲染文本、日志、错误消息或导出内容中。

## 7. 非目标

- 不复刻 WebUI 的圆角、动效、portal 布局与 CSS。
- 不在 TUI 内实现 HTTP/SSE transport；继续用进程内 `AppHandle`。
- 不在首页做供应商"管理"入口（首页阶段拿不到 `settings`，强行做会绕过 core 权威）。
- 不修改 core 的 `settings.rs`、Provider、Storage 或协议；如需扩展另开 core 改动并 bump rev。
- 不实现 `enabled_models` 可编辑界面（core 尚未在请求期强制）。
- 不做 API Key 明文回显。
- 不用 TUI 本地估算替代 core 的上下文容量决策。

## 8. 完成定义

1. **不查文档的用户能在主界面看到供应商面板入口**，并能用鼠标点到（页脚提示 + pill 控件 + 切换器底栏三处）；70 列以下的窄终端同样能点到。
2. **页脚不再重复 mode**：mode 只出现在输入框标题（仍可点击切换），页脚次行只承载供应商与模型。
3. 密钥未就绪时界面主动提示，而不是等到请求失败。
4. Ctrl+S / `/provider` 打开的是双栏供应商管理界面，内置四家恒可见，状态标记与 core 一致。
5. 面板支持名称、模板（只读）、协议、模型（动态 + 静态 + 元数据）、接口地址、上下文窗口（含获取）、思考能力、只写 API Key；表单字段可点击。
6. 删除有二次确认，应用有禁用态与内联错误，名称校验实时可见。
7. 网络动作全部后台化，事件循环内无长 `await`。
8. 相关单元、conformance、Clippy 与 workspace 测试全部通过；无 API Key 泄漏。