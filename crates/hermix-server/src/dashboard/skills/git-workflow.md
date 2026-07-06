---
name: "git-workflow"
display_name: "Git 工作流"
category: "software-development"
description: "Git 版本控制最佳实践，包括分支策略、提交规范、代码审查。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# Git 工作流 Skill

## 触发条件
当用户要求以下操作时使用此 Skill:
- "git" / "提交" / "commit"
- "分支" / "branch"
- "合并" / "merge" / "rebase"
- "版本控制" / "version control"

## 执行步骤
1. 检查当前 Git 状态（分支、暂存、未提交）
2. 根据任务选择操作（提交/分支/合并）
3. 执行操作并验证结果

## 提交规范
### Conventional Commits
```
<type>(<scope>): <description>

[optional body]

[optional footer]
```
类型: feat, fix, docs, style, refactor, test, chore

## 最佳实践
1. **频繁提交** — 小步提交，保持历史清晰
2. **有意义的提交信息** — 描述做了什么
3. **原子提交** — 每个提交只做一件事
4. **提交前审查** — 检查 diff，确保无误

## 常用命令
```bash
git add -p          # 交互式暂存
git commit -m "..."  # 提交
git rebase -i       # 交互式变基
git bisect          # 二分查找 bug
git stash           # 暂存更改
```

## 输出格式
```
## Git 操作结果
### 状态
- 分支: [branch]
- 变更: [文件列表]

### 操作
- [执行的 git 命令]
```
