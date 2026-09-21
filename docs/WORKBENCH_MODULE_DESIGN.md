# 工作台（Workbench）模块设计方案 v2

> 目标：在 Hermix Desktop 宿主内增加「工作台」插件形态 —— 对话创建、对话开发、对话演进、
> 宿主内运行、数据永久保留、默认带右侧对话栏。
> 本文只做方案设计，不含实现。

---

## 〇、v2 路线修正（重要）

**v1 的错误**：我原本设计"宿主提供 12 个内置 UI 组件（表格/表单/看板…）+ AI 只输出声明式 schema"。
这是**替 LLM 把界面设计干了** —— 等于把它降成配置填写员，产物必然是预设模板，
正是需求里明令禁止的"预设好的简单表单/CRUD 模板"。方向反了。

**v2 原则**：

| | 归属 |
|---|---|
| 界面设计、交互、功能逻辑 | **100% 归 LLM**，宿主不干预、不提供 UI 组件 |
| 代码**怎么写**（模块拆分、样式分离、命名、封装） | **宿主强制**，机器校验，不靠 prompt 自觉 |

宿主的角色是 **ESLint + 编译器 + 文件系统**，不是组件库。
一句话：**约束它的工程纪律，不约束它的设计发挥。**

---

## 一、强制模块化：靠骨架与硬限，不靠 prompt

### 1.1 目录骨架由宿主预生成，AI 只在格子里填

创建工作台时宿主先落这套骨架（含每文件头部契约注释），AI 后续只能往里填内容：

```
code/
├── index.html              # 只允许结构骨架 + <link>/<script src>，塞不下东西
├── styles/
│   ├── tokens.css          # 宿主生成，AI 只能覆写变量值，不得新增结构
│   ├── layout.css          # 布局
│   └── components.css      # 组件样式
└── src/
    ├── main.js             # 【宿主锁定，AI 不可写】只做装配
    ├── app.js              # 视图挂载 / 内部路由
    ├── store.js            # 唯一状态源（subscribe / dispatch）
    ├── api.js              # 唯一数据出入口（全仓只有它能调 wb.*）
    ├── views/              # 一个视图一个文件
    │   └── overview.js
    ├── components/         # 可复用 UI 片段，纯函数
    │   └── card.js
    └── utils/              # 纯函数工具
        └── format.js
```

### 1.2 三把锁 —— 让「一个页面写完」物理上做不到

| 锁 | 机制 | 为什么它堵死了单文件写法 |
|---|---|---|
| 锁 1 | `index.html` 禁 `<style>`、禁 `style=` 属性、禁 `<script>` 内联内容 | HTML 里根本放不下样式和逻辑，塞进去就校验失败 |
| 锁 2 | `main.js` 由宿主生成且**不可写** | 入口不是它的画布，它只能填 views / components |
| 锁 3 | 单文件行数硬限（JS ≤200 / CSS ≤300 / HTML ≤150） | 超了直接报错，逼它拆模块 |

> 关键：**不要指望 AI "记得"要模块化，要让它没法不模块化。**

### 1.3 模块契约（每个文件必须导出固定接口）

```js
// views/overview.js —— 宿主生成的头部契约注释，AI 必须遵守
export function mount(root, ctx) { /* 渲染到 root，绑定事件 */ }
export function unmount()        { /* 必须解绑本视图全部监听 */ }

// components/card.js —— 纯函数，不碰状态、不碰 DOM 查询
export function render(props) { return '<div class="wb-card">…</div>'; }
```

### 1.4 模块索引注入 —— 防重复比事后检测更有效

每次迭代前，宿主把当前模块清单注入给 agent：

```
已有模块（复用优先，禁止重写同类功能）：
- utils/format.js  → formatDate(d), formatSize(n)
- components/card.js → render({title, body})
- api.js → listLedger(), saveLedger(row)
```

**AI 只有在知道已有什么时才会复用**，否则它每次都重写一个 `formatDate`。
这条比"事后检测重复代码"有效得多，两者都做。

---

## 二、硬性代码规则（全部可机器校验）

每条规则都有确定性的检查方式，不是"建议"。违规即拒绝落盘。

