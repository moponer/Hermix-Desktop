# 执行日志 — 2026-10-06 夜间自主推进（对接用）

> 本文件是**会话工作日志**，不是决策笔记（决策在 docs/notes/）。全部工作完成后可删。
> 产品前提：多用户、多客户端 + 手机端（已拍板，见 docs/notes/proposed/feature/2026-10-06-relay-hosted-multi-tenant.md）。

## 背景快照（对接必读）

- 两线计划已定稿并提交：A 线审计修复 = `docs/notes/proposed/process/2026-10-05-audit-remediation-plan.md`（含 2026-10-06 评审修订节）；B 线手机端产品化 = `docs/notes/proposed/feature/2026-10-06-relay-hosted-multi-tenant.md`（三项拍板已结：E2EE 门槛=是 / 商业模型暂不做→开放注册 / relay 要做、域名值后定）。
- B 线实现属功能分支 `feat/auto-20261004-1c1479c0`（worktree `.worktrees/feat-auto-20261004-1c1479c0`，并行会话地盘）——**本日志的夜间执行不碰它**，B 线开工需把笔记路径发给那个会话。
- 宝塔部署（用户自用 relay dogfood）：`https://域名/health` 已通、证书已签发；**剩余手动步骤在用户侧**：① 提取证书指纹填 `HERMIX_RELAY_CERT_SHA256` 环境变量并重启（面板终端一行：`openssl x509 -in /www/server/panel/vhost/cert/<域名>/fullchain.pem -noout -fingerprint -sha256`，冒号去掉转小写）；② 面板终端签发 token：`node src/relayctl.js add-node my-pc` / `add-phone my-pc`。端口用 7777（仅本机，防火墙不放行）。
- 并行会话状态（00:30 检查）：main 工作区有其未提交改动（tauri.conf.json / Cargo.toml / Cargo.lock / license.rtf / design-preview/）——与 A 线目标文件零重叠，A 线逐文件精确 stage 不受影响。

## 任务队列与状态

| # | 任务 | 状态 |
|---|---|---|
| 1 | A 线批次 1：exec_command 显式 Deny + browser 会话键 + pairing TOCTOU（各带测试） | ✅ 1aa398bd |
| 2 | A 线批次 4：注释/文档同步 | ✅ f3912c4d |
| 3 | A 线批次 2-7：config 死字段删除 | ✅ 579e2930 |
| 4 | A 线批次 3：死代码清除（Rust 两组 + 前端） | ✅ be441f5a / 800a0980 / 4f1f3fbe |
| 5 | A 线批次 2-5：审批 family → signature 级（实施草图见收尾快照） | 待做 |
| 6 | A 线批次 2-6：traversal 硬拒——先枚举合法穿越场景 | 设计后定 |
| 7 | A 线批次 2-4：审批一次性 token（独立卡） | 未排 |
| 8 | B 线：发笔记路径给功能分支会话（用户动作） | 用户侧 |

## 夜间执行记录（每步更新）

### Step 0（00:35）— 日志建立，并行状态检查
- 并行会话未提交改动与 A 线目标零重叠，批次 1 开工。
- 待办提醒：批次 1 各修复按符号定位（方案行号基于 2d4baf09，已漂移）。

### Step 1（01:00）— A 线批次 1 完成 ✅
- **1-1 exec_command**（routes.rs）：overrides 提取为 `exec_command_overrides()`，显式 `approval_fallback: Some(Deny)`；回归测试断言 Deny + 无审批通道（防 ambient 默认将来变化）。
- **1-2 browser 会话键**（browser.rs）：`session_from_params` 改为 `_session_id` 优先 → `session` 覆盖 → "default"，注释写明注入机制；3 个测试（注入优先/显式覆盖/默认）。
- **1-3 pairing TOCTOU**（pairing.rs）：`create()` 的 mint+insert 收敛到同一临界区（对齐 `create_code_only`），`reset_attempts` 锁内调用已确认无死锁（attempts 与 inner 是不同互斥量）；并发唯一性回归网测试（4 线程 × 25 次 = 100 码全唯一，注释注明结构保证为主防线）。
- **验证**：`cargo check --workspace --all-targets` 0 错误；server 292 / tools 769 全绿。
- **提交**：见 git log `fix(audit-batch1)`。

