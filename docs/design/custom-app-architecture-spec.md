# Hermix 自定义应用架构设计 Spec

> 目标：把"用户说新建应用 → LLM 写网页工程 → dev 跑得动 → 打包成单一离线 exe → 用户双击即用"整条需求链串成闭环。  
> 设计口径：**第一性原理**，不迁就当前已实现代码（现状差异见 §5）。

---

## 0. 一句话定义

让用户用自然语言说"新建一个 XXX 应用"，LLM 产出一个**多文件前端工程**（HTML/CSS/JS + 资源文件），可以：

- **开发模式**秒级运行、热更、测试
- **一键打包**成**单一离线 exe** 分发，断网也能跑

即：**LLM 写网页工程 → dev 跑得动 → 打包成独立 exe → 用户双击即用，零网络依赖。**

---

## 1. 已拍板的核心决策

| 决策点           | 结论                                                                | 第一性原理依据                                                                                 |
| ------------- | ----------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| 要不要壳          | **要**，薄 Rust + 系统 WebView2                                        | 网页自己变不成 exe；必须有一个原生进程去 owning 窗口、挂载浏览器引擎、暴露 Native API 桥（writeFile/readFile/exec/win.*） |
| 壳的形态          | **可复用模板 + 内嵌资源 = 每 app 一个 exe**                                   | 避免 Electron 100MB+ / 每个 app 带整套 Tauri 运行时造成的体积爆炸                                        |
| LLM 写啥语言      | **始终 vanilla HTML/CSS/JS（TSX 退役）**                                | 零编译 = 打包最快 + 运行质量最高；"复杂"所需的组件/状态由壳原语提供，LLM 永不碰 TSX/构建                                   |
| 壳预置前端能力       | **组件(Web Components) + 响应式状态 store + 可选 Preact 全局 + 图标库(Lucide)** | 把"框架能力"下沉到壳，LLM 永远写 vanilla，零构建、零 npm、离线可用                                              |
| 外部库（pdf.js 等） | **创作期 vendor 到 `app_dir/vendor/`，运行期零网络**                         | 断网白屏不可接受；库下载一次嵌进 exe 即可                                                                 |
| 运行时加载         | **虚拟协议（WebResourceRequested）内存直喂**                                | ES module / CORS / fetch / 相对外联 / worker 全活，不落盘、启动最快、质量最高                               |
| 打包形态          | **walk `app_dir` → 归档（relpath→bytes）→ 嵌 PE 资源节**                  | 多文件工程进，单 exe 出，外联相对路径存活                                                                 |

---

## 2. 端到端流程

```
[用户] "新建一个 XXX 应用" + 需求描述
   │
   ▼
[propose 阶段] LLM 拍板：是否用壳高级原语(组件/状态/路由) · 是否需外部库(vendor) · UI 复杂度
   │
   ▼
[产出] app_dir/  ← 多文件工程（HTML/CSS/JS + 资源 + 可选 vendor/）
   │
   ▼
[dev 运行] 虚拟协议直接 host app_dir（无打包、热更）─────► 用户验收
   │                                                      │
   │                                                      ▼（不满意回到产出）
   ▼
[打包] walk 全部文件 → 归档 → 嵌壳模板 PE 资源节（**无编译步骤，零编译报错面**）
   │
   ▼
[分发/运行] 壳读资源 → 虚拟协议直喂 WebView2 → index.html 加载，相对外联全解析
```

**壳原语兜底**：若 LLM 误用框架 import，validate 阶段拦截并提示改用 `Hermix.*` 壳原语（见 §3.8）。

---

## 3. 组件职责

### 3.1 app_dir 工程结构（LLM 产出）

```
app_dir/
  index.html            # 入口，引用 ./src/*.js  ./styles.css  ./assets/*  ./vendor/*
  src/*.js              # ES modules（import/export 干净写法）
  styles.css            # 效果样式
  assets/logo.png       # 资源文件（图标/logo 等）
  vendor/pdf.min.js     # 外部库本地化（可选，创作期拉入）
```

- 入口自行挂载（vanilla 原生，`document.getElementById('app').appendChild(...)`）；用壳组件时由 `Hermix.defineComponent()` 注册自定义元素
- **禁止** `import from 'npm包'`（零 node_modules 模型）

### 3.2 dev runner（开发模式）

- 用虚拟协议（或 loopback HTTP）直接 host **磁盘上的 `app_dir`**，跳过打包
- 秒级热更，与打包后运行**共用同一加载机制** → dev / prod 行为一致，不出现"dev 好好的打包就挂"
- 触发：用户在 Hermix 里点"运行/预览"

