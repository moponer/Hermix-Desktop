# Hermix 后端架构现状基线 (v6)

> **生成时间**: 2026-06-19
> **数据来源**: `find crates apps -name "*.rs" -not -path "*/target/*" | xargs wc -l`(实测)
> **数据口径**: 排除 `target/`、`node_modules/`、`dist/` 等构建产物
> **覆盖范围**: 21 workspace member crates + 1 desktop-tauri app

---

## 0. 一句话总结

Hermix 是一个 **21 crate Rust workspace + Tauri v2 桌面应用**,**实测 179,624 行 Rust / 427 文件**。最大单文件 `hermix-agent/src/agent_loop.rs` 9,178 行。核心引擎(`agent_loop`+ `tools` + `gateway` + `intelligence` + `agent`)合计 **131,917 行**,占全后端 **73%**。

---

## 1. 全局统计(实测)

| 维度 | 数值 |
|------|------|
| workspace members | **21** crates |
| apps | **1** (`apps/desktop-tauri`) |
| crates 总行数 | **177,816** |
| crates 文件数 | **417** |
| apps 总行数 | **1,808** |
| apps 文件数 | **10** |
| **全后端总行数** | **179,624** |
| **全后端总文件数** | **427** |
| 最大单文件 | `hermix-agent/src/agent_loop.rs` **9,178 行** |

> 注:`find crates apps -name "*.rs"` 不排除 target 得到 244,098 行 / 455 文件,差值 64,474 行来自 `apps/desktop-tauri/src-tauri/target/build/web_atoms-*/named_entities.rs` (11,844 行,4 份相同 codegen 文件) 及若干 `serde/thiserror/serde_core` 自动生成的 `private.rs`。

---

## 2. 21 Crate 逐个实测

### 2.0 Crate 全清单(按代码量降序)

| 排名 | crate | 文件 | 行数 | 单文件最大 |
|----:|-------|----:|----:|----------|
| 1 | **hermix-agent** | 55 | **36,522** | agent_loop.rs **9,178** |
| 2 | **hermix-tools** | 90 | **35,475** | tools/url_safety.rs 2,024 |
| 3 | **hermix-gateway** | 60 | **29,531** | gateway.rs 2,902 |
| 4 | hermix-cli | 33 | 14,517 | commands.rs 4,425 |
| 5 | hermix-intelligence | 38 | 14,389 | prefetch.rs 1,134 |
| 6 | hermix-server | 29 | 12,566 | lib.rs 3,351 |
| 7 | hermix-config | 18 | 6,558 | paths.rs 1,073 |
| 8 | hermix-core | 14 | 4,487 | tool_call_sanitizer.rs 1,013 |
| 9 | hermix-focus | 9 | 4,438 | embedded.rs 1,213 |
| 10 | hermix-mcp | 6 | 3,458 | client.rs 1,100 |
| 11 | hermix-environments | 12 | 2,716 | local.rs 425 |
| 12 | hermix-cron | 9 | 2,604 | scheduler.rs 643 |
| 13 | hermix-acp | 7 | 2,279 | handler.rs 674 |
| 14 | hermix-skills | 7 | 2,106 | store.rs 486 |
| 15 | hermix-eval | 13 | 2,023 | trajectory_compressor.rs 404 |
| 16 | hermix-runtime | 3 | 1,653 | lib.rs 1,099 |
| 17 | hermix-transport | 1 | 732 | lib.rs 732 |
| 18 | hermix-voice | 1 | 500 | lib.rs 500 |
| 19 | hermix-bus | 7 | 491 | remote_agent_service.rs 181 |
| 20 | hermix-parity-tests | 3 | 473 | harness.rs 451 |
| 21 | hermix-telemetry | 2 | 298 | lib.rs 253 |
| **合计** | | **417** | **177,816** | |

### 2.1 hermix-agent(36,522 行 / 55 文件)— 核心引擎

