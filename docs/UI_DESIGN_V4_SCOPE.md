# 界面方案 v4 · 真实剩余范围

> 2026-09-28 · 取代 `UI_DESIGN_V3_REVISED.md`
>
> **落地状态（当晚）**：A / B / C / D 四批全部完成，字体体系已落地。
> 验证：`pnpm tsc` 通过 · 主题契约 73/73 · vitest 172/172。
>
> | 批次 | 状态 | 实际落点 |
> |---|---|---|
> | A 正文行宽 640 | ✅ | 新增 `--width-reading` 令牌 + `max-w-reading` 工具类，替换 MessageList(2处)/ChatView(1处) |
> | B 摘要行补计数 | ✅ | ToolCallBubble 统计 tool_complete 行数；摘要行字体归位 meta/500/secondary，统计量 tiny/tertiary/mono |
> | C 产物栏折叠态 | ✅ | 新组件 RightRail（44px 竖条 + 朱砂角标），栅格第三列 `--col-right: 44px`，<900px 视口自动撤掉 |
> | D 工作台在场球 | ✅ | 新组件 FloatingChat：可拖球（比例位置记忆）+ 可拖可缩浮窗（像素尺寸记忆）+ Esc 收起 + 完成态对勾；删除旧固定右栏与折叠钮 |
> | 字体体系 | ✅ | 7 档字号（`--text-*` 修好命名空间）+ 3 档字重 + 4 档行高 + 思考专用色；54 处 TSX 迁移（407→353），77 处 CSS 半档清零 |
>
> 遗留：353 处 TSX 任意字号待分批迁移（settings / 各 View 页面），归零后 `type-debt` 开阻断。
>
> **追加上线（同日晚）**：
> - **accent 换色**：朱砂橙（与 Claude 同质）→ 青碧 `#1d8a72`（暗 `#35b899`）；
>   success 往黄绿挪（`#35854a`/`#5cb176`）保持 ≥30° 分离；workspace chip 改 info 蓝灰。
> - **审批抽屉**：待决定审批不再内联渲染在消息流里（`ToolCallBubble`/`MessageBubble`
>   的 pending 分支返回 null），统一由新组件 `ApprovalDrawer` 在**输入框上方**弹出：
>   风险 chip + 工具动词 + 目标预览 + 可折叠 diff + 三个选项
>   （拒绝 N / 会话内允许 A / 允许一次 ⏎）；批量审批同抽屉（列出前 3 项）。
>   已决定的历史卡片仍内联渲染（那是记录）。快捷键在焦点位于输入类元素时
>   不生效——打字打出 "y" 变成"允许"是绝对不可接受的事故。
>   `hasPendingApproval` 改为统一扫描（legacy + unified_steps 两路），keepOpen /
>   activeTurnMsgIds / 抽屉共用同一份事实。
> 原型：[`prototype/index.html`](prototype/index.html)
>
> **v4 的唯一变化：把"已经有"的实现从待办里划掉。**
> 用户指出最终结果输出后所有过程消息自动折叠——核查代码确认这已是既有实现，
> v3 里三条"提议"属于重复劳动。本文档只列真正还没做的部分。

---

## 1. 核查结论：以下三条已实现，不动

### 1.1 轮次结束后过程全折叠（已实现）

`ToolCallBubble.tsx:162` `ProcessTimeline`：

```ts
const expanded = isStreaming || keepOpen || hasRunningTool ? true : (userOverride ?? false);
```

- 流式中 → 展开
- `keepOpen`（等审批 / loop 暂停）→ 展开
- `hasRunningTool`（工具仍在跑）→ 展开
- **真正结束 → 默认折叠**，摘要行显示耗时（`durationMs`，Done 事件打戳）
- 用户手动展开的 `userOverride` 保留到下一轮开始（`useEffect` 在 `!isStreaming` 时清空）

三条保护（有审批不折、工具在跑不折、手动展开保留）都是对的，**一行不改**。

### 1.2 工具行 rail + 折叠输出（已实现）

`components.css:1211` `.tool-rail` / `.tool-row` / `.rail-dot`：竖线 + 7px 状态点，
成功空心绿、运行呼吸、失败实心红；`ToolGroupCard` 负责分组与输出折叠。

### 1.3 审批卡（已实现且完整）

`inline-approval-card.css`（428 行）：accent 实色主按钮 + error-soft 反按钮 +
accent 焦点环 + 离场动画 + `is-resolved` 终态。配色换新令牌后自动跟随。

---

## 2. 真正剩下的四件事