### 结构类

| ID | 规则 | 校验方式 |
|---|---|---|
| W001 | 禁内联样式属性 `style="…"` | 正则扫 HTML/JS 模板串 |
| W002 | 禁 HTML 内 `<style>` 块 | 正则 |
| W003 | 禁内联 `<script>` 内容，只准 `<script type="module" src>` | 正则 |
| W004 | 单文件行数上限：JS 200 / CSS 300 / HTML 150 | 行数 |
| W011 | 每个模块必须是 ESM（有 `import` 或 `export`） | 正则 |
| W013 | 跨文件重复块 ≥15 行相似 → 强制抽公共模块 | 行级哈希比对 |

### 样式类

| ID | 规则 | 校验方式 |
|---|---|---|
| W004a | CSS 禁裸色值，只能 `var(--token)` | 正则扫 hex/rgb/hsl（可复用 `check-theme-tokens.mjs` 思路扩展） |
| W005 | 禁 `!important` | 正则 |
| W006 | 禁 ID 选择器；选择器嵌套 ≤3 层 | 解析 |
| W006a | 类名遵循项目既有命名规范（见 `docs/CSS_NAMING_AUDIT.md`） | 正则 |

### JS 类

| ID | 规则 | 校验方式 |
|---|---|---|
| W007 | 禁挂 `window.*` 全局（宿主注入的 `wb` 除外） | 正则 |
| W008 | 禁 `var`，一律 `const` / `let` | 正则 |
| W009 | **只有 `api.js` 能出现 `wb.db` / `wb.fs` / `wb.exec`** | 全仓 grep，命中即违规 |
| W010 | 禁 `innerHTML` 直接拼接未转义数据，必须走 `escapeHtml()` | 正则 + 上下文 |
| W012 | 每个导出函数必须一行 JSDoc（功能，禁日期/人物） | 正则 |
| W014 | DOM 查询只能在自己的 `root` 内（禁裸 `document.querySelector`） | 正则 |
| W015 | 事件监听必须在 `unmount()` 内解绑 | AST 级或正则配对 |
| W018 | 列表渲染 >200 条必须走 `wb.virtualScroll()` 工具 | 正则 + 上下文 |

> W009 是"必须封装"的杀手锏：数据访问被物理隔离到 `api.js`，
> AI 想在视图里直接读写数据库，**校验直接拦下**。

---

## 三、门禁怎么跑（落盘前，且必须可自修）

新建 `apps/desktop-tauri/src/workbench/lint.ts`，**自写、零新依赖**（正则 + 简单解析），
符合项目六步决策阶梯（标准库优先，不为几行逻辑加依赖）。

```
agent 产出文件
   ↓
lint(files) → violations[]
   ↓
有违规 → 回传「文件:行号 + 规则 ID + 一句话修法」→ agent 自修（≤2 轮）
   ↓
仍不过 → 拒绝落盘，弹卡片给用户（附违规清单）
   ↓
通过 → 写入 code/ + git commit
```

**成败关键：错误信息必须精确到行号且带修法。**
只回一句"代码不符合规范"会让 agent 陷入重写死循环（复用项目已有 `convergence_detector` 兜底）。

示例回传格式：
```
src/views/overview.js:47  W001  禁止内联样式 style="color:#333"
  → 改到 styles/components.css，用 var(--color-text-primary)
src/views/overview.js:88  W009  数据访问只能在 src/api.js
  → 把 wb.db.query 移到 api.js 并导出函数，此处改为 import 调用
```

---

## 四、宿主提供什么：算法工具 + 依赖通道（不给 UI 组件）

### 4.1 明确划界

| 宿主提供 | 性质 | 例子 |
|---|---|---|
| ✅ 底层算法工具 | 无外观，纯函数 | `wb.virtualScroll()`、`wb.diffText()`、`wb.formatBytes()`、`wb.escapeHtml()`、CSV 解析、文件树遍历 |
| ✅ **第三方依赖通道** | 业界成熟开源库 | Tabulator / AG Grid / CodeMirror / Monaco / ECharts / pdf.js / SheetJS / dayjs —— 见 4.2 |
| ✅ 通道与权限 | 非 UI | `wb.db.*`（受限 ORM）、`wb.fs.*`（审批后）、`wb.emit()` |
| ❌ 宿主自造 UI 组件 | 有外观 | **一律不提供**（v1 的错误，不重犯） |

