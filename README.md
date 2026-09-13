# Hermix Desktop

本地优先的 Windows 桌面 AI 助手。Rust 后端 + React 前端，经 Tauri v2 打包为免安装便携应用：
对话、工具执行、定时自动化、自定义应用（custom app）
都在本机闭环，数据不依赖任何云端托管。

当前为 **Windows x64（MSVC）单平台**项目，无跨平台目标，也没有云服务依赖。

---

## 核心能力

| 能力 | 说明 |
|---|---|
| Agent 对话 | 多 provider LLM 接入（内置十余家厂商注册表 + 自定义 OpenAI 兼容接入），工具调用、子代理派发、上下文压缩、审批引擎 |
| 自定义应用 | 用户以自然语言或模板生成真代码应用：独立窗口、真分发、可复用技能，是唯一的"用户自造能力"入口 |
| 自动化 | cron/rrule 定时任务、SQL 条件提醒（零 LLM 扫描）、LLM 定时总结，调度器与会话隔离打通 |
| 内置技能 | 打包内嵌技能目录（`builtin-skills/`）+ 人格配置（`builtin-soul/`），启动期落盘到用户数据目录 |

后端以 **axum** 提供 `127.0.0.1:3000` 上的 HTTP/WebSocket API（前端与外部调用共用同一套接口），
Tauri 主进程把可执行名 `hermix` 作为 sidecar 拉起。

## 构建与验证

环境要求：

- Windows 10+ x64，Rust stable MSVC 工具链。MSVC linker（rust-lld 与 LIBPATH）已在
  `.cargo/config.toml` 写死，任何终端直接用 `cargo` 即可，无需额外环境脚本
- Node.js ≥ 20 与 pnpm（仅前端需要）

常用命令：

```bash
cargo check --workspace --all-targets    # 全量编译检查（应为 0 error / 0 warning）

cargo test --workspace --lib             # 全部单元测试（约 1,054 个）

cd apps/desktop-tauri && pnpm install    # 首次安装前端依赖
pnpm tsc --noEmit                        # 前端类型检查（EXIT=0 为过）
pnpm vitest run                          # 前端测试（15 个文件 / 131 个用例）

./scripts/ci-gate.sh                     # CI 总闸：编译 + 测试 + tsc + 行数红线扫描
```

构建产物统一落在仓库外层 `release/target/`；`target/`、`node_modules/`、运行时数据目录
（`hermix-data/` 等）均已 gitignore。打正式包走 Tauri NSIS 流程产出便携 zip。

## 仓库结构

```
├── Cargo.toml                 # cargo workspace（resolver = 2）
├── crates/
│   ├── hermix-core            # 基础类型：Message / LlmResponse / ToolError …
│   ├── hermix-config          # 配置加载、provider 注册表唯一真理源、路径管理
│   ├── hermix-agent           # Agent 循环内核：run_loop / 会话生命周期 / 重试 /
│   │                          #   上下文分层 / 空响应处理 / 子代理编排
│   ├── hermix-tools           # 30+ 工具后端 + 审批引擎 + 凭证防护
│   ├── hermix-intelligence    # 工具路由、思考模式
│   ├── hermix-focus           # FocusStack / 记忆显著性
│   ├── hermix-automation      # 调度器（cron/rrule 任务与触发器）
│   ├── hermix-mcp             # MCP 客户端接入
│   ├── hermix-gateway         # 供应商网关适配
│   ├── hermix-server          # axum HTTP/WS API + Dashboard 处理器；
│   │                          #   产出二进制 `hermix`（Tauri sidecar 入口）
│   └── …                      # 其余领域 crate：voice / cua / task / transport /
│                              #   telemetry / skills / plugin / acp / runtime /
│                              #   environments / coreutils / shell-parser / cron
├── apps/
│   ├── desktop-tauri/         # Tauri v2 桌面壳 + React 19 / Tailwind 4 / Vite 前端
│   │                          #   （前端源码 src/，Rust 壳在 src-tauri/，独立 sub-workspace）
│   ├── hx-shell/              # Custom App Shell 运行时
│   ├── hx-packager/           # 自定义应用打包工具
│   └── playground/            # 前端实验场
├── sdk/                       # protocol-fixtures（协议黄金样本）、typescript 类型包
├── scripts/                   # ci-gate.sh、图标生成、主题 token 检查等工程脚本
├── docs/                      # 设计方案与历史审查报告
├── AGENTS.md                  # 贡献者约定、代码六步决策阶梯、核心原则 —— 改码前必读
└── SOUL.md                    # 助手人格定义（同步打包进 builtin-soul/）
```

## 前端速览

`apps/desktop-tauri/src/` 下按域组织组件：聊天主流（含消息渲染与流式协议适配）、模型选择器
（以后端 `/api/model/list` 为持久权威、`/api/providers/meta` 做厂商分组）、设置中心、群聊、
自定义应用渲染。API 封装集中在 `src/api/rest.ts`，类型镜像在 `src/types.ts` 与后端 serde 结构一一对应。

## 贡献约定（摘要）

- 改动必须附带验证证据：`cargo check` + 相关 crate 测试 + `pnpm tsc`，提交前跑 `scripts/ci-gate.sh`
- 单文件超过 2500 行会被 CI 门警告；新增依赖遵循"标准库 > 平台原生 > 已有依赖"阶梯
- 错误要精准暴露，禁止静默吞结果；跨入口不变量放单一分发点强制，不做"每个入口各自记得检查"
- 详见 [AGENTS.md](AGENTS.md)

## 文档

- 架构演进与专项计划存档：`docs/`
