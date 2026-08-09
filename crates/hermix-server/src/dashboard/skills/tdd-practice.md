---
name: "tdd-practice"
display_name: "测试驱动开发"
category: "software-development"
description: "Use when implementing test-driven development (TDD) workflow — writing tests before code, enforcing the RED-GREEN-REFACTOR cycle, or improving test coverage of existing code."
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin"]
---

# 测试驱动开发 Skill

## When to Activate

Use this skill when:
- 需要按 TDD 流程开发新功能（先写测试，再写实现）
- 需要对现有代码补充测试（先理解行为，再补测试）
- 需要重构代码并确保行为不变（测试作为安全网）
- 需要评审测试质量（测试是否小而专注、独立、快速）

Do not activate for:
- 纯探索性编程（先 prototype 再补测试，不算 TDD）
- 脚本/一次性工具（测试成本高于收益）
- 性能基准测试（用 benchmark 工具，不是 TDD）

## Core Concepts

### RED-GREEN-REFACTOR 循环

```
RED   → 写一个失败的测试（定义期望行为）
GREEN → 写最少的代码让测试通过（不追求完美）
REFACTOR → 重构代码，保持测试通过（消除重复、提升可读性）
```

每一轮循环应该很小（几分钟到十几分钟），大的功能拆成多个小循环。

### 测试原则

1. **小而专注**：每个测试只验证一个行为
2. **独立**：测试之间不共享状态、不依赖执行顺序
3. **快速**：单元测试毫秒级，集成测试秒级
4. **确定性**：同样的输入，同样的输出，不依赖外部状态
5. **自描述**：测试名就是行为描述（`test_user_cannot_login_with_wrong_password`）

### 测试金字塔

```
        /  E2E  \        ← 少量，验证关键路径
       / 集成测试 \       ← 中量，验证模块协作
      /  单元测试   \      ← 大量，验证单个函数/方法
```

## Practical Guidance

### TDD 五步法

1. **写失败测试**：用自然语言描述期望行为，运行确认失败
2. **看测试失败**：确认失败原因是你预期的（不是编译错误）
3. **写最少代码**：让测试通过，不做多余的事
4. **看测试通过**：确认通过
5. **重构**：消除重复、改善命名、提取函数，每步都跑测试

### 何时停止写测试

- 当你对"这段代码的行为"有信心时
- 当继续写测试的边际收益递减时
- 当所有边界条件和错误路径都被覆盖时

### Rust 中的 TDD

```rust
// RED: 先写测试
#[test]
fn test_add_positive_numbers() {
    assert_eq!(add(2, 3), 5);
}

// GREEN: 再写实现
fn add(a: i32, b: i32) -> i32 {
    a + b
}

// REFACTOR: 如果有重复，提取函数
```

### JavaScript/TypeScript 中的 TDD

```typescript
// RED
test('calculate total with tax', () => {
  expect(calculateTotal(100, 0.1)).toBe(110);
});

// GREEN
function calculateTotal(price: number, taxRate: number): number {
  return price * (1 + taxRate);
}
```

## Gotchas

- 不要跳过 RED 步骤直接写代码——没有失败的测试等于没有测试
- 不要在 GREEN 步骤追求完美代码——先通过，再重构
- 不要让一个测试验证太多东西——拆成多个独立测试
- Mock 不要过度使用——真实行为比 mock 更可靠
- 测试代码也是代码，也需要重构和维护

## Examples

```python
# Python + pytest TDD 示例
# RED
def test_empty_list_returns_zero():
    assert total([]) == 0

# GREEN
def total(items):
    return 0

# RED (下一轮)
def test_single_item():
    assert total([10]) == 10

# GREEN
def total(items):
    if not items:
        return 0
    return items[0]

# 逐步迭代直到全部通过...
```

## Integration

- 与 `refactoring` skill 配合：重构时 TDD 是安全网
- 与 `debugging` skill 配合：先写复现 bug 的测试（RED），再修复（GREEN）
- 与 `testing` skill 配合：TDD 是测试策略的一种，测试 skill 提供更多测试技术
- 与 `planning` skill 配合：大功能拆成多个 TDD 循环的计划

## References

- Kent Beck《Test Driven Development: By Example》
- Rust: `cargo test` 内置 TDD 支持
- JavaScript: Jest / Vitest 提供 watch mode 适合 TDD
- Python: pytest + `--looponfail` 自动重跑失败测试
