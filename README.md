# Hermix Desktop

本地优先的 Windows 桌面 AI 助手。Rust 后端 + React 前端，经 Tauri v2 打包为免安装便携应用：
对话、工具执行、定时自动化、自定义应用（custom app）都在本机闭环，数据不依赖任何云端托管。

当前为 **Windows x64（MSVC）单平台**项目，无跨平台目标，也没有云服务依赖。

---

## 核心能力

| 能力 | 说明 |
|---|---|
| Agent 对话 | 多 provider LLM 接入（内置十余家厂商注册表 + 自定义 OpenAI 兼容接入），工具调用、子代理派发、上下文压缩、审批引擎 |
| 自定义应用 | 用户以自然语言或模板生成真代码应用：独立窗口、真分发、可复用技能，是唯一的"用户自造能力"入口 |
| 自动化 | cron/rrule 定时任务、SQL 条件提醒（零 LLM 扫描）、LLM 定时总结，调度器与会话隔离打通 |
| 手机伴侣端 | `apps/hm`（uni-app）：扫桌面端二维码配对，经自建 relay 中转，**不要求同一网络**；远程指挥桌面助手、审批与通话 |
| 内置技能 | 打包内嵌技能目录（`builtin-skills/`）+ 人格配置（`builtin-soul/`），启动期落盘到用户数据目录 |

后端以 **axum** 提供 `127.0.0.1:3000` 上的 HTTP/WebSocket API（前端与外部调用共用同一套接口），
Tauri 主进程把可执行名 `hermix` 作为 sidecar 拉起。

## 构建与验证

环境要求：

- Windows 10+ x64，Rust stable MSVC 工具链。MSVC linker（rust-lld 与 LIBPATH）已在
  `.cargo/config.toml` 写死，任何终端直接用 `cargo` 即可，无需额外环境脚本
- Node.js ≥ 20 与 npm（仅前端需要）

常用命令：

```bash
cargo check --workspace --all-targets    # 全量编译检查（应为 0 error / 0 warning）
cargo test --workspace --lib             # 全部单元测试（约 1,800 个）

cd apps/desktop-tauri
./node_modules/.bin/tsc --noEmit        # 前端类型检查（约 25s，见下方警告）
./node_modules/.bin/vitest run          # 前端测试

./scripts/ci-gate.sh                     # CI 总闸：编译 + 测试 + tsc + 笔记/卫生/侧车门禁
```

> **类型检查必须直接调 `node_modules/.bin/tsc`。** `pnpm tsc --noEmit` 是假信号——
> 它只跑 pnpm 自己的 install 流程就返回（约 0.8s、输出 `Already up to date`、exit 0），
> 类型检查压根没执行。

构建产物统一落在仓库根 `target/`（2026-10-09 起 src-tauri 已并入根 workspace，
`release/target`、`src-tauri/target` 已不再使用）。`target/`、`node_modules/`、
运行时数据目录均已 gitignore。

sidecar 刷新的唯一入口是 `npm run ensure-sidecar`（`npm run build` 与 `cargo tauri build`
的 `beforeBuildCommand` 都已挂载），它会编译 hermix-server / hermix-indexer 并做 sha256
校验；**禁止绕过它手工 copy**。打包前跑 `./scripts/check-sidecar-freshness.sh`（exit 1 即过期）。

## 仓库结构

```
├── Cargo.toml                 # cargo workspace（resolver = 2）
├── crates/                    # 21 个 Rust crate
│   ├── hermix-core            # 接缝定义（Fs/Shell/ExecWorld/Sandbox/Transport + ToolHandler）与 boot
│   ├── hermix-context         # 服务注册与生命周期（ServiceGuard Drop 即注销）
│   ├── hermix-session         # 会话 commit 协议（先落盘再并入内存日志）
│   ├── hermix-store           # 全仓唯一 SQLite 入口
│   ├── hermix-config          # 配置加载、provider 注册表唯一真理源、路径管理
│   ├── hermix-agent           # Agent 循环内核：run_loop / 上下文分层 / 子代理编排
│   ├── hermix-tools           # 30+ 工具后端 + 审批引擎 + 凭证防护
│   ├── hermix-server          # axum HTTP/WS API；产出二进制 `hermix`（sidecar 入口）
│   ├── hermix-desktop         # Tauri 主程序
│   ├── hermix-installer       # 自研安装器（替代 NSIS）
│   ├── hermix-indexer         # usn-indexer（独立 bin crate）
│   └── …                      # intelligence / focus / automation / mcp / gateway /
│                              #   voice / cua / task / transport / telemetry / skills /
│                              #   plugin / acp / runtime / environments / coreutils …
├── apps/
│   ├── desktop-tauri/         # Tauri v2 桌面壳 + React 19 / Tailwind 4 / Vite 前端
│   ├── hm/                    # 手机伴侣端（uni-app，HBuilderX 布局，见其 README）
│   ├── relay/                 # 自建中转服务 + 官网静态页 + 桌面端/spiker 更新源
│   ├── installer/             # 自研安装器前端（vite）+ 载荷嵌入工具
│   ├── hx-shell/              # Custom App Shell 运行时
│   ├── hx-packager/           # 自定义应用打包工具
│   └── playground/            # 前端实验场
├── sdk/                       # protocol-fixtures（协议黄金样本）、typescript 类型包
├── scripts/                   # ci-gate.sh、release.sh、各种卫生/门禁脚本
├── docs/notes/                # 决策笔记唯一入口（proposed / implemented / rejected）
├── AGENTS.md                  # 贡献者约定、六步决策阶梯、核心原则 —— 改码前必读
└── CHANGELOG.md               # 发布脚本取版本段作为更新说明
```