### 3.3 打包器 packager

- **walk** `app_dir` 全部文件（含 `vendor/`、`assets/`、`icons/`）→ 打成归档
- 把归档嵌进壳模板的 PE 资源节（`RT_RCDATA`）
- **无编译步骤**：LLM 写 vanilla，壳提供组件/状态原语，不需要 esbuild，因此**零编译报错面**
- 注：壳预置的 store/组件/图标/路由是**壳自身资源**，不在 `app_dir` 内，打包时无需处理

### 3.4 壳 hx-shell

- 薄 Rust + WebView2（系统自带 Edge，不打包 Chromium）
- **预置前端能力（壳资源，不在 `app_dir` 内）**：
  - **响应式状态 store**：`Hermix.store(initial)` —— Proxy 驱动，订阅即重渲染，补 vanilla 状态短板
  - **组件原语**：`Hermix.defineComponent(name, {state, render, onMount})` —— 薄封装原生 Custom Elements + Shadow DOM；重交互可改用预置 **Preact 全局 `HermixUI`**（~10KB）
  - **图标库**：`Hermix.icon('settings')` 返回内嵌 Lucide SVG 字符串（精选子集，零外链、零字体请求、离线）
  - **轻量路由**（已预置，app 按需用）：`Hermix.route()` hash 路由 helper —— 单页多视图免手写
  - **主题自管理（壳零介入）**：app 自带 CSS 主题，壳**不注入**任何 `--color-*` 变量、也**不暴露**宿主明暗（`Hermix.theme` 不存在）；主题完全由 app 自身设计
  - **本地存储封装**：`Hermix.storage` 统一 localStorage/IndexedDB 键值接口
- **渲染性能加固**（Rust 侧）：GPU 加速 WebView2（默认开）、禁用 devtools/上下文菜单/状态栏、单 WebView 实例复用、预创建预热、正确 DPI 缩放
- 暴露 **Native API 桥**：writeFile / readFile / exec / win.* 等
- 启动时读 PE 资源 → 交给加载机制

### 3.5 加载机制（虚拟协议，推荐）

- WebView2 `WebResourceRequested` 拦截 `app://` 方案
- 内存里按 `relpath` 查归档 → 直接喂 bytes（带正确 MIME）
- **不落盘、无 temp 清理包袱、启动最快**
- ES module / fetch / 相对外联 / worker **全部存活**
- 备选方案：本地 loopback HTTP 服务（实现更简单，但需管 temp 清理，启动略慢）

### 3.6 vendor 机制（核心补件，当前缺失）

- **职责**：给 LLM 一个**创作期**"下载 X 库到 `app_dir/vendor/`"的能力（工具/管线步骤）。**只创作期联网，运行期零网络**。
- 例：pdf.js → `vendor/pdf.min.js` + `vendor/pdf.worker.min.js`
- SKILL 护栏配合：**禁止 npm import，要库就走 vendor + 相对外联**

#### 3.6.1 下载源策略（国内网络优先，github 兜底）

**硬约束：开发机网络环境不可靠（国内常连不上 github / npm 官方源），下载源必须多镜像有序回退，且国内源优先。**

- **源优先级（有序，依次尝试直到命中）**：
  1. **可配置企业/内网源**（用户或部署环境自定义，最高优先，离线内网场景）
  2. **国内公共镜像**：
     - npm 类库（UMD/打包产物）：`registry.npmmirror.com`（淘宝 npm 镜像，含完整包与 tarball）
     - CDN 类（jsdelivr 替代）：`cdn.jsdelivr.net` 不稳时退 `fastly.jsdelivr.net` / `gcore.jsdelivr.net`
     - git raw 类（直接抓单文件）：优先 **gitee 镜像仓库**（`gitee.com/<mirror>/raw/...`），github 对应的国内 mirror 很多
  3. **官方源兜底**：`github.com`、`registry.npmjs.org`、`unpkg.com`、`cdn.jsdelivr.net`
- **回退机制**：单源超时/404/TLS 失败 → 自动试下一源；全部失败 → 明确报错并附"可用源清单"，**绝不静默产出残缺 vendor**（否则 exe 里库不全 → 运行期白屏）
- **源可覆盖**：下载源列表做成配置文件（非硬编码），支持用户填内网/私服地址 —— 因为"连不上 github"也可能意味着需要走公司私仓
- **实现位置**：下载逻辑放在**主程序/Hermix 侧**（有完整网络栈与镜像配置能力），不是放进壳 exe；壳只消费已落地的 `vendor/` 目录

