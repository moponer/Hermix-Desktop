# Decision Note: 助手阿黎（Twin）实施执行方案

Status: proposed — 方案已定稿，阶段一（界面落地）已执行，阶段二起待后端接通。

## Decision

把已确认的设计落成可运行的产品代码，按「界面先落、数据源分步接、执行能力最后开」的顺序推进。本方案是执行计划，不是新一轮设计讨论；设计已由原型确认，本方案只解决「谁在什么时候做什么、怎么验收」。

配套设计依据：
- 实体与能力边界：[分身助理](../feature/2026-10-21-digital-twin-assistant.md)
- 监督机制：[守望者](../architecture/2026-10-21-watcher-resident-agent.md)
- 视觉原型：`docs/prototypes/twin/index.html` + `twin.css`（双主题，impeccable detect exit 0）

### 1. 目标与范围

**目标**：hermix 桌面端出现一个独立入口「助手阿黎」，能展示她替你看到的东西、正在推动的事、需要你拍板的事；底部单行输入口；浅色/深色双主题下与主界面同源不突兀，但保持独立的仪表盘设计语言。

**范围内（阶段一，本次执行）**

| 编号 | 内容 | 状态 |
|---|---|---|
| S1 | 独立样式层 `styles/components-twin.css`，`.twin-*` 命名空间，只用 `--color-*` 令牌 | 本次交付 |
| S2 | `components/TwinView.tsx` 三栏信息板 + 底部输入口 + 权限徽标 | 本次交付 |
| S3 | 路由与侧栏接入：`NavPage` 加 `twin`、`routes.tsx` 加分支、侧栏加「助手阿黎」 | 本次交付 |
| S4 | 数据适配层：接现有可得的真实数据（自动化任务、活动运行数、最近会话） | 本次交付 |
| S5 | 缺口模块的显式空态（新鲜情报 / 待你定夺 / 她最近学到），标注「数据源待接通」 | 本次交付 |
| S6 | 设计门禁与类型检查全绿 | 本次交付 |

**范围外（不在本次执行，需后端先行）**

| 编号 | 内容 | 阻塞原因 |
|---|---|---|
| N1 | 阿黎的独立会话与对话通道 | 需要 `twin` 实体与会话归口，属后端新能力 |
| N2 | 新鲜情报的真实数据 | 需要订阅源水位线与增量去重（`sources.json`） |
| N3 | 待你定夺的真实问句 | 需要 `MessageRole::Watcher` 与决策队列 |
| N4 | 她最近学到的真实条目 | 需要 `profile/` + `ledger` 的每日提炼循环 |
| N5 | 权限切换真实生效 | 需要代批身份与审计（`PermissionLevel` 加身份维度、`ApprovalRecord` 加 `actor`） |
| N6 | 底部输入口真实发送 | 依赖 N1 |

### 2. 阶段拆分与任务清单（含优先级）

**阶段一 · 界面落地（P0，本次执行，4 小时）**

| ID | 任务 | 产出 | 优先级 |
|---|---|---|---|
| T1.1 | 建 `components-twin.css`，双主题令牌别名，迁入三栏布局、卡片、进度、输入口样式 | 1 个 CSS 文件 | P0 |
| T1.2 | `globals.css` 注册 `@import "./components-twin.css"` | 1 行 | P0 |
| T1.3 | 建 `TwinView.tsx`：`PageHeader` + 状态读数 + 三栏 + 输入口 | 1 个组件 | P0 |
| T1.4 | 数据适配：`getAutomationTasks` / `getActiveAutomationRuns` / 会话列表；`loading/error/空态` 三态 | 组件内 `refresh()` | P0 |
| T1.5 | 视图注册：`types.ts` NavPage、`routes.tsx` lazy + case、`App.tsx` 传 `openChat` | 3 处改动 | P0 |
| T1.6 | 侧栏菜单项「助手阿黎」+ lucide 图标 | 1 处改动 | P0 |
| T1.7 | 门禁验证：`tsc --noEmit`、`check:design`、`check:design-primitives`、`check:tokens`、`impeccable detect` | 5 份结果 | P0 |

**阶段二 · 记忆与能力自省（P1，1 天，需后端）**

| ID | 任务 | 依赖 |
|---|---|---|
| T2.1 | `daily_memory` 工具补读取动作（现在只能写今天、不能读历史） | 无 |
| T2.2 | `tool_search` 加 `action=list` 全量枚举（现在只搜 deferred 桶） | 无 |
| T2.3 | `builtin-skills/hermix/twin/SKILL.md` 能力地图（12 类能力 + 何时用哪个） | T2.2 |
| T2.4 | 「她最近学到」接 `profile/` 与 `ledger` 只读展示 | T2.1 |

