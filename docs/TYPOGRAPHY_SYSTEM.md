# 字体规范 v1

> 2026-09-28 · 已落地到 `tokens.css` / `themes/dark.css` / `components-helpers.css`
> 标本屏：[`prototype/index.html`](prototype/index.html) 屏 ⑦ → `shots/07-typography.png`

---

## 1. 体检结论：原来没有全局控制

你问"是否有全局对字体大小样式的控制"——**没有**。三个证据：

### 1.1 语义档位是死的

`tokens.css` 原本定义 6 档：

```css
--font-size-display: 22px;  --font-size-h1: 16px;  --font-size-h2: 14px;
--font-size-body: 14px;     --font-size-meta: 12px; --font-size-tiny: 11px;
```

这是 **Tailwind v3 的命名**。Tailwind v4 只有 `--text-*` 命名空间才生成
`text-*` 工具类，`--font-size-*` **不生成任何工具类**。所以：

```
text-tiny / text-meta / text-body / text-h1 / text-h2 / text-display
在 45 个 TSX 文件里出现 0 次
```

真正生效的是硬编码的任意值：

| 任意值 | 出现次数 |
|---|---|
| `text-[11px]` | 194 |
| `text-[13px]` | 104 |
| `text-[12px]` | 93 |
| `text-[15px]` | 8 |
| 其余零星（14 / 16 / 20 / 12.5 / 11.5） | 10 |

合计 **409 处**，分布在 45 个文件。

### 1.2 CSS 里另有 60+ 处半档

`12.5px` 35 处、`11.5px` 24 处、`10.5px` 15 处、`13.5px` / `14.5px` 各若干。
连同上面的任意值，实际存在 **11 个正文级字号**（10 / 10.5 / 11 / 11.5 / 12 /
12.5 / 13 / 13.5 / 14 / 14.5 / 15），加上 16–22px 的标题级。
这就是"某个组件特别小、某个特别大"的直接来源——没有档位，就没有约束。

### 1.3 字重完全没有体系

`tokens.css` 里没有任何字重令牌，组件全靠 Tailwind 默认或随手写。

### 1.4 思考与正文同字号同色（你点名的那条）

`components-helpers.css:408` 原本是：

```css
.tl-reasoning-row {
  color: var(--color-text-primary);        /* 与正文完全相同 */
  font-size: var(--font-size-body, 14px);   /* 与正文完全相同 */
  line-height: 1.72;
}
```

注释甚至写着"视觉上就是一段普通消息文本"。过程和结论在同一条视觉通道上，
读者只能靠左侧竖线猜。

### 1.5 没有任何机制阻止复发

`scripts/check-theme-tokens.mjs` 检查主题契约、孤儿令牌、裸色、Tailwind 原生色阶，
**唯独不检查字号**。所以体系就算建好，也会被新的 `text-[11px]` 慢慢冲垮。

---

## 2. 已落地的体系

### 2.1 字号 7 档（`--text-*` 命名空间，生成工具类）

| 令牌 | 值 | 唯一用途 | 当前使用处数 |
|---|---|---|---|
| `--text-micro` | 10px | 角标、计数徽标 | 待迁移 |
| `--text-tiny` | 11px | 元信息：时间、计数、路径、参数预览 | 待迁移 |
| `--text-meta` | 12px | 工具名、chip、命令输出、区块标题 | 待迁移 |
| `--text-ui` | 13px | 界面文本：按钮、列表项、表单标签、**思考正文** | 1 处（思考） |
| `--text-body` | 14px | **对话正文（全局唯一正文字号）** | 待迁移 |
| `--text-title` | 16px | 小标题：区块、弹窗 | 待迁移 |
| `--text-display` | 20px | 页面标题 / 空态（一次性） | 待迁移 |

旧名 `--font-size-*` 保留为别名供 `var()` 引用，注释已标注"禁止新代码使用"。

**规则**：一屏内字号档位 ≤ 3。禁半档（11.5 / 12.5 / 13.5 …）。

### 2.2 字重 3 档（原来没有）

| 令牌 | 值 | 用途 |
|---|---|---|
| `--font-weight-regular` | 400 | 正文 |
| `--font-weight-medium` | 500 | 界面强调：按钮、选中项、工具名 |
| `--font-weight-semibold` | 600 | 标题：区块 / 轮次 / 弹窗 |

**最高只到 600**——中文在 13px 以下用 700 会糊成色块，这是硬约束。

### 2.3 行高 4 档（按用途，不按字号）