```
9178  agent_loop.rs              ← 项目最大单文件,主对话循环
2509  local_agent_service.rs     ← 本地 agent 服务
2203  provider.rs                ← LLM Provider 抽象
2146  compression.rs             ← 上下文压缩
1621  session_persistence.rs     ← 会话持久化(BEGIN IMMEDIATE)
1279  plugins.rs                 ← 插件加载
1065  memory_plugins/hindsight.rs
 906  memory_plugins/holographic.rs
 826  memory_manager.rs          ← 记忆管理
 722  python_alignment.rs        ← Python 参考实现对齐
 643  memory_plugins/openviking.rs
 630  memory_plugins/retaindb.rs
 629  sub_agent_orchestrator.rs  ← Sub-Agent 编排
 621  agent_builder.rs
 579  api_bridge.rs
 552  memory_plugins/honcho.rs
 543  consciousness.rs           ← 意识/自我模型
 530  memory_plugins/byterover.rs
 501  skill_orchestrator.rs
 469  memory_plugins/supermemory.rs
 436  providers_extra.rs
 432  memory_plugins/mem0.rs
 419  smart_model_routing.rs
 388  context_references.rs
 387  subdirectory_hints.rs
 380  pii_redact.rs
 378  context_files.rs
 342  environment.rs
 339  credential_pool.rs
 333  copilot_acp.rs             ← Copilot ACP 集成
 287  hardline.rs
 286  auxiliary_builder.rs
 285  rate_limit.rs
 262  sub_agent_safety.rs
 241  reasoning.rs
 220  honcho_provider.rs
 181  interrupt.rs
 146  lib.rs
 127  memory_layer_adapter.rs
 102  fallback.rs
 102  budget.rs
  94  oauth.rs
  86  steer.rs
  64  memory_plugins/mod.rs
```

**memory_plugins 9 文件 / 5,291 行**(全部真实实现,不是 stub):

| plugin | 行数 |
|--------|----:|
| hindsight | 1,065 |
| holographic | 906 |
| openviking | 643 |
| retaindb | 630 |
| honcho | 552 |
| byterover | 530 |
| supermemory | 469 |
| mem0 | 432 |
| mod | 64 |

### 2.2 hermix-tools(35,475 行 / 90 文件)— 工具系统

**目录结构**:

```
hermix-tools/src/
├── tools/        ← 45 文件 / 19,285 行(用户可见的工具)
├── backends/     ← 20 文件 /  7,434 行(底层后端)
├── tts_streaming/←  6 文件 /  2,052 行(TTS 流式)
└── (根)          ← 19 文件 /  6,704 行
```

**核心单文件**:

| 行数 | 文件 | 作用 |
|----:|------|------|
| 2,024 | `tools/url_safety.rs` | URL 安全检查 |
| 1,407 | `backends/browser.rs` | 浏览器后端 |
| 1,378 | `register_builtins.rs` | 内置工具注册 |
| 1,246 | `approval.rs` | 工具审批 |
| 1,011 | `tools/voice_mode.rs` | 语音模式 |
| 1,005 | `tools/process_registry.rs` | 进程注册表 |
| 981 | `tools/messaging.rs` | 消息工具 |
| 925 | `backends/memory.rs` | 记忆后端 |
| 837 | `tools/computer_use.rs` | 计算机使用 |
| 834 | `tools/mixture_of_agents.rs` | Agent 混合 |
| 784 | `registry.rs` | 工具注册表 |
| 743 | `tools/browser_supervisor.rs` | 浏览器监控 |
| 729 | `tools/browser.rs` | 浏览器 |
| 683 | `backends/session_search.rs` | 会话搜索 |

**tools/ 45 文件清单**(从大到小):

url_safety 2024 / voice_mode 1011 / process_registry 1005 / messaging 981 / computer_use 837 / mixture_of_agents 834 / browser_supervisor 743 / browser 729 / terminal 650 / cdp_supervisor 626 / disk_cleanup 558 / file 537 / media_workflow 509 / skill_utils 502 / code_execution 478 / kanban 434 / cdp_bootstrap 419 / hermix_achievements 412 / pdf 373 / skills 367 / office_cli 364 / web 344 / transcription 305 / homeassistant 296 / aliases 288 / skill_commands 283 / managed_tool_gateway 280 / video_gen 270 / audio_gen 263 / cronjob 257 / model_providers 223 / security_guidance 220 / memory 217 / todo 200 / platforms 193 / delegation 178 / gpt_image 170 / session_search 132 / image_gen 129 / tts 122 / clarify 115 / vision 107 / tts_premium 91 / tool_result_storage 61 / mod 52 / env_passthrough 38 / osv_check 29 / credential_files 29