**阶段三 · 主动研究与告知（P2，2 天）**

| ID | 任务 | 依赖 |
|---|---|---|
| T3.1 | `sources.json` 订阅源与水位线（RSS `last_guid` / 页面 `content_hash` / 搜索 `top_n_fingerprint`） | T2.4 |
| T3.2 | 增量判定纯函数 + 单测：「新」且「与 profile 相关」才通知 | T3.1 |
| T3.3 | 新鲜情报栏接真实数据，含「为什么给你看」的依据行 | T3.2 |

**阶段四 · 执行能力（P2，2 天，风险最高）**

| ID | 任务 | 依赖 |
|---|---|---|
| T4.1 | `workflow` 补触发入口（现在只有 save/list/remove） | 无 |
| T4.2 | `cronjob` 补投递目标参数（现在 anchor 只能靠注入） | 无 |
| T4.3 | `Watch` 存进引擎 A 的 JSON 任务表（`serde(default)` 扩展，不动 SQLite） | 无 |
| T4.4 | 代批身份与审计：`PermissionLevel` 身份维度、`ApprovalOutcome` 带 actor、`ApprovalRecord` 加 `actor` 列 | 独立 PR |

**阶段五 · 对话通道（P1，1 天）**

| ID | 任务 | 依赖 |
|---|---|---|
| T5.1 | 阿黎的专属会话归口（不进普通会话列表，可回看） | T4.3 |
| T5.2 | 输入口真实发送 + 流式渲染 | T5.1 |

### 3. 负责人与协作方

单人项目，按角色分工而非人头：

| 角色 | 承担者 | 职责 |
|---|---|---|
| 执行（实现） | AI 助手 | 编码、验证、写交付说明 |
| 产品决策与验收 | 用户 | 拍板范围、验收界面、决定阶段二的取舍 |
| 代码评审 | 用户（兼） | 只审阶段四（代批与审计，唯一有架构风险的改动） |
| 协作方：automation 引擎 | `hermix-server/src/dashboard/automation.rs` | 提供任务与运行态；阶段四的 Watch 落点 |
| 协作方：工具体系 | `hermix-tools` | 提供 `workflow` / `cronjob` / `tool_search` 的改动面 |
| 协作方：设计门禁 | `scripts/check-design-*.mjs` | 阶段一每步都要过 |

### 4. 时间节点与里程碑

| 里程碑 | 内容 | 时点 | 判定 |
|---|---|---|---|
| M1 | 样式层与视图骨架完成，能渲染三栏 | 夜间 1h | 组件能在应用里打开 |
| M2 | 数据适配接通（自动化 + 会话） | 夜间 2h | 有真实任务时列表正确显示 |
| M3 | 路由与侧栏接入，门禁全绿 | 夜间 3.5h | 五个检查命令全部 exit 0 |
| M4 | 交付说明与验收清单 | 夜间 4h | 用户早上可直接打开验证 |
| M5 | 阶段二完成 | 用户拍板后 1 天 | 阿黎能读到自己的记忆 |
| M6 | 阶段三/四/五 | 按优先级排期 | 见各阶段验收 |

### 5. 所需资源与依赖条件

**资源**：本机 Windows + MSVC 工具链（已配好）、`node_modules`（已装）、Tauri 前端依赖、无新增 npm 包、无新增 crate。

**硬依赖（必须先成立）**

1. 前端只能读 token，不能造色 —— 已由 `check-theme-tokens` 强制。
2. 视图必须靠 `flex-1`/`h-full` 撑满 `App.tsx:4083` 的容器，否则页面塌高。
3. 侧栏菜单位于 `NAV_ITEMS` 数组，图标来自 lucide。
4. 阶段四的 Watch 必须落在引擎 A（`automation_tasks.json`），**不能**落到引擎 B 的三表（无生产写入方）。

**软依赖（可降级）**

- 阿黎的会话不存在 → 输入口降级为「切到对话页」。
- 情报/定夺/成长三栏无数据源 → 显式空态，不造假数据。

### 6. 风险点与应对预案