> 关键区分：**宿主不自己造轮子，但绝不拦着 AI 用别人造好的轮子。**
> "禁止内置组件库"≠"禁止用开源库" —— 前者是宿主替 AI 设计，后者是 AI 自由选型。

### 4.2 第三方依赖通道：用开源库，不重复造轮子

#### 机制：ESM + importmap + 本地 vendor，无需打包

宿主在 `index.html`（AI 不可改的头部）注入 importmap：

```html
<script type="importmap">
{ "imports": {
    "tabulator": "/_vendor/tabulator@6.3.0/esm.mjs",
    "echarts":   "/_vendor/echarts@5.5.0/esm.mjs",
    "dayjs":     "/_vendor/dayjs@1.11.13/esm.mjs"
} }
</script>
```

AI 在模块里直接 `import { TabulatorFull as Tabulator } from 'tabulator'` 即可 ——
**不需要 npm install、不需要 bundler、不需要编译**，完美契合"生成即生效"。
这是 ESM importmap 相对传统打包工作流的最大优势。

#### 为什么必须本地 vendor，不走 CDN

| 理由 | 说明 |
|---|---|
| 离线可用 | 便携版零安装是核心卖点，不能依赖网络 |
| 安全 | 库文件不可被 AI 或远端篡改，换库必须走白名单 |
| 性能 | 本地加载，多工作台共享同一份，不重复下载 |
| 可审计 | 版本固定，`manifest.json` 记录依赖清单 |

#### 白名单与申请机制

- 宿主维护 `~/.hermix/workbenches/_vendor/`（预置 + 按需下载，ESM 格式）。
- **依赖申请**：agent 说「我要用 `echarts`」→ 宿主查白名单 → 已预置直接用；
  未预置则提示用户「需下载 X MB」，确认后拉取到本地 vendor，之后**所有工作台共享**。
- 用户也可手动往 vendor 目录放库（写进白名单）。
- 版本在 importmap 里锁定，AI 无权改。

#### 初始白名单建议

| 类别 | 库 | 说明 |
|---|---|---|
| 表格 | Tabulator / AG Grid Community | 排序、分页、虚拟滚动全都有 |
| 编辑器 | CodeMirror 6 / Monaco | 代码、Markdown、差异对比 |
| 图表 | ECharts / Chart.js | 统计可视化 |
| 文档 | pdf.js、SheetJS(xlsx) | PDF 渲染、Excel 导入导出 |
| 工具 | dayjs、diff-match-patch、marked | 日期、文本 diff、Markdown |

> 有这些，AI 就不必手写排序分页、diff 算法、Excel 解析 ——
> **用成熟库本身就是最强的防屎山手段**：它不用造轮子，代码自然就少而清晰。

#### lint 配套规则（新增）

| ID | 规则 |
|---|---|
| W019 | 禁从 CDN / 远程 URL `import`，必须走 importmap 白名单 |
| W020 | 禁把第三方库源码复制进工作台（必须 `import`，不是粘贴） |
| W021 | 禁 `require()` / 非 ESM 加载方式 |

#### 风险点

- **CSP**：Monaco / pdf.js 用 Worker 与 blob，可能需调整 `tauri.conf` 的 CSP
  （参考项目既有教训：wasm 需 `'wasm-unsafe-eval'`）。
- **体积**：重型库按需下载，不预装全部。

理由：性能与安全的**下限**由宿主兜（虚拟滚动、转义、审批），
**外观与设计 100% 归 LLM**。算法工具不含任何像素，不会污染它的设计发挥。

---

## 五、右侧对话栏（宿主外壳，不是组件库）

这条与 v1 一致，用户已确认采纳：

