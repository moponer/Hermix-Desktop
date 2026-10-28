# Decision Note: Three.js 渲染层可行性评估

Status: proposed — 评估 Three.js 引入前端渲染层的可行性。三条路径（3D 背景层 / 混合渲染 / 全量 3D 界面）的实测结论、可行边界与三处硬约束。**本文只做评估与建议，尚未拍板落地任何方案。**

## Decision

**Three.js 引入渲染层技术上可行，但可行形态只有一种：混合渲染（DOM 扛语义与文字，Three.js 扛视觉与空间）。**

「全量 3D 界面」（把对话流、设置、列表、表单全部渲染成三维物体）**不可行**，且不因「允许重构前端」而变得可行——障碍不在工程量，在**输入方式、无障碍与文本渲染**这三件事上，DOM 有不可替代性：

- **输入**：中文/英文输入法候选框、选区复制、粘贴富文本、屏幕阅读器，这些全部依赖 DOM 文本节点。WebGL 里没有 selection concept，IME 候选框无法锚定到三维物体上的某个字。
- **无障碍**：`aria-label` / 语义标签 / 焦点环是 DOM 原生能力。把 167 个组件改成三维物体，等于把整个产品的可访问性归零。
- **文本渲染**：中文三维文本要么走 SDF（troika-three-text，934KB），要么走 Canvas2D 贴图，两者都不如 DOM 的原生文本渲染清晰、可选中、可搜索。

因此**重构的范围应限定在「渲染层」**，而不是「整个前端」：

| 重写 | 不重写 |
|---|---|
| 主题层：116 个色彩令牌 → 材质描述符映射 | MarkdownBody（981 行）、代码高亮、表格 |
| 3D 场景外壳（renderer / camera / 调度） | `@hermix/preview` 共享预览层（37 文件 / 2842 行） |
| 渲染调度（按页面挂载/卸载场景） | 后端 23 个 crate ＋ sidecar 协议（与渲染层无关） |
| 令牌 ↔ 材质桥接（`--color-*` → material uniform） | 交互语义（表单、列表、弹窗的 DOM 实现） |

**主题契约的处置**：现有 116 令牌契约（`tokens.css` @theme 派生，`scripts/check-theme-tokens.mjs` 强制每套主题全覆盖）**保持不变**。3D 层不去新增色彩令牌，只**消费**既有令牌——材质颜色从 `var(--color-*)` 现读现用。这样主题切换时 3D 场景天然跟随，不需要第二套主题体系，也不需要改任何主题 CSS。

## Alternatives considered

