---
status: implemented
class: process
date: 2026-10-05
---

# 工具配置面板去描边 + 图标统一主色

## Decision

`ToolTabs.tsx`（工具集 / 技能 / 插件 / MCP 四页）的卡片一律**不带描边**，靠底色浮出；
卡片图标**统一使用主按钮那一对 token**（`--color-btn-primary-bg` / `--color-btn-primary-text`），
不再按分类分配语义色。同时补上原型确认过的两处交互：技能页的分类 chip 筛选与统计带、
工具集卡片的工具清单可展开。

改动落在 `ToolTabs.tsx` / `McpCatalogGrid.tsx` / `ToolCardIcon.tsx` 三个文件。

## 根因

三条独立的错误叠加，逐条记下来是因为它们都属于「看起来对、实际不是」这一类：

1. **描边与项目底色关系相反。**
   `tokens.css` 里 `--color-bg-app: #ffffff`（窗口底，最亮）、`--color-bg-card: #fafafa`（卡片比底**暗**一档）。
   组件此前写的是「浅灰底 + 纯白卡 + 投影」—— 正好颠倒。纯白卡片配投影是典型网页卡片语言，
   在这个以纸面为主的项目里显得「网页感重」。

2. **图标色按分类分配，而色相在项目里是受管控的。**
   `ToolCardIcon` 原有 accent / info / success / warning 四档，由 `toneForCategory` 按分类名匹配。
   但 `themes/dark.css` 已把 Tailwind 原生色槽全部重定向到语义色
   （`red-400`→`--color-error`、`emerald-400`→`--color-success` …），
   「给分类发一个颜色」在项目体系里没有立足点；且 info/success/warning 在本项目已被状态语义占用
   （安全扫描结论、启停状态），拿来当分类色会语义串台。

3. **状态信息重复表达。**
   开关旁另起一行「已启用 / 已停用」，与开关本身表达同一件事，且比卡片标题更抢眼。

4. **设置页左导航也有描边**（页头下边线 + 侧栏右边线 + 搜索框），
   与卡片是同一个问题，同批清除。侧栏与内容区靠 `bg-bg-secondary` / `bg-bg-app` 的底色差分界，
   再加一条竖线等于在已经分明的两块之间多画一刀。

5. **内置技能的 `display_name` 是死字段。**
   `types.ts` 声明了 `display_name`，`hermix-core::SkillMeta` / `hermix-skills::SkillFrontmatter`
   都没有这个字段，后端从不解析、也从不返回。结果：17 条内置技能里有 15 条**已经写好了中文名**
   （`display_name: "API 设计模式"` 之类），界面上却全部显示英文 `name`。
   顺带查出 2 条 `description` 是英文（`frontend-architecture` / `tdd-practice`），一并译为中文。
   另 2 条（`code-craftsmanship` / `custom-app-development`）走 seed 注册路径、连中文名都没写，补上。

## Alternatives considered

- **给分类分配低饱和色相（青绿→陶土→中性灰一条轴）**：做过原型，确实比彩虹色干净，
  但仍然是在项目体系外另起色相，且 8 个色相挤在窄区间后肉眼难分。**否决**：
  分类辨识交给分组标题与徽标文字即可，颜色留给有语义的地方。

- **保留 `ToolCardIcon` 的语义分档，只调色**：改动面小得多。**否决**：
  保留就等于保留「按分类发色」这个前提，与 1、2 两条根因冲突。

- **只删描边、不动图标色**：一步到位。**否决**：用户明确指出「图标要统一适配主题色，不是按类别不同」。

## Consequences

- 卡片与背景的分离现在**完全依赖底色差**（浅色 `#fff` 底 vs `#fafafa` 卡；夜墨 `#171717` 底 vs `#1d1f22` 卡）。
  后续若调整 `--color-bg-app` / `--color-bg-card`，必须保证两者仍差一档，否则卡片会糊。
- 阴影保留但压到极轻（卡片已靠底色浮出，重阴影会重新变成「浮起的网页卡片」）。
- **输入框与「添加 MCP」弹窗的描边也已删除**（同批追加）。首轮以「输入框需要边界表示可编辑区域」
  为由保留，属替用户做判断——用户明确要求一律不要描边。改由三级底色层次承担可编辑性：
  浅色 `#fff` 底 → `#fafafa` 卡 → `#f0f0f0` 输入框；夜墨 `#161719` → `#1d1f22` → `#24262a`。
  三层在两套主题下均差一档，不会隐形。
- 统计带（`SummaryBar`）已覆盖全部四个 tab：工具集 / 技能 / 插件 / 连接器。原型只画了三个，
  落地时补齐了连接器页——四个 tab 现在信息结构一致。
- 技能中文名链路已打通：`SkillFrontmatter.display_name` → `SkillMeta.display_name`
  → `SkillInfo.display_name` → 前端 `SkillInfo.display_name`，卡片标题与**搜索**都按中文名走
  （用户按看到的字搜才搜得到）。`Skill.display_name` 同样带 `#[serde(default)]`，
  `save()` 会原样写回——漏掉会让任何一次保存把已有中文名抹掉。
- 今后新增内置技能，`name` 保持英文稳定标识不动，必须同时写 `display_name` 中文名；
  缺失时界面回落到英文 `name`，属可接受降级但不应成为常态。
- `ToolCardIcon` 的 `tone` prop 与 `toneForCategory` / `ToolCardTone` 一并删除。
  若将来确实需要按语义上色（例如安全扫描结论），应新增显式 prop，而不是复活按分类名匹配的启发式。
- 技能页的 chip 计数刻意取**未按分类筛**的结果（`countsByCat` 基于 `byQuery`），
  分组顺序同理基于 `countsByCat`；否则选中任一分类后其余 chip 全部归零、chip 行只剩一个，用户无法切换。
- 无障碍：`focus-visible` 的 outline 环**保留**（chip / 输入框 / 开关）。键盘用户靠它定位焦点，
  这不属于装饰性边框，删掉会造成实质可访问性回退。

## Reintroduction conditions

- 若某个维度确实需要「一眼分类」（例如未来把 20+ 个 MCP server 摊平展示、不再按分类分组），
  可以重新引入颜色维度，但**必须从项目既有语义色派生**，不能自造色相。
- 若项目将来放开色相管控（原生色槽不再重定向到语义色），本决策的约束 2 需要重新评估。
