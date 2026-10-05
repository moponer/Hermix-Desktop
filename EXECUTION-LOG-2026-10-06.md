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
| 1 | A 线批次 1：exec_command 显式 Deny + browser 会话键 + pairing TOCTOU（各带测试） | 进行中 |
| 2 | A 线批次 4：注释/文档同步（逐条见方案 §批次 4） | 待做 |
| 3 | A 线批次 2-7：config 死字段删除（tools/tools_config + roundtrip 测试，免 alias 已验证） | 待做 |
| 4 | A 线批次 3：死代码清除（拆 Rust / 前端两个 PR） | 待做 |
| 5 | A 线批次 2-5：审批 family → signature 级（terminal.rs approve_for_session） | 待做（中风险，排 4 之后） |
| 6 | A 线批次 2-6：traversal 硬拒——**先枚举合法穿越场景再动** | 设计后定 |
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

### 下一步
- Step 3 = 批次 2-7 config 死字段删除（tools/tools_config + roundtrip 测试，免 alias 已验证）。