| ID | 风险 | 影响 | 应对 |
|---|---|---|---|
| R1 | 视图空态过多，看起来像半成品 | 验收观感差 | 每个空态写清「将来显示什么」+ 一句能力说明；真实数据栏（正在执行）优先填充 |
| R2 | 独立 CSS 与 Tailwind 冲突 | 样式错乱 | 全部类名 `.twin-*`，不使用 Tailwind 原子类；`components-twin.css` 只依赖 `--color-*` 令牌 |
| R3 | 深色下卡片与画布只有一档明度差 | 层次弱 | 采用 app 的 `bg-card` 而非自造；若验收嫌弱，改 `bg-elevated`（`#262626`）一档，只改一行 |
| R4 | 门禁 G10 中性描边额度（全仓 27） | 新页面推高总量 | 本页全部描边走 CSS 而非 Tailwind 类，不消耗额度 |
| R5 | 新页面依赖 `RouteContext` 扩字段 | 编译面扩大 | 只加 `openChat`（已存在），不新增字段 |
| R6 | 阶段四代批触及 `hermix-core` 三处类型 | 波及面大 | 单独立项，不与功能混在一个 PR |
| R7 | 空态被误认为「后端已坏」 | 用户困惑 | 文案区分「尚未接入」与「加载失败」，error 态单独渲染 |


### 7. 验收标准与交付物

**阶段一验收（可逐条勾）**

| 编号 | 验收项 | 判定方法 |
|---|---|---|
| A1 | 侧栏出现「助手阿黎」，点击进入独立页面 | 目视 + 点击 |
| A2 | 页面为三栏信息板，底部单行输入口贴底 | 目视 |
| A3 | 浅色/深色切换后，本页无突兀色块，无自造颜色 | 切主题目视 |
| A4 | 「正在执行」显示真实自动化任务（无任务时为空态，不显示假数据） | 建一条任务后回看 |
| A5 | 三处缺口模块显示「未接入」空态，文案不谎报 | 目视 |
| A6 | `cargo check` 不受影响、`tsc --noEmit` 通过 | 命令 exit 0 |
| A7 | `check:design` / `check:design-primitives` / `check:tokens` / `impeccable detect` 全部 exit 0 | 命令 exit 0 |

**交付物**

1. `apps/desktop-tauri/src/styles/components-twin.css`
2. `apps/desktop-tauri/src/components/TwinView.tsx`
3. 改动：`types.ts`、`app/routes.tsx`、`components/Sidebar.tsx`、`styles/globals.css`
4. 本方案文档
5. 验收报告（含五条命令的实际输出）

### 8. 进度跟踪与汇报机制

| 机制 | 频率 | 责任人 | 载体 |
|---|---|---|---|
| 任务清单状态 | 每个任务开始/结束时更新 | 执行 | 会话内 todo 列表 |
| 门禁结果 | 每阶段末 | 执行 | 验收报告（贴命令与退出码） |
| 缺口登记 | 每发现一个新缺口 | 执行 | 本文档「范围外」表 |
| 阶段验收 | 每阶段一次 | 用户 | 打开应用实操 |
| 里程碑复盘 | 每里程碑一次 | 用户 + 执行 | 本文档追加一行记录 |

**汇报格式（固定五段）**：做了什么 / 验证结果（命令 + 退出码）/ 偏离原型的部分及原因 / 未完成项与阻塞 / 下一步建议。

## 执行记录（2026-10-21 夜，阶段一）

**做了什么**：S1–S6 全部交付。新增 2 个文件、改动 4 个文件。

| 文件 | 类型 | 内容 |
|---|---|---|
| `apps/desktop-tauri/src/styles/components-twin.css` | 新增 | `.twin-*` 命名空间，双主题令牌别名，三栏板/卡片/任务/输入口/待接通块 |
| `apps/desktop-tauri/src/components/TwinView.tsx` | 新增 | `PageHeader` + 读数 + 权限徽标 + 三栏 + 底部输入口 + 数据适配 |
| `apps/desktop-tauri/src/styles/globals.css` | 改 | 注册 `@import "./components-twin.css"` |
| `apps/desktop-tauri/src/types.ts` | 改 | `NavPage` 加 `"twin"` + 理由注释 |
| `apps/desktop-tauri/src/app/routes.tsx` | 改 | `lazy` 声明 + `case "twin"` |
| `apps/desktop-tauri/src/components/Sidebar.tsx` | 改 | 导入 `Sparkles`，`NAV_ITEMS` 首位加「助手阿黎」 |

**验证结果（实际命令与退出码）**