### 2.1 正文行宽 920px → 640px（唯一实在的代码改动）

`MessageList.tsx:321`：
```
className="msg-stream-list max-w-wide lg:max-w-[800px] xl:max-w-[920px]"
```

14px 字号下：

| | 每行汉字数 | 判断 |
|---|---|---|
| 现状 920px | ≈ 65 | 超出中文舒适行长（35–45），回扫易串行 |
| 提案 640px | ≈ 38 | 舒适区上沿 |

**只改正文这一层**：代码块、表格、长命令行保持原宽并允许横向滚动。
用户气泡跟随 measure 收窄（现 `max-width: 80%`）。

改动量：`MessageList.tsx` 一处 class + `components-prose.css` 的 `.prose` 兜底宽度。
对照屏：原型屏 ③。

### 2.2 轮次摘要行补两个计数（微调）

现在是「已工作 X 分 Y 秒」一行。建议右侧补「N 个工具 · M 行」，
让折叠态也能判断这一轮分量，不必点开。样式走新令牌，位置与点击区不变。

### 2.3 产物栏折叠态（新增）

保持停靠（320–520px 可拖、顶部 `产物/电脑` tab、列表项带类型图标与
「改写 · 18 KB」）。只增加折叠态：

- 44px 竖条，顶部 30px 圆角图标 + 朱砂数量角标 + 竖排「产物」二字
- 收起后留在原位，点一下原路展开，宽度记忆
- 不引入浮层、不引入遮罩、不改 tab 结构

对照屏：原型屏 ⑤。

### 2.4 工作台悬浮在场球（新增，独立 PR）

`workbench.css` 现状是「左画布 + 右 460px 固定栏」，画布被永久切掉三分之一。
改为 48px 可拖拽悬浮球：画布拿回 100% 宽度，球跨页面常驻，外环直接读出
agent 状态（空闲无环 / 思考=旋转虚线弧 / 执行=不确定态脉冲 / **待审批=芥黄，
只呼吸一次** / 刚完成=对勾划过）。

---

## 3. 已落地的两项（不再重复）

| 项 | 内容 |
|---|---|
| 配色重做 | 主题砍到 **宣纸 / 夜墨** 两套（删 rainforest·sunset）；卡片恒比背景亮（旧版 `--color-bg-card` 与 `--color-bg-primary` 同为 `#faf9f5`，卡片根本没抬起，层次全靠边框 → 满屏盒子感）；正文对比降到约 11.5:1；暗色推翻"中性轴必须 R=G=B"改为饱和度上限 4%，底色 `#0b0b0b` → `#121419`。**accent 按用户要求从朱砂橙（与 Claude 同质）改为青碧 `#1d8a72`（暗色 `#35b899`）**，success 相应往黄绿挪（`#35854a`/`#5cb176`）保持 ≥30° 色相分离，workspace chip 改走 info 蓝灰避免与 accent 撞车。**契约校验 73/73 通过** |
| 外壳 CSS 去重 | `components.css` 有一段与 `components-layout.css` 重复的外壳样式（`.app-shell/.app-body/.pane*/.titlebar*`，约 160 行），因引入更晚一直覆盖掉每套主题的场景色（`--theme-bg-sidebar/titlebar`）——四套主题看起来一样的根因。已删 |

---

## 4. 批次

| 批次 | 内容 | 规模 | 依赖 |
|---|---|---|---|
| A | 正文行宽 920 → 640 | ~10 行 | 无 |
| B | 轮次摘要行补计数 | ~20 行 | 无 |
| C | 产物栏 44px 折叠态 | ~120 行 | 无 |
| D | 工作台悬浮在场球 | ~300 行 | 无（独立 PR） |

四批互不依赖，可分别验证、分别回滚。

---

## 5. 原型索引

| 屏 | 文件 | 看什么 |
|---|---|---|
| ① | `shots/01-chat-light.png` | 宣纸对话页：原样侧栏分组 + 原样输入栏 + 收窄正文 + 折叠的轮次摘要行 |
| ② | `shots/02-chat-dark.png` | 夜墨同款 |
| ③ | `shots/03-line-width.png` | 920px vs 640px 同一段文字对照 |
| ④ | `shots/04-turn-collapse.png` | 轮次折叠的现状判定（已实现）+ 唯一想微调的摘要行 |
| ⑤ | `shots/05-artifact-collapse.png` | 产物栏展开态 vs 44px 折叠态 |
| ⑥ | `shots/06-workbench-orb.png` | 工作台画布 + 在场球（芥黄环 = 待审批） |