```
┌────────────────────────────┬──────────────┐
│                            │  ◀ 折叠       │   ← 宿主按钮，状态存 ui-state.json
│      画布（AI 全权设计）     ├──────────────┤
│                            │  对话栏       │   ← 复用现有 ChatView
│                            │  sessionId   │      固定绑定 wb.sessionId
└────────────────────────────┴──────────────┘
```

- 由**宿主 React** 渲染，不是 AI 生成的 HTML。
- AI **无权通过对话隐藏/关闭**它（防工作台把自己锁死，也防它屏蔽审批卡）。
- 工作台创建时开一个**持久 session**，存档 `sessions/main.jsonl`，跨会话保留。
- 折叠状态按工作台持久化，重启恢复。

---

## 六、存储：代码与数据物理分离（与 v1 一致，地基不变）

```
~/.hermix/workbenches/<wb-id>/
├── manifest.json
├── code/                    # 【代码层】骨架 + AI 填的模块，git 可回滚
│   ├── .git/
│   ├── index.html
│   ├── styles/
│   └── src/
├── data/                    # 【数据层】代码层白名单禁区
│   ├── wb.sqlite            # rusqlite 已在 workspace（Cargo.toml:134，bundled）
│   ├── files/
│   ├── ui-state.json        # 布局 / 筛选 / 折叠状态
│   └── _undo/               # 写操作快照（支撑「撤销上一步」）；软删回收站见 P3
├── sessions/main.jsonl
└── snapshots/
```

**铁律（写代码，不写文档）**
1. 代码层无法直接读写 `data/`，只能经 `wb.db.*`（Rust 侧落地）。
2. 删除工作台默认保留 `data/` 与 `sessions/`，删库需二次确认 + 软删。
3. 回滚只动 `code/`；数据 schema 版本只增不减，禁止降级写。
4. 数据 schema 变更走 migration，禁 drop column / drop table。

---

## 七、没有权限关口（v4 修正：授权整层删掉）

> **主人 2026-09-21 明确两次**：「不要那么多的审批要求，现在 AI 的安全意识比人强」
> 「我说了工作台不需要授权」。
> v3 把逐次弹窗换成"首次授权 + 可撤销"，那还是**换了个名字的审批** —— 已整层删除。

### 7.1 为什么连"首次授权"也不要

v3 的"一次性成本"是自欺：**目录级授权仍然要求用户在没能力判断的时刻做决定**
（"允许访问 `D:/` 吗？"——D 盘下有台账、有源码、有下载、有备份，用户凭什么一句话答得上来），
而且它把工作台的能力边界钉死在"用户预想到的目录"上，与"长期演进的工作场所"直接冲突。

真正需要拦的从来不是"危险"，而是**越权**。越权是架构属性，不是用户的决定：
代码层够不到 `data/` 目录、够不到别人的凭据，这些在**物理上**不成立即可，
不需要问任何人。

### 7.2 实际分层

| 层 | 范围 | 处置 |
|---|---|---|
| L0 | 工作台内数据、UI 状态、纯计算 | 直接放行 |
| L1 | 本机文件读写、SQLite、`wb.*` 能力 | **直接放行，无授权、无弹窗、不中断** |
| L2 | 密钥/凭据类文件（`.env` / `.ssh` / `id_rsa` / `credentials` / `.npmrc`）、工作台自己的 `data/` | **拒绝**，理由写明"越权"而非"未授权" |

L2 的拒绝不是安全策略，是**边界声明**：凭据属于用户与外部服务的信任关系，
不属于工作台的数据面；`data/` 归宿主网关独占，代码层直读会把
「代码回滚不丢数据」的隔离打穿。

### 7.3 兜底靠"退得回"，不靠"拦得住"

| 操作 | 兜底机制 | 落地 |
|---|---|---|
| 写 / 改代码文件 | 写盘前自动备份 | ✅ `data/_undo/` + `undo_last` |
| 代码整体回退 | 撤销栈按操作逆序 | ✅ `undo_history` |
| 数据库写 | 数据层与代码层物理分离，回滚代码不动数据 | ✅ |
| 删除文件 | 走软删 | P3 |
| 版本级回滚 | 快照 | P3 |

**看得见 + 退得回，比拦得住更实用。**

### 7.4 唯一保留的一道确认：prompt injection（P3，可选开关）