| 检查 | 命令 | 结果 |
|---|---|---|
| 类型 | `./node_modules/.bin/tsc --noEmit` | exit 0，无输出 |
| 设计门禁 | `node ../../scripts/check-design-guard.mjs` | `DesignGuard: clean`，exit 0 |
| 设计原语 | `node ../../scripts/check-design-primitives.mjs --strict` | `clean (G3,G6a,G6b,G7,G8,G9,G10,P0)`，exit 0 |
| 令牌 | `node ../../scripts/check-theme-tokens.mjs` | contract dark/ink-green 107/107、无孤儿、无裸色、对比度全过，exit 0 |
| 反模式 | `npx -y impeccable@4.1.0 detect src/components/TwinView.tsx src/styles/components-twin.css` | exit 0 |
| 构建 | `vite build` | 产物含 `TwinView-*.js`；`globals-*.css` 内 `twin-view/board/dock/pending/readout` 全部命中 |
| CSS 语法 | `postcss.parse` 解析两个样式表 | OK |

**偏离原型的部分及原因**

1. **顶栏改用 `PageHeader`**：原型是自绘一层顶栏，落地改用项目统一页头（h-14），读数与权限按钮进 `actions`。理由是「页头收敛」是既有纪律，且新页面不该为视觉差异破坏它；视觉差异改由下方三栏板承担。
2. **进度条只在有限次任务（`max_runs`）出现**：原型里写死的 38%/62% 是示意。真实 API 不返回运行百分比，所以进度只在 `run_count / max_runs` 有真实语义时显示，否则显示「第 N 次」。**不编造进度**。
3. **新鲜情报栏补了「最近活跃」**：该栏主数据源未接通，空态之外补一组真实会话（取宿主 `sessions` 按 `updated_at` 降序前 5），并标明来源是「会话」而不是情报。
4. **输入口降级为切页**：会话通道未接通，回车只做 `onOpenChat()` 并清空输入；hint 行明写「阿黎的对话通道待接通」。
5. **权限徽标只切视觉态**：`data-level` 在 `default`/`full` 间切换，但不影响真实工具审批——后端没有代批身份，不能假装有。
6. **未用的 `EmptyState` 改为自绘 `.twin-pending`**：三栏只有 250–400px 宽，`EmptyState` 的居中大留白在窄栏里不成立；改用左对齐的说明块，且能列「将来会显示什么」。

**未完成项与阻塞**

- N1（阿黎的独立会话与对话通道）、N2（情报源）、N3（决策队列）、N4（成长档案）、N5（权限真实生效）全部按计划留到阶段二之后，均有明确阻塞项。
- `cargo check` 未跑：本次未触碰 Rust 代码。

**发现的一个验证陷阱（值得记住）**

`search_content`（ripgrep）会跳过 `.gitignore` 里的目录，`release/` 在其中。用它去核对构建产物会得到「0 命中」的假阴性，让我误判过一次「样式没打进包」。核对产物一律用 `Select-String` 或 `search_file`。

## 执行记录（2026-10-22，阶段二）

**做了什么**

| 交付 | 文件 | 说明 |
|---|---|---|
| 阿黎的角色形象「墨团」 | `apps/desktop-tauri/src/components/twin/AliCharacter.tsx` | 纯矢量 SVG，5 种状态姿态（idle / thinking / running / done / alert），颜色全部走 CSS 变量。不是动物——应用里已有柴犬 mascot，阿黎是「分身」，不做第二只宠物 |
| 角色样式 | `styles/components-twin.css` | `.ali__*` 段：本体淡染 + 轨道环刻度 + 双眼 + 三种状态标记；仅两个动效（4.5s 浮动、6s 眨眼），都在 `prefers-reduced-motion` 下被全局压掉 |
| 在场位 | `components/TwinView.tsx` | 页头下方一条 64px 的 strip：48px 角色 + 「她在干什么」+ 「上次核对」。角色姿态与文案同源（`aliState` 由 error/loading/activeRuns 推出），不会出现「表情在笑但文案报错」的错位 |
| `daily_memory` 补读取 | `crates/hermix-tools/src/tools/daily_memory.rs` | 新增 `action=read` + `days`（默认 7，上限 30）。此前的缺口：只能写今天、读不回来，所以「跨天回忆进展」只能靠 side-channel。输出按 char 边界裁到 16k 并留「用 days 缩小范围」的提示 |
| `tool_search` 补全量枚举 | `crates/hermix-tools/src/tools/tool_search.rs` | 新增 `action=list`：枚举**全部**工具（含常驻桶），每条带 `bucket`。原实现只扫 deferred 桶，常驻与 bridge 工具永远搜不到——这就是「助理不知道自己会什么」的直接原因 |
| 能力地图 skill | `src-tauri/builtin-skills/hermix/twin/SKILL.md` + `lib.rs` 注册 | 12 类能力路由表（需求 → 该用什么 → 禁止怎么用）+ 三条硬规则 + 外部 CLI 任务书模板。随二进制分发，`BUILTIN_SKILLS` 加一行 |
| 第三栏接真实数据 | `components/TwinView.tsx` | 「她最近学到」换成「最近工作日志」，读 `GET /api/memory/daily`（`daily_memory` 同一份数据）；成长档案（profile/ledger）仍显式标「待接通」 |