#### 3.6.2 版本与缓存（防止破坏 + 支持离线开发）

- **版本锁定**：vendor 下载必须固定版本号（`pdfjs-dist@4.x.x`），禁止每次拉 `latest` —— 否则 LLM 某次重生成拉到破坏性大版本，app 悄无声息崩
- **全局缓存**：已下载的 `<库@版本>` 缓存在 Hermix 本地（`~/.hermix/vendor-cache/`），同版本二次生成**零网络**、断网也能用；缓存键 = `库名@版本@文件指纹`
- **校验**：下载后比对预期大小/SHA（可选），残缺文件不入 `vendor/`
- **清单落地**：`vendor/manifest.json` 记录每个文件的源 URL + 版本 + 校验值，便于审计与重现

### 3.7 校验规则 validate

- **禁**：运行时 `http(s)://` 远程外链（断网白屏，违反离线底线）
- **放**：本地相对路径 `<script src="./vendor/x.js">` / `<link href="styles.css">`
- 当前规则"禁止一切外链"需改为"只禁远程、放行本地相对"

### 3.8 SKILL 护栏（引导 LLM 不走歪）

- **始终 vanilla HTML/CSS/JS**；复杂 UI 用壳原语（`Hermix.defineComponent` / `Hermix.store` / `HermixUI`），**TSX 退役**
- 禁止 `import from 'npm包'`、禁止 `import React` 等框架 import —— 要框架能力就走壳全局 `Hermix.*`
- 需要外部库 → vendor + 相对外联，不允许 CDN（运行时 `http(s)` 远程外链一律禁）
- 图标一律用 `Hermix.icon('name')`，**禁止外链图标字体 / 引 SVG CDN**
- 资源文件放 `assets/`，相对引用

---

## 4. 关键场景走查

**场景 A — 纯 vanilla 计算器**  
`app_dir` 单 `index.html` + `app.js` → dev 跑 → 打包 walk → exe。零编译、零依赖、最小体积。

**场景 B — PDF 查看器（用 pdf.js）**

1. propose 识别"需 PDF 处理" → 决定 vendor pdf.js
2. vendor 机制下载 `pdf.min.js` + `pdf.worker.min.js` 到 `vendor/`
3. `index.html` 写 `<script src="./vendor/pdf.min.js">`
4. 运行时 `GlobalWorkerOptions.workerSrc = 'app://vendor/pdf.worker.min.js'`（虚拟协议喂 worker）
5. **断网可用**，零运行时网络

**场景 C — 复杂仪表盘（vanilla + 壳原语，无 TSX）**

1. propose 拍板"用壳组件/状态原语"
2. LLM 写 `app.js`：用 `Hermix.defineComponent('dashboard-card', {...})` 定义卡片组件 + `Hermix.store()` 管全局状态 + `Hermix.icon('chart')` 取图标
3. 打包仅 walk（无编译），壳预置的 store/组件/图标在运行时由壳注入
4. 全程 vanilla JS，零构建、零 npm、离线可跑

**场景 D — 多视图应用（壳路由 + 图标 + 主题同步）**

1. LLM 用 `Hermix.route()` 定义 `/list` `/detail` 两视图
2. 各视图用 `Hermix.icon()` 取导航图标，主题由 app 自带 CSS 自理（壳不注入宿主令牌）
3. 运行时虚拟协议直喂，视图切换无整页刷新、无白屏

---

## 5. 与当前实现差异（migration gaps）

| 现状                               | 目标                           | 缺口                                   |
| -------------------------------- | ---------------------------- | ------------------------------------ |
| 单 HTML inline 进 PE 资源            | 目录归档多文件进 PE                  | packager 重写                          |
| 运行时单一 HTML 字符串 / `set_content`   | 虚拟协议内存直喂目录                   | 壳加载逻辑改                               |
| validate 禁一切外链                   | 禁远程、放本地相对                    | validate 改                           |
| 无 vendor 入口                      | 创作期拉库能力                      | 新增工具/步骤                              |
| dev 写单 `.dev.html` 加载            | dev 走虚拟协议 host 目录            | dev runner 改                         |
| React inline / TSX inline 进 HTML | 壳预置组件/状态原语 + 图标库（TSX 退役，零构建） | packager 去 esbuild；壳注入 `Hermix.*` 全局 |

---

## 6. 开放问题（已决 / 待首日验证）

### 6.1 已决策 / 已关闭

