---
name: "tdd-practice"
display_name: "测试驱动开发"
category: "software-development"
description: "RED-GREEN-REFACTOR 循环开发与补测：先写失败测试再写实现，或为既有代码补行为测试。用于：新功能开发、修 bug 前锁定行为、重构前搭安全网。不适用于：一次性脚本（测试成本高于收益）、性能基准（用基准工具）。"
version: "0.2.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin", "testing"]
---

# 测试驱动开发

## 工具接线（本仓库验证命令）

1. `terminal` 跑测试，失败原因必须是"断言失败"而非"编译错误"——编译错误
   不算 RED：
   - Rust 单 crate：`cargo test -p <crate> --lib`
   - Rust 全量：`cargo test --workspace --lib`
   - TS：`cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit`（类型层）
   - Python：`python -m pytest <目标> -x -q`
2. 改动前先确认现状测试是绿的；在红的基础上继续写码 = 看不懂结果。
3. 大范围补测（> 10 个用例）走 `delegate_task`，task 给行为清单，让子代理
   起草用例再人工收敛。

## RED-GREEN-REFACTOR 纪律

1. **RED**：写一个失败的测试，用自然语言命名的行为做测试名
   （`test_user_cannot_login_with_wrong_password`）。跑一次确认失败原因
   符合预期。
2. **GREEN**：写**最少**的代码让测试通过，不追求完美。
3. **REFACTOR**：消除重复、改善命名，每步重跑测试。

循环要小（分钟级）；大功能拆成多个小循环，每轮循环对应一次提交粒度的进展。

## 测试五原则

小而专注（一测试一行为）、独立（不共享状态不依赖顺序）、快速（单元毫秒级）、
确定性（同输入同输出）、自描述（测试名即行为文档）。

## 何时停止写测试

边界条件与错误路径全覆盖、边际收益递减时停。测试也是代码，同样要维护——
不给私有实现细节写测试，给行为写。

## 反例（不要这样做）

- ❌ 跳过 RED 直接写实现——没有失败过的测试等于没测试
- ❌ GREEN 阶段顺手优化——先通过，重构阶段再说
- ❌ Mock 一切——真实行为比 mock 可靠，只在边界（网络/时钟/文件系统）mock
- ❌ 一个测试验证五个断言链——拆成独立小测试

## 集成

- `debugging-methodology`：修 bug 先写复现测试（RED）再修（GREEN）
- `code-review`：测试覆盖是审查项
- 参考：Kent Beck《Test Driven Development: By Example》
