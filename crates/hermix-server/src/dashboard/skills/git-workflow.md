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

## 概述
使用 Git 进行高效的版本控制和团队协作。

## 分支策略
### Git Flow
- main: 生产环境
- develop: 开发环境
- feature/*: 功能开发
- release/*: 发布准备
- hotfix/*: 紧急修复

### Trunk-Based
- main: 主干
- feature/*: 短期功能分支

## 提交规范
### Conventional Commits
```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

类型：feat, fix, docs, style, refactor, test, chore

## 最佳实践
1. **频繁提交** — 小步提交，保持历史清晰
2. **有意义的提交信息** — 描述做了什么，而非怎么做的
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