| 方案 | 为何否决／采纳 |
|---|---|
| **全量 3D 界面**（所有组件三维化） | 否决。输入法、选区、屏幕阅读器依赖 DOM 文本节点；167 个组件三维化后产品可用性与可访问性归零 |
| **3D 背景层**（WebGL canvas 垫底，DOM 界面不动） | 采纳为**首选落地形态**。令牌零改动、改动面可控在新增组件、不破坏现有主题门禁；代价是 3D 只是装饰层，不承载信息 |
| **空间化数据视图**（记忆图谱 / 工具关系 / 任务流 3D 化） | 采纳为**第二优先**。仓库已有 ECharts 图谱（`AnalyticsView` / `MemoryGraph`）与 cytoscape+elk 布局基础，3D 化有真实信息增益，不是纯装饰 |
| **引入 tree.js（`@dgreenheck/ez-tree`）** | 否决。见 [§为什么排除 tree.js](#为什么排除-treejs) |
| **引入 react-three-fiber（R3F）** | 采纳。v9.8.2 的 peer 要求 `react >=19 <19.4`，本仓 React 19.1 满足；drei 10.7.9 依赖 `r3f ^9.0.0` 匹配 |
| **重写后端 / sidecar 协议** | 否决。与渲染层完全正交，属另一个话题 |

### 为什么排除 tree.js

用户初始理解为 tree.js。实测结论是**工具与需求不匹配**：

- `@dgreenheck/ez-tree` v1.1.0（MIT，2026-01 发布，2026-07-16 最新提交）是**程序化树木生成器**——生成树干/树枝/树叶几何，与界面主题无关。
- 其前身 `@dgreenheck/tree-js` 已于 2024-08 停更，不要用旧包。
- 库本体 ESM 4.02MB，其中 **base64 内联纹理占 3.88MB**（16 张 jpeg + 4 张 png），**纯代码仅 46KB**。
- 用它做 3D 主题，最终只能贡献「一棵树的网格」——光照、材质、后期处理全需自研，而树本身在界面主题里没有语义位置。

若未来确有「生成真实树木」的需求（例如工作台视觉装饰），可**按需单独引入**该包，与本次渲染层决策解耦。

## Consequences

### 收益

- **主题机制零改动**：116 令牌契约不动，`check-theme-tokens.mjs` 门禁不动，既有主题 CSS 一行不改。
- **增量落地**：3D 能力以新增组件形式进入，不触碰 3619 处既有 `var()` 引用，改动面可控、可回滚。
- **真实信息增益**：空间化数据视图（记忆图谱 / 工具关系）比 2D 图表更适合展示多维关联，这是 3D 唯一有说服力的用武之地。
- **依赖成本低**：three 本体 128KB gzip（`three.module.js` 663KB → gzip 128KB），tree-shaking 后可再压；相对当前前端产物 19MB / 248 文件可忽略。

### 成本与风险

| 项 | 实测数据 | 说明 |
|---|---|---|
| **中文字体（最高风险）** | troika-three-text 934KB；默认字体 Roboto 走 **Google Fonts**；中文回退查 **jsdelivr 的 unicode-font-resolver** | 本仓是便携零安装 ＋ 本地 sidecar ＋ 本地 SQLite 的离线优先架构，运行时外网请求不可接受。**必须本地内嵌中文字体子集并绕过 resolver** |
| **zoom 缩放错位** | 根元素 `zoom` 是缩放唯一入口（`utils/uiScale.ts`）；`zoom` **不改变 `window.innerWidth`** | canvas 物理尺寸与视口会在 125% 下错位。须按 `devicePixelRatio × zoom` 计算，且监听 `UI_SCALE_EVENT` 重算 |
| **零 WebGL 基建** | 仓库现无任何 WebGL 使用（仅 2D canvas）；WebView2（webview2-com 0.39）＋ Tauri 2.12.1 提供 WebGL2 | `context lost`、GPU 降级、显存回收全无先例。**须实现无 WebGL2 时自动降级到 DOM 视图**，且失败要看得见不能静默 |
| **CSP** | `default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; img-src 'self' data: https: blob: asset:` | WebGL 本身不需额外放行；纹理须走 `data:`／asset 协议。**注意 `connect-src` 含 `https:`，运行时外链字体在 CSP 上"合法"但违背离线优先定位——须靠代码层而非 CSP 拦** |
| **文本层职责** | troika 支持 `lang` 属性显式选 CJK 字体；缺字走 resolver 回退 | 若本地内嵌字体子集，需自行做字符集覆盖管理，避免缺字静默变豆腐块 |

### 未核验项（本文结论的已知盲区）

- **未实测** WebView2 环境下的实际帧率与 `devicePixelRatio` 行为。
- **未实测** `zoom` 生效时 canvas 与 DOM 的像素级对齐。
- **未跑** `npx impeccable detect`、`tsc --noEmit`、实机渲染、屏幕阅读器走查。
- **未量化** 中文子集字体体积（取决于选用字库与子集化策略，可能显著大于 934KB）。

### 建议落地顺序

1. **最小可运行原型**：three 场景外壳 ＋ 令牌驱动材质 ＋ 一个背景层页面，实测 CSP、zoom 对齐、GPU 降级、产物体积。
2. **本地中文字体方案**：确定内嵌字库与子集化策略，确认零运行时网络请求。
3. **降级路径**：无 WebGL2 时的 DOM 兜底视图。
4. **空间化数据视图**：从记忆图谱切入（已有 ECharts 实现可作对照基准）。
5. 上述四项验证通过后，再评估是否扩大 3D 覆盖面。

## Reintroduction conditions

本文为评估文档，未落地任何方案。以下情况出现时需重新评估：

1. **空间化数据视图被证明有明确用户价值**（而非仅技术可行）——此时可扩大 3D 覆盖面。
2. **中文 3D 文本方案被验证可行**（本地内嵌子集字体无缺字、体积可接受）——此时「界面元素三维化」的技术障碍解除一大部分。
3. **出现必须 3D 呈现的核心交互**（如可旋转的模型查看器、体积化数据探索）——此时混合渲染的边界需要重新划定。

## 附：实测数据来源

| 数据 | 值 | 来源 |
|---|---|---|
| 主题色彩令牌 | 116 | `tokens.css` @theme 块派生（`check-theme-tokens.mjs` 同源逻辑） |
| `var()` 引用 | 3619 处 / 189 种令牌名 | `apps/desktop-tauri/src` + `packages` 全量 grep |
| 组件规模 | 167 tsx / 24 css / 14413 行 CSS | 文件系统统计 |
| MarkdownBody | 981 行 | `apps/desktop-tauri/src/components/MarkdownBody.tsx` |
| preview 共享层 | 37 文件 / 2842 行 | `packages/preview/src` |
| 前端产物体积 | 19MB / 248 文件 | `release/frontend` |
| three 本体 | 663KB → 128KB gzip | `npm pack three@0.186.1` 实测 |
| troika-three-text | 934KB unpacked | `npm pack troika-three-text@0.52.5` 实测 |
| ez-tree 体积构成 | 4.02MB 总 / 3.88MB base64 纹理 / 46KB 代码 | `npm pack @dgreenheck/ez-tree@1.1.0` 实测 |
| R3F / drei 版本 | 9.8.2 / 10.7.9 | npm registry |