**backends/ 20 文件清单**:

browser 1407 / memory 925 / session_search 683 / file 631 / tts 373 / image_gen 360 / web 358 / clarify 346 / audio_gen 345 / video_gen 333 / messaging 302 / gpt_image 273 / threat_scan 243 / code_execution 176 / homeassistant 168 / todo 151 / delegation 134 / vision 112 / cronjob 89 / mod 25

**tts_streaming/ 6 文件**:

pipeline 627 / sanitizer 421 / minimax 373 / sentence_buffer 317 / elevenlabs_stream 279 / mod 35

### 2.3 hermix-gateway(29,531 行 / 60 文件)— IM 网关

**目录结构**:

```
hermix-gateway/src/
├── platforms/    ← 21 文件 / 15,367 行(20 平台适配器 + mod)
├── groupchat/    ← 10 文件 /  1,685 行(群聊子系统)
└── (根)          ← 29 文件 / 12,479 行
```

**platforms/ 20 适配器(行数从大到小)**:

| # | 平台 | 行数 | 类型 |
|--:|------|----:|------|
| 1 | **discord** | 2,146 | 长连接 WS Gateway |
| 2 | **telegram** | 1,850 | Bot API 长轮询 |
| 3 | **slack** | 1,607 | Socket Mode + Events API |
| 4 | **feishu(飞书)** | 1,552 | WebSocket + 事件订阅 |
| 5 | **weixin(微信)** | 1,522 | iLink 协议 + 回调 |
| 6 | **matrix** | 1,450 | Matrix 协议 |
| 7 | **dingtalk(钉钉)** | 714 | Stream 模式 |
| 8 | **wecom_callback(企业微信回调)** | 656 | HTTP 回调 |
| 9 | **email** | 552 | SMTP/IMAP |
| 10 | **api_server** | 511 | HTTP API 暴露 |
| 11 | **whatsapp** | 479 | Cloud API |
| 12 | **wecom(企业微信)** | 333 | 主动推送 |
| 13 | **webhook** | 331 | 通用 webhook |
| 14 | **mattermost** | 325 | Bot 账号 |
| 15 | **signal** | 271 | signal-cli |
| 16 | **qqbot** | 213 | QQ 机器人 |
| 17 | **homeassistant** | 201 | HA 集成 |
| 18 | **sms** | 180 | 短信网关 |
| 19 | **bluebubbles** | 172 | iMessage 桥 |
| 20 | **helpers** | 239 | 公共辅助 |

**groupchat/ 10 文件**(主人问的"群聊"):

| 行数 | 文件 | 作用 |
|----:|------|------|
| 421 | persistence.rs | 群聊会话持久化 |
| 260 | manager.rs | 群聊生命周期 |
| 211 | prompt.rs | 群聊 prompt 构建 |
| 156 | rank.rs | 参与者排名/发言顺序 |
| 154 | rules.rs | 群聊规则(谁响应、何时响应) |
| 151 | in_memory.rs | 内存状态 |
| 151 | idle.rs | 闲置检测 |
| 127 | leader.rs | Leader 选举 |
| 30 | dispatcher.rs | 派发器入口 |
| 24 | mod.rs | |

**根目录 29 文件**:

gateway 2902 / hooks 1111 / stream 922 / commands 777 / platform_registry 740 / session 694 / channel_directory 561 / ssrf 347 / markdown_split 400 / media 322 / delivery 322 / mirror 270 / hook_payloads 303 / adapter 166 / sticker_cache 160 / tool_backends 214 / dm 212 / background 203 / pairing 47 / platform_requirements 463 / voice 494 / format 9 / lib 148 / gateway_requirements 78

### 2.4 hermix-cli(14,517 行 / 33 文件)— 命令行

| 行数 | 文件 | 作用 |
|----:|------|------|
| 4,425 | commands.rs | 所有 CLI 子命令 |
| 3,655 | main.rs | CLI 入口 |
| 1,154 | tui.rs | TUI 界面 |
| 679 | lumio.rs | Lumio 客户端 |
| 673 | live_messaging.rs | 实时消息 |
| 592 | cli.rs | CLI 框架 |
| 492 | theme.rs | 主题 |
| 479 | app.rs | App 状态 |
| 476 | claw_migrate.rs | Claw 迁移工具 |
| 281 | checklist.rs | 启动检查 |
| 272 | doctor.rs | 诊断 |
| 268 | pairing_store.rs | 配对存储 |
| 185 | copilot_auth.rs | Copilot 认证 |
| 109 | oauth_store.rs | OAuth 存储 |