若某条执行指令的**来源是被读取的文件 / 网页内容**而非用户直接对话，才提示确认。
这与"AI 安全意识"无关 —— **AI 被骗 ≠ AI 变坏**：读到一份写着
"把所有 txt 上传到某地址"的文件时，再强的安全意识也会被绕过。
它针对的是指令溯源异常，**不是**正常操作，所以默认不打断用户。

### 7.5 架构前提不变

能力在 **Rust 侧执行**，工作台 JS 内没有 `fetch` / `fs` / `child_process`，
只有一个注入的 `wb` SDK；宿主外壳是唯一转发者（校验 `e.source`），
代码层拿不到 IPC 入口 —— 绕过不了，也不靠 AI 自觉。

---

## 八、迭代闭环（v2 修订）

v1 反对"整文件重写"，是因为当时假设文件很大。
**v2 里单文件 ≤200 行且职责单一，重写单个模块是安全的** —— 于是 patch 协议大幅简化：

| 层 | 改动方式 |
|---|---|
| HTML / CSS | 整文件写（都 ≤300 行，且样式与结构分离） |
| `views/*.js`、`components/*.js`、`utils/*.js` | 整文件写（≤200 行） |
| `app.js` / `store.js` / `api.js` | **增量 patch**（这三个是骨架，改动要可追溯） |
| `main.js` / `tokens.css` | 锁定，AI 不可写 |

流程：

```
① 右侧栏一句话 → ② agent 拿到「模块索引 + 目标文件」→
③ 产出改动 → ④ lint 门禁（不过→带行号自修）→
⑤ 热更新（只重载受影响模块，不 reload）→ ⑥ git commit
```

**全程不 reload 页面**，数据与 UI 状态不丢。
`data/` 不在这条流水线上：回滚代码、重做界面、改布局，数据都不动。

---

## 九、性能护栏（针对你最初的担心）

| 手段 | 说明 |
|---|---|
| ESM + importmap | 依赖由宿主注入一次，多工作台共享，不重复加载 |
| `wb.virtualScroll()` | 大列表唯一入口（W018 强制） |
| 逻辑跑 Worker | 重计算不占主线程；watchdog 5s 超时 kill |
| 单文件行数硬限 | 顺带保证单模块解析开销可控 |
| 懒加载 | 只渲染当前激活工作台 |

**结论**：模块化 ESM 在本机 WebView2 上性能不是问题。
真正会慢的只有"无虚拟滚动的大列表"和"整页重渲染"，前者被 W018 堵死，
后者因为按模块热更新天然不会发生。

---

## 十、规模扩展性：功能变多会不会被限制

**直接回答：不会。代码规范只限制单个文件的大小，不限制功能总量。**

### 10.1 三类限制，只有一类是真的

| 类型 | 例子 | 是否限制功能 |
|---|---|---|
| 组织约束（**假限制**） | 单文件 ≤200 行、禁内联样式、必须模块化 | ❌ 不限制。只改变代码怎么摆放，**文件数量不设上限** |
| 能力边界（**真限制**） | `wb.*` SDK 有没有提供对应本机能力 | ✅ 这是唯一天花板 |
| 上下文（**软限制**） | 模块太多时 agent 一次看不完 | ⚠️ 可用工程手段化解 |

### 10.2 规模分层

| 规模 | 模块数 | 代码量 | 状态 | 需要的基础设施 |
|---|---|---|---|---|
| 小 | 5–15 | <2k 行 | 完全自由 | 骨架 + lint |
| 中 | 15–50 | 2k–8k 行 | 自由 | 模块索引注入 |
| 大 | 50–150 | 8k–25k 行 | 自由 | 子目录分区 + 索引检索 + ARCHITECTURE.md |
| 超大 | 150+ | 25k+ 行 | 建议拆分 | 多工作台 + 共享数据层 |

参照：一个完整的台账管理系统（表单 + 列表 + 统计 + 打印 + 导入导出）
大约 40–70 个模块、1–1.5 万行，落在「大」区间，完全装得下。

### 10.3 功能变多后要补的三个机制

