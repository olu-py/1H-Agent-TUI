# TUI 模块入口

共同维护规则见[工作区 AGENTS.md](../AGENTS.md)。本仓库负责终端应用、事件循环、projection、渲染和输入；Git 历史、CI 与发布保持独立。

按任务阅读[TUI 专题指南](../.agents/guides/tui/)，再核对其中指向的源码和测试。修改适配层时保留终端输入、渲染和跨平台行为。

Git 交付使用[共享工作流 Skill](../.agents/skills/git-pr-local-sync/SKILL.md)。