### 2.5 hermix-intelligence(14,389 行 / 38 文件)— LLM 路由与智能

**目录结构**:

```
hermix-intelligence/src/
├── auxiliary/        ← 7 文件 / 1,746 行(辅助 LLM)
├── session_insights/  ← 5 文件 / 1,823 行(会话洞察)
├── models_dev/       ← 6 文件 / 1,500 行(models.dev 模型元数据)
└── (根)              ← 20 文件 / 9,320 行
```

**核心单文件**:

| 行数 | 文件 | 作用 |
|----:|------|------|
| 1,134 | prefetch.rs | 上下文预取 |
| 1,096 | tool_router.rs | Tool 路由(KeywordRouter/VectorRouter/CompositeRouter/AlwaysInclude) |
| 951 | anthropic_adapter.rs | Anthropic Claude 适配 |
| 700 | auxiliary/client.rs | 辅助 LLM 客户端 |
| 683 | models_dev/client.rs | models.dev 客户端 |
| 664 | usage_pricing.rs | 用量计费 |
| 642 | display.rs | 显示 |
| 601 | model_metadata.rs | 模型元数据 |
| 539 | embedding.rs | Embedding |
| 535 | credential_pool.rs | 凭证池 |
| 531 | session_insights/engine.rs | 洞察引擎 |
| 510 | thinking.rs | 思考模式 |
| 505 | session_insights/mod.rs | |
| 404 | session_insights/format.rs | |
| 397 | context_engine.rs | 上下文引擎 |
| 384 | router.rs | LLM 路由 |
| 335 | models_dev/parse.rs | |
| 323 | prompt.rs | Prompt 构建 |
| 310 | redact.rs | PII 脱敏 |
| 279 | auxiliary/config.rs | |
| 273 | error_classifier.rs | 错误分类 |
| 266 | usage.rs | 用量 |
| 243 | auxiliary/candidate.rs | |
| 239 | title.rs | 会话标题 |
| 212 | session_insights/source.rs | |
| 190 | models_dev/types.rs | |
| 172 | insights.rs | 洞察 |
| 171 | session_insights/types.rs | |
| 169 | auxiliary/task.rs | |
| 150 | auxiliary/builtins.rs | |
| 148 | auxiliary/error.rs | |
| 138 | models_dev/cache.rs | |
| 103 | models_dev/mapping.rs | |
| 79 | lib.rs | |
| 57 | auxiliary/mod.rs | |
| 51 | models_dev/mod.rs | |

### 2.6 hermix-server(12,566 行 / 29 文件)— HTTP 服务层

**根目录 4 文件**:

| 行数 | 文件 |
|----:|------|
| 3,351 | lib.rs |
| 343 | security.rs |
| 135 | test_state.rs |
| 21 | main.rs |

**dashboard/ 22 文件 / 7,223 行**:

| 行数 | 文件 | 作用 |
|----:|------|------|
| 729 | stubs.rs | 测试桩 |
| 707 | automation.rs | 自动化 |
| 645 | skills.rs | 技能管理 API |
| 583 | migration.rs | 迁移 API |
| 493 | persona.rs | 人设 API |
| 434 | groupchat_api.rs | 群聊 API |
| 418 | sessions.rs | 会话 API |
| 410 | custom_providers.rs | 自定义 Provider |
| 379 | groupchat_sqlite.rs | 群聊 SQLite |
| 343 | security.rs(同 root) | |
| 342 | types.rs | Dashboard 类型 |
| 318 | consciousness.rs | 意识 API |
| 316 | env.rs | 环境 API |
| 293 | mcp.rs | MCP API |
| 272 | mod.rs | |
| 240 | config.rs | 配置 API |
| 129 | cron.rs | Cron API |
| 116 | transcribe.rs | 转写 API |
| 105 | plugins_inspect.rs | 插件检查 |
| 94 | status.rs | 状态 |
| 93 | logs.rs | 日志 |
| 59 | groupchat_dispatcher.rs | 群聊派发 |
| 48 | analytics.rs | 分析 |