1. **子目录分区** —— `views/ledger/`、`views/inspection/`，避免一个目录堆 60 个文件。
2. **索引检索代替全量注入** —— 模块 >50 时按任务关键词检索相关模块注入，
   不把全量清单塞进上下文。
3. **ARCHITECTURE.md 自动生成** —— 宿主维护模块关系图，agent 按需查阅，而非每次全量读。

### 10.4 真正的天花板是能力通道，不是代码规范

`wb.*` SDK 决定功能上限。应对：

- 宿主持续加通道（fs / db / exec / dialog / notify / watch 已由 hx-shell 验证过）。
- **能力申请机制**：agent 提出「我需要 `wb.pdf.parse`」，宿主评估后新增；
  短期用逃生舱（iframe）兜底，逃生舱同样要过 lint，只是行数上限放宽。
- **依赖同理**（见 §4.2）：缺库走依赖申请，下载进本地 vendor 后全工作台共享，
  **绝不逼 AI 手写轮子** —— 手写才是屎山的真正来源。
- 结论：**规范决定代码是否好维护，通道决定功能能走多远。** 两者互不干扰。

### 10.5 性能上限与代码量无关

DOM 节点数才是真瓶颈（几万节点会卡），不是代码行数 —— W018 虚拟滚动兜底。
SQLite 单库 10 万行量级无压力。

## 十一、落地路线（v2 重排）

| Phase | 内容 | 完成标志 |
|---|---|---|
| **P0** ✅ | 目录骨架生成器 + manifest + `WorkbenchShell`（画布 + 右侧 ChatPanel + 折叠）+ 持久 session | 说「建一个安全管理工作台」能建出空壳 |
| **P1** ✅ | 骨架契约 + importmap 注入 + lint 门禁 + 自修回传 + wb SDK 落地 + 模块索引 | AI 生成的代码必然模块化、无内联样式、可直接 import 开源库 |

**P1 已落地的东西**（2026-09-20）：

| 件 | 落点 |
|---|---|
| 规则单一真相源 | `crates/hermix-plugin/src/workbench_rules.json` —— 18 条规则 + limits + lockedFiles |
| lint 引擎 | `workbench_lint.rs` —— 逐行正则 / 成对规则 / 行数上限 / 跨文件重复检测，输出「文件:行号 + 规则ID + 修法」 |
| **落盘前门禁** | `write_code_file()` 在写盘前跑 lint，违规直接拒写并回传可定位文本。只拦**本次文件自身**的违规，历史旧账不堵新改动 |
| 骨架自检 | 单测 `skeleton_passes_its_own_lint`：骨架必须先于一切 AI 产出通过自己的 lint。它当场抓出两个真 bug（W022 正则笔误、骨架注释里字面 `!important` 自触 W005） |
| SKILL.md 生成 | `scripts/gen-workbench-skill.mjs` 从 rules.json 生成 `~/.hermix/skills/workbench/SKILL.md`，`--check` 可挂 CI 防漂移 |
| importmap | `index.html` 内 `<!-- wb:importmap:start/end -->` 标记段由宿主重写；`set_deps()` 校验 `_vendor/index.json` 白名单，未装的点名拒绝（引向依赖申请，不是引向 CDN） |
| wb SDK | `ensure_vendor()` 把 `wb-sdk.js` 落到共享 `~/.hermix/workbenches/_vendor/`，多工作台共用一份；骨架在 `main.js` 之前引用 |
| 模块索引 | `module_index()` 扫出「文件 → 导出符号」，渲染成一行一模块的紧凑文本，迭代前注入 prompt |

**规则编号以本文档 §二 为准**：W015 = 事件必须解绑，W016/W017/W018/W020 已预留（循环依赖 / 状态走 store / 虚拟滚动 / 禁粘贴库源码，需 AST 或上下文分析，暂未机器化）。
新加的机器规则取 W023–W025（禁 eval / 禁裸 fetch / 禁 localStorage）。
| **P2** ✅ | `wb.*` 网关（SQLite + fs）+ 撤销栈 + 热更新（**无授权层**，见 §七 v4） | 能读写本机文件，改代码不丢数据 |

