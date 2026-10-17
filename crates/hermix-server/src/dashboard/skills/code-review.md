---
name: "code-review"
display_name: "代码审查"
category: "software-development"
description: "对已写代码做分级审查：先跑项目自带静态检查工具，再按正确性>安全>可维护性人工复核，产出带文件:行号与修复方向的报告。用于：用户要求审查/复查/评估刚写完或指定的代码、提交前检查。不适用于：从零设计新代码（走 architecture-design）、调试运行时错误（走 debugging-methodology）。"
version: "0.2.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin", "review"]
---

# 代码审查

## 工具接线（先机检后人审）

1. `terminal` 跑项目自带静态检查，工具零输出 = 该项通过，不要口头复述：
   - Rust：`cargo clippy --workspace --all-targets 2>&1 | tail -50`
   - TypeScript：`cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit`
   - Python：`python -m ruff check <目标>`（无配置则跳过机检，直接人工审）
2. `read_file` 带 offset/limit 分段读目标代码，单次 ≤ 500 行；目标 > 1500 行
   时改走 `delegate_task`（Explore 角色），把本清单作为 task 传下去。
3. 只报机检和人工都确认的问题；机检已报的不重复定性，只补影响面。

## 检查清单（每项先想反例再下结论）

### 正确性（必须过）
- 边界条件：空集合、零值、超长输入、并发访问
- 错误路径是否吞掉失败状态——反例：把 `unwrap_or_default()` 当成功路径上报
- 改动是否破坏调用方契约（函数签名/返回值/事件语义变更未同步）

### 安全（必须拦，不许降级为"建议"）
- 注入面：SQL 拼接、shell 拼接、路径拼接用户输入
- 越权：操作是否校验了属主/权限，还是只校验了登录
- 密钥硬编码、敏感信息进日志

### 可维护性（只报真问题）
- 只报会真实坑下一个人的问题（错误处理缺失、状态不一致、命名误导），
  不凑风格分；风格问题留给 linter

## 输出契约

按严重度三级：🔴 必须修复（错误/安全）、🟡 建议修复（隐患/坏味道）、
🔵 可选（风格，默认不报）。每条 = 文件:行号 + 为什么是问题 + 最小修复方向。
结尾一句总评（可合并 / 需修改 / 需讨论）。整体报告遵循系统提示
REPORT_SECTIONING 分节规则。审查的是代码不是人：只评代码，不评判作者；
看不出意图先问一句，不臆测需求。

## 反例（不要这样做）

- ❌ 只报问题不给修复方向——用户拿到的是清单不是方案
- ❌ 把机检已报的问题原样复述当自己的发现
- ❌ 顺手建议重构无关代码——保持 diff 聚焦
- ❌ 测试覆盖低就阻断——提示即可，不阻断合理 PR

## 集成

- `debugging-methodology`：审查发现的 bug 走调试流程
- `tdd-practice`：测试覆盖是审查项；修复补测试
- 参考：OWASP Top 10、各语言 linter 官方规则集