1. **归档格式 → 默认裸 `relpath→bytes` 清单**（零解压依赖、随机访问快、启动最快、零第三方依赖）。  
   自序列化 map（TLV/MessagePack/JSON）即可；exe 体积敏感再上 zip（`flate2` 编译进壳，不计入 app 体积）。逻辑：自定义 app 通常数百 KB~几 MB，不压缩可接受（YAGNI）。
2. **临时目录生命周期 → 已关闭**。已采用虚拟协议（不落盘），此问题不存在；仅 loopback 备选方案才需管 temp，而 loopback 已降级未采用。
3. **壳分发 → 静态链接 WebView2 loader（~3–5MB 烘进 exe）+ 依赖系统 Evergreen 运行时**。不捆绑固定版运行时（150MB+ 不现实）。边缘：目标机未装 Evergreen 时首跑引导安装 / 报错提示，不静默。
4. **壳预置库取舍 → 必装四件套（组件/状态/图标/路由）+ 额外预置 Preact 全局 `HermixUI`（~10KB，复杂兜底，仍 vanilla 调用非 TSX）；不预置 anime.js（占体积+低频，CSS 动画覆盖 90%，真需走 vendor）与 day.js（低频，LLM 手写或 vendor）**。原则：高频刚需才预置。
5. **图标子集 → 精选 60–120 枚 Lucide 子集**（不全嵌）；壳留 `Hermix.icon.register(name, svg)` 供 app 运行期补图标。全嵌 1500+ 估算：裸 ~500–750KB / JS map ~0.8–1.2MB(min) / gzip ~120–150KB；精选 100 ≈ 裸 30–50KB / gzip ~10KB（构建图标包时需精确称重）。

### 6.2 首日验证清单

1. **pdf.js worker**：虚拟协议下 `GlobalWorkerOptions.workerSrc='app://vendor/pdf.worker.min.js'` 须确认 ①`WebResourceRequested` 拦截 worker 脚本的 `app://` 加载；②worker 内部 fetch（分块 PDF）也走同一拦截。验证：写最小 pdf.js demo 在壳 dev runner 下实测；失败回退「worker 内联进主 bundle」或 pdf.js 自带 fake worker 模式。
2. **默认镜像源可达性**（已实测，须在开发机重跑 `scripts/vendor-mirror-probe.ps1`）：
   - 沙箱实测（仅供参考）：`npmmirror` 111ms / `gitee` 125ms / `github` 260ms / `npmjs` 6475ms 可达；`jsdelivr`(fastly/gcore/cdn) 与 `unpkg` 全超时不可达。
   - **默认源列表 = 可达源按延迟升序回退**（`npmmirror → gitee → github → npmjs`；jsdelivr/unpkg 实测不可达暂不入默认）。
   - **配置格式（JSON 源数组 + 超时/重试）**：
     ```json
     {
       "vendorSources": [
         { "id": "npmmirror", "kind": "npm",    "base": "https://registry.npmmirror.com", "timeoutMs": 8000, "retries": 2 },
         { "id": "gitee",     "kind": "gitraw", "base": "https://gitee.com",             "timeoutMs": 8000, "retries": 2 },
         { "id": "github",    "kind": "gitraw", "base": "https://github.com",            "timeoutMs": 10000,"retries": 2 }
       ],
       "defaultTimeoutMs": 8000,
       "defaultRetries": 2
     }
     ```
     `kind` 决定 URL 拼法（`npm`→registry tarball；`gitraw`→raw 文件路径）。全部失败明确报错，**绝不静默残缺 vendor**。
3. **sql.js WASM 加载 + 持久化**：虚拟协议下 `sql-wasm.wasm` 须确认 ①`app://vendor/sql-wasm.wasm` 经 `WebResourceRequested` 正常拦截（WASM 字节流 / MIME）；②`Hermix.sqlite` 整库导出 → hx-shell `writeFile` 落盘 → 重启 `readFile` 重载 往返一致。验证：写最小 todo/台账 demo 在壳 dev runner 下实测；失败回退「WASM 文件 base64 内联进 JS bundle」或「rusqlite 原生桥」。

---

## 7. 壳可强化能力清单（扩容思考）

把"框架能力"下沉到壳是核心思路。下面按价值排序列出已预置 / 已加固的能力（已拍板，落地见壳改造计划）。

### 7.1 必装六件套（高频刚需，零成本，无条件提供）