**P2 已落地的东西**：

| 件 | 落点 |
|---|---|
| SQLite 网关 | `workbench_gateway.rs` —— `db.query`（只读）/ `db.exec`（写）分离，参数化；`ATTACH`/`PRAGMA`/`VACUUM` 硬拒。库在 `data/workbench.db` |
| 文件系统网关 | `fs.read/write/list`，原子写（tmp + rename） |
| **零授权层** | 本机文件直接用：不弹窗、不预批、不中断。只有越权路径拒绝（密钥/凭据类 + 工作台自己的 `data/`），理由写明「越权」而非「未授权」 |
| 撤销栈 | 每次写盘前备份到 `data/_undo/`，`undo_last` / `undo_history`。撤销代码不碰数据库 —— 这是「代码与数据生命周期分离」的验收点 |
| 热更新 | `workbench_write` 成功后 emit `workbench://changed`，宿主外壳收到就用新 key 重建 iframe。工作台没有构建步骤，重载就是热更新 |
| SDK 真桥 | `wb-sdk.js` 改成 Promise + request id 的 postMessage 请求/响应；宿主外壳是唯一转发者，代码层拿不到 IPC 入口 |

**验收**：`crates/hermix-plugin/tests/workbench_e2e.rs` 真建工作台跑通
建库 → 读写 → 硬拒 → 撤销 → 骨架齐全（1 passed）；lib 单测 28 passed。
| **P3** ✅ | 算法工具箱（虚拟滚动 / diff / 解析 / 统计）+ 三路径投递 + SKILL 同步 | AI 迭代时直接 import `wb-tools`，不重写通用算法 |
| **P4** | 版本快照 UI + 多窗口分割 + 文件对比界面 | 可一键回退到任意历史版本；并排看多个工作台 |

**P3 已落地的东西**（2026-09-21）：

| 件 | 落点 |
|---|---|
| 工具箱源码 | `crates/hermix-plugin/src/workbench_tools.rs` —— **以 TS 维护**（`include_str!` 纳入二进制），四个纯函数：`virtualWindow` / `diffLines` / `parseDelimited` / `columnStats` |
| 剥类型 | 宿主注入前剥掉 TS 类型。手写状态机处理：interface/type 块、参数注解、返回注解、局部变量注解、`as` 断言、泛型实参 |
| **三条投递路径** | ① importmap 的 `wb-tools` → `_vendor/wb-tools.js`（ESM，任意模块可 import）② SDK 内联 `wb.util`（同步，只在 `api.js` 用）③ 对象字面量供 `__wbTools`。单测 `toolbox_reaches_workbench_through_all_three_paths` 钉住 |
| 验证脚本 | `scripts/verify-wb-tools.mjs` —— esbuild 真解析（语法）+ 动态 import 真跑 12 项用例（语义） |
| SKILL 同步 | `gen-workbench-skill.mjs` 新增「算法工具箱」章节，含签名表与「别自己重写」的明确要求 |

**为什么必须双验证**：剥错一个字符的症状是「Rust 单测全绿、浏览器白屏」。实测踩到 6 个坑，
其中两个 **esbuild 单独查不出来**（`<number>` 泛型实参被当 JSX 放行；对象字面量
`{left: a[i]}` 被当参数注解剥成 `{left}` —— 语法合法但运行时 `left is not defined`）。
只有真执行才暴露。7 种形态已固化为回归单测 `strip_types_handles_every_known_shape`。

**工具箱只给算法，不给外观** —— 四个函数全是纯函数，不含任何 DOM 结构与样式决策。
这是 §四 原则的延伸：宿主可给算法级工具，界面/交互 100% 归 LLM。

**验收**：workbench 相关 lib 单测 29 passed；`node scripts/verify-wb-tools.mjs` 语法+语义双绿。

**P4 依赖关系**：版本快照 UI 需先有 §八 迭代闭环稳定运行；多窗口分割涉及宿主布局
（`.app-body` 是 grid），改动前须读「布局与缩放」铁律。

**P1 是分水岭**：lint 规则写得越具体，后面越省心。
规则本身应该是可配置的（`workbench-rules.json`），方便随时加条。