| 令牌 | 值 | 用途 |
|---|---|---|
| `--leading-reading` | 1.75 | 长文阅读：对话正文、思考正文 |
| `--leading-ui` | 1.5 | 界面文本：按钮、chip、列表 |
| `--leading-compact` | 1.4 | 紧凑：工具行、元信息、表格 |
| `--leading-heading` | 1.3 | 单行标题 |

### 2.4 文本色 6 档

原有 5 档（`text-primary/secondary/tertiary/muted/faint`），新增一档
**专供思考**：

| 主题 | `--color-text-thinking` | 位置 |
|---|---|---|
| 宣纸 | `#8f897c` | ink-3 与 ink-4 之间 |
| 夜墨 | `#7a7e86` | ink-3 与 ink-4 之间 |

理由：过程不是内容，不该和结论抢同一条视觉通道。

### 2.5 思考块已改

```css
.tl-reasoning-row {
  color: var(--color-text-thinking);       /* 浅两档 */
  font-size: var(--text-ui, 13px);         /* 小一档 */
  line-height: var(--leading-reading, 1.75);
}
```

### 2.6 校验器新增 `type-debt`

`scripts/check-theme-tokens.mjs` 现在会扫出所有 `text-[Npx]` 任意值：

```
[type-debt] 407 处任意字号 text-[Npx]（分布于 45 个文件，仅告警）— 目标：全部迁到
text-micro/tiny/meta/ui/body/title/display 七档后在脚本中改为阻断
```

品牌/资产类一次性尺寸（20/22/24/28/32/36/40/48/72px）走白名单。
当前是 WARN 模式（与既有 `palette-debt` 同一策略），迁移完成后改为 `exit(1)` 阻断。

---

## 3. 角色对照表

组件**只能**从这里取，不允许自选：

| 内容 | 字号 | 字重 | 颜色 | 行高 |
|---|---|---|---|---|
| 对话正文 | `body` 14 | 400 | `text-primary` | 1.75 |
| 用户气泡 | `body` 14 | 400 | `text-primary` | 1.68 |
| **思考正文** | `ui` 13 | 400 | **`text-thinking`** | 1.75 |
| 轮次摘要行 | `meta` 12 | 500 | `text-secondary` | 1.4 |
| 工具名 | `meta` 12 mono | 500 | `text-primary` | 1.4 |
| 参数 / 路径 | `tiny` 11 mono | 400 | `text-tertiary` | 1.4 |
| 命令输出 | `meta` 12 mono | 400 | `text-secondary` | 1.6 |
| 时间 / 计数 | `tiny` 11 | 400 | `text-tertiary` | 1.4 |
| 区块标题 | `meta` 12 | 600 | `text-tertiary` + 字距 .04em | 1.4 |
| chip 文本 | `meta` 12 | 500 | `text-secondary` | 1.4 |
| 按钮 | `ui` 13 | 500 | `text-primary` | 1.4 |
| 占位 / 禁用 | `tiny` 11 | 400 | `text-faint` | 1.4 |

---

## 4. 与用户级缩放的关系

`utils/uiScale.ts` 的缩放走**根元素 `zoom`**，与本体系正交：

- 字号阶梯定的是**相对关系**（谁比谁大几档）
- `zoom` 定的是**绝对大小**（用户整体放大 125%）

两者不冲突，不需要为缩放再开一套字号。迁移到语义档位后，缩放会自动作用于
全部组件——这正是现在做不到的：`text-[11px]` 是硬编码像素，`zoom` 只改布局
尺寸，两者叠加会让小字在 125% 下显得过密。

---

## 5. 迁移计划（尚未执行）

409 处任意值建议**按文件批量**迁移，不要一次性全局替换——不同文件里的
`text-[11px]` 语义并不相同（有的是元信息，有的是工具行，有的是 chip）。

| 批次 | 范围 | 建议 |
|---|---|---|
| 1 | 对话链路：`MessageList` / `MessageBubble` / `StreamingBubble` / `ToolCallBubble` / `Composer` | 最影响观感，先做 |
| 2 | 侧栏与导航：`Sidebar` / `TitleBar` / `UserMenuPopover` | 次之 |
| 3 | 设置与各页面：`*Tab` / `*View` | 最后 |
| 4 | CSS 里的半档（12.5 / 11.5 / 10.5 / 13.5 / 14.5） | 并入上面对应批次 |
| 5 | `type-debt` 改为 `exit(1)` 阻断 | 迁移完成后 |

每批跑一次 `node scripts/check-theme-tokens.mjs` 看数字下降，
`text-[Npx]` 计数归零后开阻断。
