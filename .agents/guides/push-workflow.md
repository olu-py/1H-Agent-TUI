# GitHub PR 与本地同步指南

## 适用范围

适用于 Core、TUI、WebUI 的日常代码交付。三个仓库独立维护；日常改动统一通过功能分支和 PR 进入 `main`，版本号与 tag 仍分别管理。

## 入口

- 开始前确认仓库根目录、remote、默认分支与 `git status --short --branch`；保护已有改动，再 fetch `origin/main` 并从最新 main 创建功能分支。
- 只暂存本次文件；检查完整 staged diff 和 `git diff --cached --check`，按对应指南运行验证，再 commit 并记录 SHA。
- 推送功能分支（首次用 `git push -u origin HEAD`），确认本地 SHA 与 upstream SHA 相同。
- 创建 PR 指向 `main`；检查 head/base、diff、required checks、评审、冲突和仓库允许的合并方式。全部通过且用户授权后再合并。
- 合并后读取 PR 的 merged 状态和 merge SHA；本地 fetch、切换 `main`、`git pull --ff-only origin main`，确认 `HEAD` 等于 `origin/main` 且工作区干净。

> AI agents: 本仓库 commit/push、PR 创建/合并和合并后同步任务可使用项目 Skill [`git-pr-local-sync`](../skills/git-pr-local-sync/SKILL.md)，也可调用 `$git-pr-local-sync`；未发现 Skill 时按本指南执行。

## 不变量

- `main` 只接收已通过 PR 检查并合并的改动；不直接 push `main`，不使用 `--force` 掩盖分歧，不绕过 required checks 或保护规则；只有确认是个人功能分支、无人依赖且用户请求与项目策略允许时，才考虑 `--force-with-lease`。
- 合并策略服从仓库设置；squash/rebase 后 head SHA 与 merge SHA 可不同。PR closed 不代表 merged。
- GitHub 插件用于读取 PR/CI 和执行获准的 GitHub 操作；本地 Git 命令负责本地提交与分支同步。
- core/TUI 联调保留 `--config` 临时 path patch；正式交付移除覆盖，先合并 core，再在 TUI 单独建 PR 更新锁文件和适配。

## 诊断

| 阻塞 | 处理 |
| --- | --- |
| main 或功能分支落后 | fetch 并确认差异；只在可快进时同步，分叉时保留提交并查明原因。 |
| PR checks/评审/合并阻塞 | 按 GitHub 返回的具体规则修复并重新验证，不绕过门禁。 |
| 合并成功但本地不一致 | fetch、切换 main、尝试 `--ff-only`；失败时不 reset，先查明本地提交和改动。 |
| 凭据或权限异常 | 使用已授权的连接重试；不把 token 写入命令、文件、日志或回复。 |

## 验证

- 提交前检查 staged diff 与 `git diff --cached --check`，按 [Release](release.md) 及任务相关指南运行验证。
- 合并前确认 PR checks 全绿；合并后确认 PR merged、`HEAD == origin/main`、工作区干净，并记录本地与远端 SHA。