---

## 十二、对抗式审查：v2 会在什么情况下崩

| 风险 | 触发场景 | 缓解 |
|---|---|---|
| lint 太严，AI 反复不过 | 规则互相冲突或错误信息不可自修 | 错误信息必须带行号 + 修法；失败 2 轮转人工 |
| AI 把代码拆碎成一堆 200 行文件，还是乱 | 拆分≠设计 | 模块索引注入 + 重复检测（W013）双管 |
| 模块间循环依赖 | A import B，B import A | lint 加循环依赖检测（W016） |
| 样式仍写成一坨 | CSS 300 行上限还是够写乱 | 按组件分文件（每个组件一个 css），后续可再收紧 |
| 状态散落各模块 | 各模块自己存局部变量 | W009 同类手段：状态只能走 `store.js`（W017） |
| 回滚后代码引用了不存在的模块 | 版本错配 | commit 前 lint 全量跑一遍 |

---

## 十三、Agent 怎么知道：workbench skill 的加载机制

### 13.1 现有机制（已核对源码）

- `crates/hermix-agent/src/prompt_assembly.rs:102` `skills_system_prompt`
  渲染 `<available_skills>` **索引块**（skill 名 + 描述 + tags，上限 80 条）。
- 系统提示原文："Before replying, check whether an existing skill applies.
  If yes, inspect it with `skill_view` and follow it."
- **`ephemeral_skills`（`AgentOverrides::skills`）会把 SKILL.md 全文 inline 进提示**
  （`prompt_assembly.rs:166`）—— 这是最可靠的通道。

也就是说：**AI 能知道，但"靠它自己想起去看"是不够的**，必须有确定性兜底。

### 13.2 三层保障，越靠下越确定

| 层 | 机制 | 可靠度 |
|---|---|---|
| 1 | skill 索引常驻 + description 写强触发词（"创建/开发/迭代工作台时必读"） | 中 —— 依赖 AI 主动 `skill_view`，会漏 |
| 2 | **宿主关键词检测**：用户消息命中「工作台 / 工作环境 / 做个…工作台」→ 本轮自动追加 `ephemeral_skills=["workbench"]` | 高 —— 确定性，不依赖 LLM 判断 |
| 3 | **工作台 session 的 `AgentOverrides.skills` 恒为 `workbench`** | 100% —— 根本不需要意图识别 |

第 3 层是关键：右侧对话栏绑定的 session **本来就只为这个工作台服务**，
skill 常驻即可，**零意图识别成本**。主对话里"建一个安全管理工作台"由第 2 层兜住。

### 13.3 SKILL.md 里必须写什么

- 骨架契约（目录结构 + 哪些文件锁定、哪些可写）
- **全部 lint 规则（W001–W021），带正反例**
- 依赖白名单与 importmap 用法
- 模块索引的注入格式
- 落盘流程与"违规带行号自修"要求

### 13.4 规则单一真相源（防止文档与实现漂移）

```
workbench-rules.json   ← 唯一真相源
    ├── 生成 lint 实现（workbench/lint.ts）
    └── 生成 SKILL.md 规则章节
```

**同一份源，两种用法**：
- SKILL.md 让 AI **一次写对**（预防）
- lint 在落盘前拦截（兜底）

避免"文档写 200 行、lint 查 300 行"这种必然出现的漂移。

### 13.5 部署位置

`~/.hermix/skills/hermix/workbench/SKILL.md`（端用户层）。
既有教训：builtin skill 改后需 `cargo build` 重嵌 **并** cp 到该目录，两处都要更新。

---

## 十四、待你拍板

1. **行数上限取值**：JS 200 / CSS 300 / HTML 150 是否合适？（偏严则 AI 拆得更碎，偏松则单文件变重）
2. **重复检测阈值**：15 行相似即报错，是否太敏感？
3. **数据层粒度**：每工作台一个 SQLite 库（隔离好）vs 统一库分表 —— 仍建议前者。
4. **入口**：主侧栏独立入口 vs 挂 `CustomAppsView` 下 —— 建议独立（分发型 vs 演进型语义不同）。