| 能力     | 壳暴露                              | 解决啥                                                  |
| ------ | -------------------------------- | ---------------------------------------------------- |
| 响应式状态  | `Hermix.store()`                 | vanilla 缺响应式状态，最痛的短板                                 |
| 组件原语   | `Hermix.defineComponent()`       | 组件化 + 局部作用域 + 生命周期                                   |
| 图标库    | `Hermix.icon('name')`            | 项目已用 Lucide，内嵌精选子集，零外链                               |
| 轻量路由   | `Hermix.route()`                 | 单页多视图免手写 hash 路由（§6.1 已拍板必装；原 §3.4/§7.2 标"可选"作废）     |
| 本地存储封装 | `Hermix.storage`                 | 统一 localStorage/IndexedDB，按 appId 自动命名空间隔离（规则见 §7.4） |
| 主题自管理  | 壳**不**注入 `--color-*`、**不**暴露宿主模式 | app 自带主题、按自身需求设计，壳零介入（避免宿主与 app 视觉耦合）                |

### 7.2 预置库（已承诺预置）

| 能力        | 壳暴露        | 备注                                      |
| --------- | ---------- | --------------------------------------- |
| Preact 全局 | `HermixUI` | **已承诺预置**：重交互兜底，~10KB，仍 vanilla 调用非 TSX |

> **明确不预置（YAGNI，按 §6.1 决策）**：动画库（anime.js）、日期库（day.js）。理由：CSS animation/transition 覆盖 90% 动画场景；日期格式化 LLM 手写几行或创作期 vendor 即可；预置只占壳体积 + 增维护面。真需要时在 `app_dir/vendor/` 拉入，运行期零网络。

### 7.3 渲染性能加固（Rust 侧，非 app 侧）

- **GPU 加速**：WebView2 默认开 GPU 合成，大列表/动画不卡
- **禁用冗余 UI**：关 devtools、上下文菜单、状态栏，减绘制开销
- **单 WebView 复用**：同 app 多窗口复用实例，避免重复初始化
- **预创建预热**：壳启动即创建 WebView 并 navigate 到占位，首屏秒开
- **DPI/缩放**：正确设 `SetScale`/`Bounds`，高分屏不糊
- **app 侧引导**（SKILL 提示，非壳代码）：`content-visibility:auto`、`contain`、`will-change` 谨慎用、长列表虚拟滚动

### 7.4 壳强化能力（已承诺）

- **本地存储封装** `Hermix.storage`（已升 §7.1 必装）：统一 localStorage/IndexedDB 接口，app 免自理持久化。**必须按 appId 自动命名空间隔离**（key 前缀 `hx:<appId>:`、IndexedDB 库名 `hx-app-<appId>`），否则多 app 共享同一 WebView origin 会互相污染、甚至污染宿主 Hermix 自身存储
- **结构化本地数据库** `Hermix.sqlite`（已承诺预置）：壳内置 **sql.js（SQLite 编译为 WASM）**，app 免自理即用完整 SQL 引擎做结构化数据存取——台账 / 报表 / 筛选 / 聚合天然契合，查询力比 `Hermix.storage` 的 NoSQL KV 强一个量级。  
  - **默认实现 = WASM 零原生依赖**：全部在 WebView2 内运行，不引 rusqlite、不增大壳 Rust 体积、离线零网络；经虚拟协议 `app://vendor/sql-wasm.wasm` 加载。  
  - **持久化**：sql.js 内存 DB，写入时整库导出经 hx-shell 文件 API（`writeFile`，按 appId 隔离目录 `~/.hermix/custom-apps-data/<appId>/db.sqlite`）落盘；读取时 `readFile` 整库载入内存。  
  - **与 `Hermix.storage` 分工**：`storage` 管轻量 KV（配置 / 草稿 / UI 状态）；`sqlite` 管结构化、需查询的数据集。二者都按 appId 隔离，互不污染。  
  - **取舍（WASM 通病）**：纯 WASM 模式每次写需导出整库字节存档，单本台账数千条级完全够；数十 MB 高频写有开销。若未来出现大数据高频写场景，再上 **rusqlite 原生桥**（hx-shell Rust 嵌 SQLite 直落盘增量写）——列为可选增强，不默认预置。
- **离屏截图** `Hermix.screenshot()`：把当前视图导出 PNG（报表 / 分享场景）
- **无障碍基线**：壳强制 focus 管理 + 语义标签提示，降低各 app 重复成本
- **重计算分流（避免阻塞 UI 线程）**：① 纯 JS 重算 → 用 Web Worker（Web 标准；虚拟协议下 worker 脚本加载同 §6.2 #1 验证）；② 需原生 / OS 能力（大文件读写、调外部程序、本地库）→ 走 hx-shell 原生桥（writeFile/readFile/exec/win.*），由**壳自身 Rust 线程**异步执行。**此"Rust 侧"指每个自定义 app 自带的 hx-shell 薄壳，不是主项目 Hermix Desktop —— 自定义 app 运行时是独立 exe，与主项目零耦合。**