**验证结果**

| 检查 | 结果 |
|---|---|
| `cargo check -p hermix-tools --all-targets` | exit 0（57.96s） |
| `cargo test -p hermix-tools --lib` | **783 passed / 0 failed**（新增 4 个用例：list 枚举含常驻工具、空 query 仍被拒、clip 短文本不动、clip 多字节按 char 边界） |
| `tsc --noEmit` | exit 0 |
| `check-design-guard` / `check-design-primitives --strict` / `check-theme-tokens` | 全部 clean，exit 0 |
| `impeccable detect`（角色 + 视图 + CSS） | exit 0 |
| `cargo check -p hermix-desktop` | **被环境挡住**：`hermix`（PID 22600，23:39 启动）正在运行，`src-tauri/binaries/*.exe` 被锁，tauri-build 在 `lib.rs:164` 拿不到写权限。这不是代码问题——本次对 `lib.rs` 的改动只有「一个 `include_str!` 常量 + `BUILTIN_SKILLS` 一行」，且该 SKILL.md 路径已核实存在。关掉应用即可复验 |

**一处修正**

角色尺寸初版写 `size={44}`，被设计原语门禁 G7 判为偏离档位（合法集 10/12/14/16/22/24/28/32/36/48/72）。改 48。门禁比自觉可靠，这就是它存在的意义。

**下一批（按依赖顺序，未做）**

1. `workflow` 补触发入口 —— 现在只有 save/list/remove，「落地工作流」缺最后一环。
2. `cronjob` 补投递目标参数 —— 现在 anchor 只能靠注册表注入的 `_session_id`。
3. `Watch` 落进引擎 A 的 JSON 任务表（`serde(default)` 扩展 + 独立 `event_key` 去重）。
4. 代批身份与审计（`PermissionLevel` 加身份维度 / `ApprovalOutcome` 带 actor / `ApprovalRecord` 加 `actor`）—— 唯一有架构风险的一批，建议单独评审。

## Alternatives considered

1. **复用 Tailwind 原子类写页面，不建 CSS 文件** — 否决。用户明确要求「独立样式，方便以后单独调整」；且三栏仪表盘的密度控制需要精确的间距与发丝线，原子类会把它写成一屏 className 噪音。
2. **等后端 Twin 实体完成再落界面** — 否决。界面是需求的载体，先落界面能让「她能做什么」变成可讨论的对象；空态已显式标注，不会误导。
3. **空态里填示例数据让页面好看** — 否决。这是谎报。用户早上看到的数据必须是真的，否则无法判断界面是否可用。
4. **把页面做成对话页的另一个入口** — 否决。那就退化成「又一个聊天」，与已确认的定位冲突。
5. **现在就做代批（阶段四 T4.4）** — 否决。它触及 `hermix-core` 的 `PermissionLevel` / `ApprovalOutcome` / `ApprovalRecord`，必须独立评审，不能和界面一起塞进一个夜晚。

## Consequences

**放弃的东西**

- 本次不交付「能对话的阿黎」。输入口在会话通道接通前只做切页降级。
- 本次不交付「她真的在学习」。三处学习相关模块只有空态与说明。
- 本次不给页面加任何动画与装饰性脉冲（设计门禁判定为假活力）。视觉张力靠排布与等宽数字，不靠动效。

**欠下的债**

1. 阶段二的 `daily_memory` 读取与 `tool_search` 全量枚举——它们是所有主动行为的输入源，不补则空态永远是空态。
2. 阶段四的代批身份与审计——不补则「完全权限」只能是用户手动的 `Full`，不能是她代批。
3. 深色下卡片明度只差一档，若验收嫌弱需回调 `bg-elevated`。
4. 侧栏「助手阿黎」四字在 208px 下刚好够，若侧栏支持收窄需做短名降级。

**Reintroduction conditions**（若将来有人提议删掉独立 CSS 回退到 Tailwind）

- 三栏布局被合并为单栏，不再需要精确的发丝网格；
- 或项目引入 CSS Modules 并统一迁移，此时本文件随之迁移而非删除。