## 前端速览

`apps/desktop-tauri/src/` 下按域组织组件：聊天主流（含消息渲染与流式协议适配）、模型选择器
（以后端 `/api/model/list` 为持久权威、`/api/providers/meta` 做厂商分组）、设置中心、群聊、
自定义应用渲染。API 封装集中在 `src/api/rest.ts`，类型镜像在 `src/types.ts` 与后端 serde 结构一一对应。

## 发版与官网

桌面端走 `scripts/release.sh`：一次构建产出**安装包 exe**（载荷嵌入安装器）与**更新包 zip**
（+ sha256 + `manifest.json`），上传到自建 relay 的 `public/desktop/`，客户端默认从
`https://hermix.cssso.com/desktop/manifest.json` 取更新。手机端 APK 由 HBuilderX 云打包后人工
放到 `public/hermix.apk`。

出包全流程（全链 cargo 编译远超单命令超时，分段预热再组装）：

```bash
./scripts/check-sidecar-freshness.sh        # 门禁必须 exit 0；STALE 就先 npm run ensure-sidecar
npm run build                               # 前端 + ensure-sidecar（dev.mjs 全链路）
cargo build --release -p hermix-desktop     # 主程序
(cd apps/installer && npm run build:vite)   # 安装器前端
HERMIX_PRODUCT_VERSION=<ver> cargo build --release -p hermix-installer
./scripts/release.sh --skip-build --skip-upload   # 组装 app 根 → latest.zip + manifest + 嵌入安装包
```

待传文件打印在脚本尾部（`target/release/update-staging/` 与 `dist/`）。注意：

- `hx-shell` 源码有改动时先 `(cd apps/hx-shell && cargo build --release)` 并把
  `target/release/hx-shell.exe` 拷到 `apps/desktop-tauri/src-tauri/resources/loader/`；
- loader 目录三个预编译二进制（hx-shell/WebView2Loader/esbuild）互为新鲜度参照，
  单独 touch 其一会造成 mtime 环报 STALE——用 `touch -t` 把三者调成同一时间戳；
- 后台跑 release.sh 必须真正脱离会话（detached 子 shell 会随会话结束被杀，
  表现为日志冻结、进程归零），宁可前台分段跑完上述预热后 `--skip-build` 组装。

Agent BBS 部署（relay 机器，代码在 `apps/bbs/`，决策笔记
`docs/notes/implemented/feature/2026-10-09-agent-bbs-integration.md`）：

```bash
node apps/bbs/src/server.js                       # Node ≥22.13，只绑 127.0.0.1:3901
node apps/bbs/scripts/migrate-from-legacy.js      # 旧站数据迁移（幂等，可重复跑）
```

relay 侧 `server.js` 已含 `/bbs/*` 反代，更新重启后 `/api/bbs/status` 才会从
`bbs_down` 变为在线。

旧站数据导出：ByetHost 对非浏览器流量有 JS 挑战，Node 抓不到——用
`apps/bbs/scripts/export-legacy-bbs.browser.js` 在**浏览器控制台**里跑（打开
旧站 → F12 → 粘贴回车 → 自动下载 `export.json`），再喂给迁移脚本。

## 官网

官网是 `apps/relay/public/` 下的**多页静态站**，随 relay 一起部署，没有独立构建：

```
public/index.html      首页        desktop.html    Hermix Desktop
mobile.html            手机端      spiker.html     Spiker
changelog.html         更新日志    articles.html   文章列表
article.html?id=<slug> 文章正文    docs.html       使用文档
dev.html               开发者文档  assets/         site.css / site.js / articles.js / docs.js
```

两条硬规则：

- **版本号不写死**。页面运行时 fetch 同域的 `desktop/manifest.json`、`spiker/manifest.json`、
  `/api/app-info`，与客户端「检查更新」同源。取不到就显示「未发布」，不回填假版本号。
- **静态托管必须挂在所有显式路由之后**（`server.js` 末尾）。`/desktop/manifest.json` 与
  `/spiker/manifest.json` 要额外带 `X-Checksum-Sha256` 与 `no-cache`，被 static 抢先命中就
  退化成没有校验头的裸 JSON，客户端更新链路会静默降级成「永远无更新」。

文章内容在 `assets/articles.js`，文档在 `assets/docs.js`（用户视角 / 开发者视角两套，不混写）。
新增文章往数组尾部加即可，列表页按 client 自动分组。

## 贡献约定（摘要）

- 改动必须附带验证证据：`cargo check` + 相关 crate 测试 + `node_modules/.bin/tsc`，提交前跑 `scripts/ci-gate.sh`
- 单文件超过 2500 行会被 CI 门警告；新增依赖遵循"标准库 > 平台原生 > 已有依赖"阶梯
- 影响代码库的持久决策写进 `docs/notes/` 决策笔记（三态 + 强制三节），不再新建顶层 `docs/XXX_PLAN.md`
- 错误要精准暴露，禁止静默吞结果；跨入口不变量放单一分发点强制，不做"每个入口各自记得检查"
- 详见 [AGENTS.md](AGENTS.md)