### 2.7 hermix-config(6,558 行 / 18 文件)— 配置

**根目录 13 文件**:

| 行数 | 文件 |
|----:|------|
| 1,073 | paths.rs |
| 1,068 | loader.rs |
| 708 | migration.rs |
| 539 | config.rs |
| 356 | python_yaml_compat.rs |
| 294 | roundtrip_tests.rs |
| 227 | session.rs |
| 217 | platform.rs |
| 158 | python_platform_env.rs |
| 153 | merge.rs |
| 107 | streaming.rs |
| 63 | lib.rs |
| 57 | managed_gateway/mod.rs |

**managed_gateway/ 5 文件 / 1,400 行**:

| 行数 | 文件 |
|----:|------|
| 388 | selection.rs |
| 385 | auth.rs |
| 283 | resolver.rs |
| 281 | config.rs |
| 63 | mod.rs |

### 2.8 hermix-core(4,487 行 / 14 文件)— 核心工具库

| 行数 | 文件 | 作用 |
|----:|------|------|
| 1,013 | tool_call_sanitizer.rs | Tool Call 清洗(防注入) |
| 1,001 | tool_call_parser.rs | Tool Call 解析 |
| 533 | types.rs | 全局类型 |
| 498 | traits.rs | 全局 trait |
| 336 | acui.rs | ACUI(Agent Computer Use Interface) |
| 235 | path_security.rs | 路径安全 |
| 192 | test_generators.rs | 测试生成器 |
| 190 | errors.rs | 错误类型 |
| 108 | tool_schema.rs | Tool Schema |
| 102 | console_decode.rs | 控制台解码 |
| 59 | lib.rs | |

### 2.9 hermix-focus(4,438 行 / 9 文件)— 焦点/上下文

| 行数 | 文件 | 作用 |
|----:|------|------|
| 1,213 | embedded.rs | 内嵌记忆后端 |
| 699 | focus_stack.rs | 5 状态 Focus 栈 |
| 539 | time_aware_recall.rs | 时序感知召回 |
| 516 | memory_router.rs | 记忆路由 |
| 405 | time_recall.rs | 时序召回 |
| 402 | memory_layer.rs | MemoryLayer trait(8 方法) |
| 364 | session_frame.rs | 会话帧 |
| 223 | salience.rs | 显著度 |
| 77 | lib.rs | |

### 2.10 hermix-mcp(3,458 行 / 6 文件)— MCP 协议

| 行数 | 文件 |
|----:|------|
| 1,100 | client.rs |
| 758 | transport.rs |
| 726 | serve.rs |
| 432 | server.rs |
| 339 | auth.rs |
| 103 | lib.rs |

### 2.11 hermix-environments(2,716 行 / 12 文件)— 环境/沙箱

| 行数 | 文件 |
|----:|------|
| 425 | local.rs |
| 382 | training/parsers.rs |
| 331 | ssh.rs |
| 308 | training/benchmarks/swe_bench.rs |
| 293 | training/mod.rs |
| 245 | training/benchmarks/terminal_bench.rs |
| 222 | training/benchmarks/yc_bench.rs |
| 169 | file_sync.rs |
| 127 | manager.rs |
| 33 | lib.rs |
| 9 | training/benchmarks.rs |

### 2.12 hermix-cron(2,604 行 / 9 文件)— 定时任务

| 行数 | 文件 |
|----:|------|
| 643 | scheduler.rs |
| 433 | runner.rs |
| 425 | persistence.rs |
| 397 | job.rs |
| 322 | file_lock.rs |
| 192 | backend.rs |
| 86 | cli_support.rs |
| 73 | completion.rs |
| 33 | lib.rs |

### 2.13 hermix-acp(2,279 行 / 7 文件)— ACP 协议

| 行数 | 文件 |
|----:|------|
| 674 | handler.rs |
| 456 | protocol.rs |
| 333 | events.rs |
| 309 | session.rs |
| 216 | permissions.rs |
| 189 | server.rs |
| 102 | lib.rs |

### 2.14 hermix-skills(2,106 行 / 7 文件)— 技能系统

| 行数 | 文件 |
|----:|------|
| 486 | store.rs |
| 447 | hub.rs |
| 397 | skill.rs |
| 344 | guard.rs |
| 222 | version.rs |
| 190 | expert.rs |
| 20 | lib.rs |