### Step 2（01:30）— A 线批次 4 完成 ✅（注释/文档同步 + 择要整洁）
- SYSTEM_PROMPT.md：已删常量（TOOL_EXECUTION_SUMMARY / STOPPING_SUMMARY）改述为 REPLY_TERMINATION_GUIDANCE 承接（4 处）；A8 行删 `TASK_FOCUS_*` 引用（已核实 directions.rs 无此常量）。
- AGENTS.md AL-8 行：补双阈值说明（15/5 advisory WARN → 25/10 hard stop），引用改 95-121。
- local_agent_service.rs :55 区：reasoning-only 注释改"立即 break 交外层 decide_empty_response"（对齐 session_lifecycle.rs:639 现状）。其余 3 处同文案已随其他提交消失，无需改。
- pairing.rs PairedDevice.scopes 注释："恒 []" → 实际签发 DEFAULT_DEVICE_SCOPES=["chat"]（对齐 :158/:171）。
- routes.rs：exec_command 两行占位注释删除。
- agent_loop/mod.rs：RetryReasoning 所有权描述段删除（枚举保留）。
- App.tsx：墓碑注释（62-64）删；594 粘连 `};` 拆行；三处 liveWs 失真引用改"live session channel"表述。
- ws.rs：:75 毒锁改 into_inner fail-open；dedup 两条 info→debug（生产零输出）。
- channels/mod.rs：install_runtime 同毒锁 fail-open。
- memory.rs：async fn 内 5 处 std::fs → tokio::fs（675/722/749/763/809，全在 try_auto_archive 内）。
- **跳过**（已干净/不存在）：ws.rs:12 乱码注释（现文案正常）；pairing "指纹在 /pair 页核对"旧注释（grep 无）。
- **验证**：cargo check 0 错误；tools memory 24 / server 292 全绿；tsc 干净。
- **提交**：`chore(audit-batch4)`。

### Step 3（01:50）— A 线批次 2-7 完成 ✅（config 死字段删除，拍板 A）
- 删除 `GatewayConfig.tools`（+ `default_tools()`）与 `GatewayConfig.tools_config`（+ `ToolsSettings` 结构体）——全仓零消费点（评审已验证）。
- 同步修复引用：config.rs 两处测试断言、lib.rs 再导出、python_yaml_compat.rs:311 断言（改为"解析成功 + 未知键被忽略"断言）、prop_config_roundtrip.rs（import + 两处字段）。
- **免 alias 已验证**：hermix-config 无 `deny_unknown_fields`，旧 config.yaml 的这两个键被 serde 静默忽略——Consequences 预案的 alias 过渡不需要。
- **验证**：hermix-config check 0 错误 0 警告；config 118 + pyyaml 12 全绿；workspace --all-targets 0 错误。
- **提交**：`refactor(audit-batch2-7)`。

### Step 4（02:40）— A 线批次 3（Rust 第一组）完成 ✅
- convergence_detector：`apply_domain_max_stall` / `reset()` 两死方法删除，连同锻炼它们的 3 个测试（test_reset / test_apply_domain_max_stall×2 / test_domain_adjusted_threshold_takes_effect）。
- skill_orchestrator：`scan_directory`（被 scan_directory_with_health 取代的旧递归版）删除。
- a11y：`indent_check` 空函数删除 **连同其活调用点**（调用空函数本身也是死重量——审计漏了调用点，已补删）。
- vendor：`_typecheck_ensure_hermix_plugin_uses_path` 占位删除。
- security：`PolicyGuardConfig` 整组（struct+impl+from_env+check_*，生产零引用）+ `header_policy_admin` helper + lib.rs 再导出删除；`effective_decision_for_paired`（pub 版 `effective_decision_with_scopes` 的私有孪生）删除，**其测试改打 pub 版**——能力位∩矩阵语义的唯一覆盖由此保留（Local/Unpaired 两断言修正为各自 Origin，测试曾因此红过一次已修）。
- 顺手修：local_agent_service 重复 `#[test]` 属性（此前插入测试时的笔误）、pairing `code` 未使用警告（块尾改回 payload）。
- **验证**：server 290 全绿、tools 769 全绿、零警告。
- **提交**：`refactor(audit-batch3-rust1)`。

### Step 5（03:20）— A 线批次 3（Rust 第二组）完成 ✅
- context.rs：`DirRoute::MultiStepTask` / `HadAnyToolUse` 两死路由删除（变体定义 + match 臂 + 唯一测试；`tool_call_count` 字段保留——构造侧仍注入，无警告）。
- prompt_assembly.rs：两处"HadAnyToolUse / MultiStepTask route variants still exist"注释同步改述。
- terminal.rs：`ProcessBackend` trait + `ProcessHandler` 整组删除（935-1081 区），lib.rs 导出同删——文件从 2073 行降到 1923 行。
- 顺手修两处警告：convergence 重复 `#[test]`、local_agent_service 测试多余 `mut`。
- **验证**：agent 1052 / tools 769 全绿；workspace check 0 错误 0 警告。
- **提交**：`refactor(audit-batch3-rust2)`。

### Step 6（04:00）— A 线批次 3（前端 PR）完成 ✅
- rest.ts：24 个死导出删除（persona 12 + 插件/MCP/会话/自定义 provider 等 12）+ isCircuitTripped 永假 stub。
- api/types.ts：DashboardSession / PersonaTextFile / PersonalitySummary / PersonaOverrides 四接口删除。
- api/ws.ts：sendMessageStream legacy 包装删除。
- context/StreamChatContext.tsx：StreamChatProvider 陷阱导出删除（App 直接用 StreamChatContext.Provider）。
- 调用方死分支清除：useBackendConnection（熔断早退）、useCustomApps（2 处）、CustomAppsView（2 处）+ import 清理。
- **波折记录**：块切割器误吞 6 个模块级声明（_toolsCache / TOOLS_CACHE_TTL_MS / isoToMs / fromBackend / McpServerListResponse / MCP_SERVERS_KEY+OFFICIAL_MCP_SEEDS+load+save+toBackend）——已从 HEAD 原样恢复；persona helper 区（getTextFile/putTextFile）与孤儿节标题清理；类型导入修剪 8 项。
- **验证**：tsc 零错误；vitest 230 全绿。
- **提交**：`refactor(audit-batch3-frontend)`。