---

## 8. 落地开发计划（壳改造路线图）

> 把 §1–§7 的决策与能力清单转化为可执行代码改造。按依赖分 Phase，每 Phase 含**目标 / 涉及文件（精确）/ 关键实现 / 验收 / 依赖**。现状代码已探查（见 §8.0）。

### 8.0 现状基线（已探查，2026-08-21）

| 维度 | 现状 | 目标落差 |
|---|---|---|
| 加载 | hx-shell `with_html(html_string)` 单 HTML 字符串（`main.rs:1308`）；`with_initialization_script` 注入 `window.hx.*`（原生桥 RPC）；`hxfile://` 协议已存在 | 无 `app://` 多文件内存直喂 |
| 嵌入 | PE `RT_RCDATA 1000`(HTML payload) / `1001`(图标)；尾部拼接旧格式兜底；`icon_library::embed_app_payload` 写资源 | 只支持单 HTML，不支持多文件归档 |
| 打包 | `hx_packager.rs` 单 HTML 模式 + `compile_tsx`(esbuild) 模式 | 与 §1「始终 vanilla、TSX 退役、零编译」冲突，待去 esbuild |
| 校验 | `validate.rs::check_frontend_quality` 禁 CDN/远程 `<script>`（话术"Vue/React 必须内联"，过时） | 需改为「禁运行时 `http(s)` 远程、放行本地相对」(§3.7) |
| 图标 | `apps/hx-shell/resources/icons/*.svg` 已含**全集 Lucide**（1500+） | 落地时精选 60–120 编译进 JS map，其余 `Hermix.icon.register` 补 |

### 8.1 Phase 0 — 归档嵌入通道（基础设施）

- **目标**：支持多文件归档进 PE（不只单 HTML），零压缩零解压（§6.1 决策）。
- **涉及文件**：
  - `crates/hermix-plugin/src/hx_packager.rs`：新增 `build_archive(app_dir) -> Vec<u8>`（walk → `Vec<(relpath, bytes)>` → 序列化裸清单 `[count:u32][[plen:u32][path][dlen:u32][data]]...`）
  - `crates/hermix-plugin/src/icon_library.rs`：`embed_app_payload` 扩展写归档 blob 到新资源 id（如 `1002`），保留 `1000/1001` 兼容
  - `apps/hx-shell/src/main.rs`：新增 `load_archive()` 读 `1002` → `HashMap<String, Vec<u8>>`；旧 `1000` 单 HTML 仍作兜底
- **关键实现**：序列化格式选裸清单（非 zip）——自定义 app 通常数百 KB~几 MB，不压缩可接受；启动零解压、随机访问最快、零第三方依赖（zip 仅作 exe 体积敏感时的可选增强，见 §6.1）。
- **验收**：`cargo test` 归档 round-trip（build→embed→load 字节一致）；旧单 HTML exe 仍可读。

### 8.2 Phase 1 — 虚拟协议加载（核心）

- **目标**：`app://` 内存直喂目录，取代 `with_html` 单字符串；dev/prod 共用 handler（§3.5 / §3.2）。
- **涉及文件**：`apps/hx-shell/src/main.rs`
  - 新增 `app_protocol_handler`：`request.uri().path()` → 查归档 map /（dev 模式）读磁盘 `app_dir` → 返回 bytes + `mime_type()`（复用现有 `mime_type()`）
  - `WebViewBuilder::with_custom_protocol("app", app_protocol_handler)`
  - 入口 `with_html(html_source)` → `with_url("app://app/index.html")`；dev `--html` 改 `--app-dir` host 磁盘目录
