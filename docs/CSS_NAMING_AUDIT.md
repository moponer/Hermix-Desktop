# CSS 选择器命名一致性审计（2026-08-30）

## 现状摘要

- **总选择器数**：1212 条（不含 @media/@keyframes）
- **主文件分布**：11 个 CSS 文件（base / code-view / components / components-* / globals / inline-* / preview-md-list / themes / tokens / utilities）

## 命名风格分布

| 风格 | 数量 | 占比 | 代表 |
|---|---|---|---|
| `kebab-case` + 命名空间前缀 | ~900 | 74% | `preview-pane-*` / `prose-*` / `modal-*` |
| `kebab-case` + 项目前缀 | ~120 | 10% | `tw-msg-*` / `prefetch-chip` / `approval-btn` |
| 裸 `kebab-case` 短名 | ~30 | 2.5% | `btn` / `icon-btn` / `nav-item` |
| BEM（仅 1 处用） | 6 | 0.5% | `stream-error-card__detail` |
| 其他（utility/状态） | ~156 | 13% | `.is-open` / `.dark` 等 |

## 不一致点（按严重度）

### P0：抽象层级不一致
**`btn` 短名** vs **`approval-btn` / `modal-btn` / `icon-btn` 长名** 混用。
- `btn`（8 处）—— 抽象"按钮"基类
- `approval-btn`（10 处）—— 同语义变体
- 同层语义应该同名 / 或都不带前缀

### P1：同语义不同名
- `sidebar-topnav` / `sidebar-row-main`（带 sidebar 前缀）
- `nav-item` / `tool-row`（裸短名）
- **都是导航/列表项语义，但用了不同命名空间**

### P1：BEM 仅 1 处使用
`stream-error-card__detail` —— 是孤立约定，迁就它没意义

### P2：项目前缀不一致
- `tw-msg-*`（说明这原本是 tailwind 类名演化来的）
- `prefetch-chip`（无前缀）
- `approval-btn`（无前缀）

## 不建议批量执行的原因

| 风险 | 影响 |
|---|---|
| 改 CSS 选择器 ↔ 改 markup className | 80+ 文件 markup sed 级操作 |
| **tsc 不验证 CSS className 引用** | 改完只有 grep 能发现死链 |
| Vite 不会因 CSS 死链报错 | 视觉回归只能肉眼验证 |
| 当前 `kebab-case` 是业界主流 | 不是坏味道，是"没那么严格" |

**遵循原则**：打磨期动视觉可见的（颜色/尺寸/间距/动效），命名一致性是**重排期**的事。

## 如果未来真要重命名，推荐方案

### 阶段 A：先建立 BEM 边界
- **不强制全项目统一**，只在新增组件时按 BEM 写
- 老的 `kebab-case` 选择器维持原状
- 等所有新组件都 BEM 后，再批量迁移老的选择器

### 阶段 B：批量重命名（如果一定要做）
1. 选定目标命名空间（推荐：`prefix-block__element--modifier`，prefix 是组件短名）
2. 写 Python 脚本同时改 CSS + 改 markup（grep 反查所有 className 引用）
3. 改完后跑 tsc + build + test，**外加**：用 puppeteer/headless 截全应用图，肉眼比对

### 阶段 C：移除冗余
- 删除从未在 markup 引用的死选择器（**已在上一次任务做过**：删 441 个）
- 删除与全局 utility 重复的类（如 `.flex` 自定义类 vs Tailwind `flex`）

## 当前建议

- **不动代码**，保留审计文档作为未来重排期的参考
- 新增组件时按 BEM 写（最小代价建立边界）
- 老的 `kebab-case` 不强制统一，避免大规模回归风险