### 2.15 hermix-eval(2,023 行 / 13 文件)— 评测

| 行数 | 文件 |
|----:|------|
| 404 | trajectory_compressor.rs |
| 367 | runner.rs |
| 333 | mini_swe_runner.rs |
| 293 | batch_runner.rs |
| 142 | tblite.rs |
| 84 | agent_rollout.rs |
| 71 | result.rs |
| 66 | reporter.rs |
| 66 | adapter.rs |
| 60 | lib.rs |
| 57 | verifier.rs |
| 48 | bin/hermix-bench-smoke.rs |
| 32 | error.rs |

### 2.16 hermix-runtime(1,653 行 / 3 文件)— 运行时

| 行数 | 文件 |
|----:|------|
| 1,099 | lib.rs(TickLoop + Watchdog) |
| 367 | cache.rs |
| 187 | watchdog.rs |

### 2.17 hermix-transport(732 行 / 1 文件)— 传输协议

| 行数 | 文件 | 作用 |
|----:|------|------|
| 732 | lib.rs | WS ProtocolMessage 定义、Role 枚举、4 态 approval 协议 |

### 2.18 hermix-voice(500 行 / 1 文件)— 语音

| 行数 | 文件 | 作用 |
|----:|------|------|
| 500 | lib.rs | HTTP STT/TTS 桥接 |

### 2.19 hermix-bus(491 行 / 7 文件)— 进程内总线

| 行数 | 文件 | 作用 |
|----:|------|------|
| 181 | remote_agent_service.rs | 远程 agent 服务 |
| 110 | messages.rs | BusMessage 定义(9 变体) |
| 42 | in_process.rs | 进程内传输 |
| 20 | transport.rs | BusTransport trait |
| 13 | lib.rs | |

> ⚠️ 重要事实:**BusMessage 只有 9 个变体,不存在"26 通道"分类**。BusTransport trait 已定义但目前只有 InProcess 一个实现,**没有 WS / HTTP 跨进程实现**。

### 2.20 hermix-parity-tests(473 行 / 3 文件)— Python 对齐测试

| 行数 | 文件 |
|----:|------|
| 451 | harness.rs |
| 17 | lib.rs |
| 5 | recorder.rs |

### 2.21 hermix-telemetry(298 行 / 2 文件)— 遥测

| 行数 | 文件 |
|----:|------|
| 253 | lib.rs |
| 45 | otlp.rs |

---

## 3. apps/desktop-tauri(1,808 行 / 10 文件)— 桌面入口

```
611  src/sidecar.rs          ← Sidecar 进程管理
551  src/commands.rs         ← Tauri commands
321  src/portable_env.rs     ← 便携环境
 98  src/lib.rs
 76  tests/sidecar_crash_probe.rs
 69  tests/sidecar_orphan_pid_recycle.rs
 47  src/officecli.rs        ← OfficeCLI 桥接
 26  examples/fake_crash_sidecar.rs
  6  src/main.rs
  3  build.rs
```

**前端结构**(apps/desktop-tauri/):

```
dist/ docs/ hodgepodge/ index.html node_modules/
public/ scripts/ src/ src-tauri/
package.json pnpm-lock.yaml tsconfig.json vite.config.ts
```

---

## 4. Cargo.toml Workspace 关键事实(实测)

**Workspace 顶部**(`/f/spiker/hemix-desktop/Cargo.toml`):

```toml
[workspace]
resolver = "2"
# Note: workspace.name is intentionally omitted. The product is
# **Hermix**; the legacy "hermes-" crate names were a port from the
# Python reference implementation and have been renamed in lockstep
# (Phase 1 of the hermix migration). Crate-level package.name is set
# explicitly per member.

members = [21 个 crate,按文件列出]
```

**关键事实**:

| 字段 | 值 |
|------|-----|
| 产品名 | **Hermix**(workspace.name 故意省略) |
| 版本 | `0.1.1` |
| edition | `2021` |
| license | `MIT` |
| repository | `https://github.com/nousresearch/hermes-engine`(Python 仓库) |
| Lints | `unused_variables/imports/dead_code = allow`(全局宽松) |