- **关键实现**：WebResourceRequested 必须拦截 ES module（`type="module"`）、`fetch()`、worker 脚本、`app://` 下相对外联的全部请求；MIME 必须正确（`.js`→`text/javascript`、`.wasm`→`application/wasm`、`.svg`→`image/svg+xml`）。
- **验收**：多文件 app（index.html + src/*.js + styles.css + vendor/*.js + assets/*）全部加载；ES module / `fetch` / 相对外联 / worker 存活（同日 §6.2 #1）。

### 8.3 Phase 2 — packager 重写（多文件归档）

- **目标**：walk 多文件 → 嵌入 PE `1002`；去 esbuild/TSX 编译（§1）。
- **涉及文件**：`crates/hermix-plugin/src/hx_packager.rs`、`icon_library.rs`
  - `package()` 重写：`build_archive(app_dir)`（§8.1）→ `embed_app_payload(archive, 1002)`；删除 `compile_tsx` 调用路径（函数可保留但 `package` 不再调用，或整段删除）
  - 保留 `PackageMode` 兼容枚举（hermix-tools/dashboard 仍引用）
- **验收**：`cargo build` 后单 exe 双击跑多文件 app，断网可用；旧单 HTML 产物仍兼容（§8.1 兜底）。

### 8.4 Phase 3 — 壳预置能力 `Hermix.*`（最大块）

- **目标**：注入 `window.Hermix.*` 纯前端能力层（不依赖 Rust IPC，除 storage/sqlite 落盘走 `window.hx.*`）。
- **涉及文件**：`apps/hx-shell/src/main.rs`（`with_initialization_script` 追加 Hermix 段）+ 新增 `apps/hx-shell/resources/hermix-runtime.js`（或内联进 init script）；图标子集来源 `resources/icons/*.svg`（已全集，精选 60–120 编译进 JS map）
- **能力清单（按 §7 已承诺顺序）**：
  - 3.1 `Hermix.store(initial)`：Proxy 响应式 state + 订阅即重渲染（补 vanilla 状态短板）
  - 3.2 `Hermix.defineComponent(name, def)`：薄封装原生 Custom Elements + Shadow DOM（局部作用域 + 生命周期）
  - 3.3 `HermixUI`：Preact 全局（~10KB，内联 runtime 或壳资源），复杂兜底仍 vanilla 调用非 TSX
  - 3.4 `Hermix.icon(name)`：内嵌 Lucide 子集 JS map（精选 60–120）；`Hermix.icon.register(name, svg)` 运行期扩展；全集 SVG 已备于 `resources/icons/`
  - 3.5 `Hermix.route()`：hash 路由 helper（单页多视图免手写）
  - 3.6 `Hermix.storage`：localStorage/IndexedDB 封装 + **appId 命名空间**（key 前缀 `hx:<appId>:`、IDB 库 `hx-app-<appId>`）；appId 由壳 spawn `--app-id` 传入（复用现有 `--app-dir` 机制，避免多 app 互污染）
  - 3.7 `Hermix.sqlite`：sql.js WASM 预置（`app://vendor/sql-wasm.wasm`）+ 持久化（内存 DB 整库导出 → `window.hx.writeFileBytes` 落 `~/.hermix/custom-apps-data/<appId>/db.sqlite`；`readFileBytes` 重载）—— 取舍见 §7.4
  - 3.8 `Hermix.screenshot()`：离屏截图 PNG（WebView `CapturePreview` API 或 `window.hx.exec` 系统截图）
  - 3.9 无障碍基线：壳强制 focus 管理 + 语义样式注入（降低各 app 重复成本）
- **验收**：最小 demo 用 store+component+icon+route+storage+sqlite 跑通；与 §6.2 #1(pdf.js worker) / #3(sql.js wasm) 首日验证一起。

### 8.5 Phase 4 — vendor 机制

- **目标**：创作期拉库到 `app_dir/vendor/`，运行期零网络（§3.6）。
- **涉及文件**：主程序侧（下载逻辑放 Hermix 侧，非壳 exe，见 §3.6.1）—— 新增 `crates/hermix-tools/src/tools/vendor.rs`（或在 `custom_app_manager` 扩展 `vendor` action）；`~/.hermix/vendor-cache/` 全局缓存；`custom-app-development` SKILL 增 vendor 步骤 + 护栏
  - 下载器：多镜像有序回退（§3.6.1 默认 `npmmirror→gitee→github→npmjs`）、版本锁定（禁 latest）、SHA 校验、`vendor/manifest.json`
- **验收**：pdf.js 经 vendor 落地 `app_dir/vendor/` + manifest；断网打包运行可用（与 §8.4 3.7 / §6.2 #1 协同）。

### 8.6 Phase 5 — validate 改写（§3.7）

- **目标**：禁运行时 `http(s)` 远程外链，放行本地相对。
- **涉及文件**：`crates/hermix-plugin/src/validate.rs`（`check_frontend_quality`）
  - 规则：检测 `<script src="http...">` / `<link href="http...">` / `<img src="http...">` 等运行时远程 → error；放行 `./vendor/x.js`、`styles.css`、`assets/*` 本地相对
  - 话术更新：去掉"Vue/React 必须内联进单文件 HTML" → "用 `Hermix.*` 壳原语 + vendor 本地相对外联，禁止运行时远程 `http(s)`（断网白屏）"
  - 保留：`<input type="file">` 违例（桌面必须用 `hx.openDialog`）、标题栏缺失 warning
- **验收**：新增「本地相对外联应通过」测试用例；`cargo test` validate 全过。

### 8.7 Phase 6 — dev runner 对齐（§3.2）

- **目标**：dev 走虚拟协议 host 磁盘 `app_dir`，与 prod 共用 `app://` handler。
- **涉及文件**：`apps/desktop-tauri/src-tauri/src/commands_custom_app.rs`、`crates/hermix-tools/src/tools/custom_app_manager.rs`（dev action）
  - "运行/预览" → spawn `hx-shell --app-dir <dir>`（不再生成 `.dev.html` 单文件）
- **验收**：dev 与 prod 行为一致（同一加载机制，不出现"dev 好好打包就挂"）。

### 8.8 Phase 7 — 首日验证清单（汇总 §6.2）

1. pdf.js worker 虚拟协议加载（§6.2 #1）
2. sql.js WASM 虚拟协议加载 + 持久化（§6.2 #3）
3. 默认镜像源开发机重跑（§6.2 #2，固化默认源顺序）
4. 归档 / 虚拟协议 / sqlite 端到端 demo（§8.1–§8.4 串联）

### 8.9 依赖与顺序

```
Phase 0 (归档通道) ──┬─> Phase 1 (虚拟协议) ──> Phase 3 (Hermix.* 注入)
                     └─> Phase 2 (packager 重写)
Phase 4 (vendor)    独立，但 Phase 3 的 pdf.js/sql.js 依赖其落地
Phase 5 (validate) / Phase 6 (dev runner) 独立，可与 Phase 1/2/3 并行
Phase 7 (首日验证)  收尾
```

### 8.10 风险与备注

- **§7.4 引用的「§6.2 #6」已不存在**（现 §6.2 为 #1/#2/#3），worker 脚本加载验证归 §6.2 #1，本计划已全部对齐（已修正 §7.4 引用）。
- **图标子集**：`resources/icons/` 已含全集 SVG，落地时精选 60–120 编译进 JS map，其余 `Hermix.icon.register` 运行期补；不全嵌以控体积。
- **WASM 持久化取舍**见 §7.4（纯 WASM 每次写导出整库；数十 MB 高频写再上 rusqlite 原生桥，列为可选增强）。
- **不预置项**（YAGNI，按 §6.1）：anime.js / day.js —— 真需走 vendor 或 LLM 手写。

### 8.11 落地进度（2026-08-21）

| Phase | 标题 | 状态 | 落地文件 |
|---|---|---|---|
| 0 | 归档嵌入通道 | ✅ 已完成 | `crates/hermix-plugin/src/app_archive.rs` (新), `icon_library.rs`（+1002资源）, `hx_packager.rs`, `apps/hx-shell/src/main.rs`(+归档读取) |
| 1 | app:// 虚拟协议 | ✅ 已完成 | `apps/hx-shell/src/main.rs`（+`app_protocol_handler` + `with_url` 切换） |
| 2 | packager 重写（去 TSX） | ✅ 已完成 | `crates/hermix-plugin/src/hx_packager.rs`（删 `compile_tsx` + React UMD 注入 + `detect_tsx` 拦截） |
| 3 | 壳预置 `Hermix.*` | ✅ 已完成 | `apps/hx-shell/resources/hermix-runtime.js`（新, 500 行）, `apps/hx-shell/src/main.rs`（`include_str!` 嵌入 + `--app-id` 注入） |
| 5 | validate 改写 | ✅ 已完成 | `crates/hermix-plugin/src/validate.rs`（regex 远程外联检测 + CDN 黑名单） |
| 4 | vendor 拉库 | ✅ 已完成 | `crates/hermix-tools/src/tools/vendor.rs`（新, 单源 + sha256 + 全局缓存 + 落 vendor/ + manifest.json）, `Cargo.toml`(+sha2), `tools/mod.rs`(+pub mod + register) |
| 6 | dev runner 走虚拟协议 | ✅ 已完成 | `apps/hx-shell/src/main.rs`(APP_DIR_FOR_DEV + app_protocol_handler 磁盘 fallback), `hx_packager.rs::run_dev`(改用 `--app-dir --app-id` 走磁盘 host) |
| 7 | 首日验证 | ⏳ Pending | pdf.js worker + sql.js WASM + 镜像源实测（§6.2 #1/#3/#2）；由 Phase 0-6 提供全部前置能力，可在开发机上手动验证 |

