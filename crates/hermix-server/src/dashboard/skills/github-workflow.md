---
name: "github-workflow"
display_name: "GitHub 操作流程"
category: "software-development"
description: "通过 gh CLI 做 GitHub 日常操作的纪律：开 Issue、发 PR、看 CI、处理 review 意见的标准序列与红线。用于：'建个 issue''提个 PR''看看 CI 为什么挂''处理一下 review 意见'。不适用于：本地 git 操作（系统提示 GIT_SAFETY_GUIDANCE 已覆盖）。"
version: "0.1.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin", "github", "git", "PR", "issue"]
---

# GitHub 操作流程

## 工具接线

全部经 `terminal` 跑 `gh` CLI（gh 未安装时如实告知，不模拟 API）：

- `gh issue list` / `gh issue create --title ... --body ...`
- `gh pr create --title ... --body ...` / `gh pr view <n>` / `gh pr diff <n>`
- `gh run list --limit 5` / `gh run view <id> --log-failed`（CI 失败先取
  失败日志，别盲猜）
- `gh api ...`（以上命令覆盖不了的场景）

git 本地操作（add/commit/branch）与系统提示 GIT_SAFETY_GUIDANCE 分工：
本地 git 走那份纪律，GitHub 平台操作走本技能。

## 标准操作序列

### 发 PR（默认序列）
1. `gh pr diff` 或 `git diff main...HEAD` 自查改动全集
2. PR 描述三段式：**改了什么**（逐条，不是贴 diff）/ **为什么**（动机/关联
   issue 号）/ **怎么验证**（跑过什么命令、结果如何）
3. `gh pr create`，自查列表里只含预期文件
4. 提交后 `gh pr checks` 确认 CI 起跑了

### CI 失败（默认序列）
1. `gh run view <id> --log-failed` 取失败日志——先看是哪一步、什么断言
2. 判断：真失败（修代码）vs 偶发（重跑 `gh run rerun <id>`，说明判断依据）
3. 盲目 re-run 超过一次 = 违规

### 处理 review 意见
逐条回应：同意就改并单独提交；不同意就给出理由回复，不静默忽略。
**每次 push 后告知用户**改了什么。

## 红线（与 GIT_SAFETY 协同）

- ❌ 不 force-push、不改已 push 的提交历史
- ❌ 不绕过 branch protection / review 要求
- ❌ PR 描述不贴整段 diff——diff 平台自己会渲染
- ❌ 不在公开仓库泄露密钥/内网地址

## 输出契约

每次操作回执：做了什么（issue/PR 编号 + 链接）、CI 状态、下一步等谁
（review/合并）。遵循系统提示 REPORT_SECTIONING 分节规则。