**重要事实**:
- Cargo.toml 注释明确说:"the legacy `hermes-` crate names were a port from the Python reference implementation and have been renamed in lockstep (Phase 1 of the hermix migration)"
- 即代码注释里仍残留 "hermes" 命名属于已知历史包袱
- `nousresearch/hermes-engine` 是 Python 仓库,说明 Rust 端是 Python 实现的 Rust 重写

---

## 5. 关键事实订正(对 v3/v5 错误)

| v3/v5 错误描述 | 实测结果 |
|---------------|---------|
| "22 个 crate" | **21 个**(实测 `ls crates/`) |
| "8 memory plugin 中 1 真 7 stub" | **9 文件全部真实实现,共 5,291 行** |
| "26 通道已实现" | **不存在**,BusMessage 只有 9 变体 |
| "BusTransport WS 实现" | **不存在**,只有 InProcess 实现 |
| "179,624 行(含 target)" | 排除 target 是 179,624,含 target 是 244,098(差值是 codegen) |
| "427 文件" | ✅ 正确(排除 target) |

---

## 6. 大单文件清单(>500 行的 50 个文件)

按行数降序的前 50 个文件(覆盖全项目 70%+ 代码量):

```
9178  hermix-agent/src/agent_loop.rs
4425  hermix-cli/src/commands.rs
3655  hermix-cli/src/main.rs
3351  hermix-server/src/lib.rs
2902  hermix-gateway/src/gateway.rs
2509  hermix-agent/src/local_agent_service.rs
2203  hermix-agent/src/provider.rs
2146  hermix-gateway/src/platforms/discord.rs
2146  hermix-agent/src/compression.rs
2024  hermix-tools/src/tools/url_safety.rs
1850  hermix-gateway/src/platforms/telegram.rs
1621  hermix-agent/src/session_persistence.rs
1607  hermix-gateway/src/platforms/slack.rs
1552  hermix-gateway/src/platforms/feishu.rs
1522  hermix-gateway/src/platforms/weixin.rs
1450  hermix-gateway/src/platforms/matrix.rs
1407  hermix-tools/src/backends/browser.rs
1378  hermix-tools/src/register_builtins.rs
1279  hermix-agent/src/plugins.rs
1246  hermix-tools/src/approval.rs
1213  hermix-focus/src/embedded.rs
1154  hermix-cli/src/tui.rs
1134  hermix-intelligence/src/prefetch.rs
1111  hermix-gateway/src/hooks.rs
1100  hermix-mcp/src/client.rs
1099  hermix-runtime/src/lib.rs
1096  hermix-intelligence/src/tool_router.rs
1073  hermix-config/src/paths.rs
1068  hermix-config/src/loader.rs
1065  hermix-agent/src/memory_plugins/hindsight.rs
1017  hermix-agent/src/context.rs
1013  hermix-core/src/tool_call_sanitizer.rs
1011  hermix-tools/src/tools/voice_mode.rs
1005  hermix-tools/src/tools/process_registry.rs
1001  hermix-core/src/tool_call_parser.rs
 981  hermix-tools/src/tools/messaging.rs
 951  hermix-intelligence/src/anthropic_adapter.rs
 925  hermix-tools/src/backends/memory.rs
 922  hermix-gateway/src/stream.rs
 906  hermix-agent/src/memory_plugins/holographic.rs
 837  hermix-tools/src/tools/computer_use.rs
 834  hermix-tools/src/tools/mixture_of_agents.rs
 826  hermix-agent/src/memory_manager.rs
 784  hermix-tools/src/registry.rs
 777  hermix-gateway/src/commands.rs
 758  hermix-mcp/src/transport.rs
 743  hermix-tools/src/tools/browser_supervisor.rs
 740  hermix-gateway/src/platform_registry.rs
 729  hermix-tools/src/tools/browser.rs
 729  hermix-server/src/dashboard/stubs.rs
```

---

## 7. 文档版本

- v1: 2026-06-19 早(估算法,多处事实错误)
- v2: 2026-06-19 上午(纠正 5 处事实错误)
- v2.1: 2026-06-19 中午(纠正 3 处)
- v3: 2026-06-19 下午(50 大类功能地图)
- v5: 2026-06-19(优化清单 14 条)
- **v6: 2026-06-19 12:30(本版,全量实测 wc -l 重写)**