### Step 7（05:10）— 启动界面重设计 + 动态进度修复 ✅（用户追加需求，提交 4df577cc）

**根因（进度"死"的真凶）**：`#splash` 原在 `#root` 内部，`createRoot(#root).render()` 挂载 React 的瞬间清空容器全部子节点——splash DOM 被 React 首帧抹掉，main.tsx 整套状态机（boot-stage 事件/爬行/阶段点亮）从此空转。用户看到的始终是 index.html 静态 HTML（恒 6%、阶段 0）。

- index.html：splash 移出 #root 成 body 直接子节点；视觉重做（mascot 112px 悬浮+光晕、双色品牌字标、accent 顶光洗底、6px 流光进度、20px 大号百分比、勾/脉冲点/空心点三态阶段列表）。
- main.tsx：百分比 rAF 平滑补间；爬行 +2%/320ms。
- 验证：静态预览 + 浏览器截图核对（浅色 45% 推进态布局配色确认）；tsc 干净；vitest 237 全绿。

### Step 8（06:00）— E2EE 审查发现项修复：入站帧重放去重 ✅（用户追加，提交 f56c3c55）

**审查结论**（读 relay_crypto/identity/transport + 跑测试）：E2EE 整体扎实——选型修正有实测依据、每帧 ephemeral 前向保密、AAD 绑 v/tag/sid/nid、密钥交换防 MITM（node_pub 二维码出带 + device_pub 密封回传）、DPAPI 失败即报错、明文帧 relay 硬拒。relay 19/19（本机先失败只是 apps/relay 未装依赖，npm install 后全过）、server 386 全绿。

**发现（中等）**：`eid` 每帧随机但只进 HKDF salt——密文原样重放能正常解密（nonce/eph/AAD 全一致）。恶意 relay（托管形态威胁模型内）可重投旧"执行命令"帧，桌面端再执行一遍。签名 ±300s 新鲜度把窗口限住，窗口内仍可重放。

**修复**：`SessionMux` 增 `ReplayGuard`（按 node 分桶、600s TTL、每桶 256 上限，仅解密成功后登记防投毒），挂 `dispatch` 收口；eid 字段注释同步更正。测试 +3。

## 会话收尾快照（次日对接看这里）

**A 线已完成**：批次 1（`1aa398bd`）、批次 4（`f3912c4d`）、批次 2-7（`579e2930`）、批次 3 Rust 两组（`be441f5a` / `800a0980`）、批次 3 前端（`4f1f3fbe`）。净效果：审计方案除 2-4 / 2-5 / 2-6 外全部落地，净删约 800 行。

**剩余三件事（按优先级）**：

1. **批次 2-5（审批 family → signature 级）——实施草图已备好**：
   - 现状：`approve_for_session(effective_cmd)` 在 terminal.rs:222（HERMIX_CONFIRM 路径）与 :549（WS 审批路径）各一次；family 归类逻辑在 `approval/engine.rs:55-60`（批准一条命令 = 记住整个 rule family）。
   - 改法（拍板 A）：engine 的会话记忆键从 rule family 换成**完整 effective_cmd 精确匹配**（参数哈希一致才放行）；`engine.rs:55-60` 的 family 语义注释同步改写；terminal.rs:1622/1689 两个测试更新（family 内不同参数现在应再次弹审批）。
   - 注意：批次 1 已落的 schema gate 会让缺参调用提前被拒，与本条无冲突。
2. **批次 2-6（traversal 硬拒）**：动手前先枚举 terminal.rs:586 / memory.rs:272 判定语义 + 合法穿越场景（junction、子目录 `..`）。
3. **批次 2-4（审批一次性 token）**：独立卡，registry 签发 → TerminalApprovalContext 传递 → terminal 校验，替代 HERMIX_CONFIRM=1 前缀（terminal.rs:557-564 的 fallback 教学文案同删）。

**B 线（手机端产品化）**：规格与拍板齐备（`docs/notes/proposed/feature/2026-10-06-relay-hosted-multi-tenant.md`，三项拍板已结）——把路径发给功能分支会话即可开工；E2EE 是托管上线门槛、实现顺序第一位。

**用户侧手动待办（自用 relay dogfood）**：证书指纹 env + relayctl 签发两条 token（命令见 Step 0 背景快照）。

**验证基线**：`cargo check --workspace --all-targets` 0 错误；server 292 / tools 769 / agent 1052 / config 130 全绿；前端 tsc 干净 + vitest 237 全绿（26 文件）；check-notes OK。（`ci-gate.sh` 全量未跑——clippy 锁守卫建议对接后跑一次。）
