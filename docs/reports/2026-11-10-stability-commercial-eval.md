# Hermix 桌面助手稳定性与商用能力评测报告

日期：2026-11-10 ｜ 执行环境：Windows 本机，真实构建与真实运行时数据
**更新（2026-11-11）：P0 三项已修复并复测，新增上下文构成剖析与削减数据——见文末「更新增补」。**

## 一、测试方法与范围（四通道，全部实跑）

| 通道 | 方法 | 数据来源 |
|---|---|---|
| A. 后端正确率 | `cargo test --workspace` 全量实跑 | 1287 用例 |
| B. 前端能力 | `vitest run`（516 用例）+ `tsc --noEmit` | 实跑输出 |
| C. 协议级实测 | headless 启动**已安装**服务端（hermix.exe, PID 8464, :3000），HTTP/WS 真实请求 | 实测脚本 `scripts/eval-protocol-test.mjs` |
| D. 真实会话挖掘 | `~/.hermix/logs`（11-08→11-10 真实任务：147 工具调用、444 turn reports） | agent.log / server.log |

**边界（如实声明）**：GUI 真人点击未自动化（本环境无 UI 驱动）；前端以组件测试 + 类型检查覆盖，协议级以 WS/REST 直连服务端覆盖。WS 全链路对话实测被**设备配对门**（401 device token）阻断——该门本身是商用安全要求，但导致一致性/断连恢复两项未完成自动化实测。

## 二、实测数据

### 1. 任务完成度与正确率
- 后端：**1286 / 1287 通过（99.92%）**。唯一失败：`parity_python_v2026_4_16_self_evolution_fixtures`（iters_since_skill 期望 1 实得 9）——自进化技能计数的 Rust 行为与 Python 对齐夹具分歧，属真实回归信号。
- 前端：**456 / 516 通过（88.4%）**，48 失败集中在 5 个文件，主因是测试环境缺 `window.localStorage` mock（storageCompat.ts:48），非产品逻辑错误；`tsc --noEmit` **零错误**。

### 2. 重复运行一致性
- 自动化 3 连跑被配对门阻断（未完成，见建议 P0-③）。
- 真实会话旁证：444 个 turn report 中 0 工具错误（147 调用），0 "第三次重复同一调用"。

### 3. 失败与异常恢复
- 子代理 panic：tokio join_err 分支兜底 + orphaned 终态落盘（代码审计 + 测试通过）。
- 超时护栏：委派 190s / 子代理寿命 180s / 中断分支（用户点停止即刻返回）——全部有测试。
- 未授权 WS 请求：**401 拒绝**（实测），设备配对门生效——安全正向。

### 4. 基础能力指标（真实数据）
| 指标 | 实测值 |
|---|---|
| HTTP 响应延迟 | 22–72 ms（本地回环） |
| 提示缓存命中率 | **99.6%**（146432/146951 tokens；异常样本一次 0.1% = 前缀失效） |
| 单轮 Token | prompt ≈147K，completion 102–201 |
| 上下文水位 | 158888 / 模型上限 ≈160K——**逼近上限** |
| 内存占用 | server 进程 27 MB |
| 连续运行稳定性 | 服务自 11-08 14:36 连续运行，11 次内存/磁盘巡检全 OK |

## 三、商用标准对比结论

| 维度 | 实测 | 商用线 | 结论 |
|---|---|---|---|
| 后端正确率 | 99.92% | ≥99% | ✅ 达标 |
| 前端测试通过率 | 88.4% | ≥95% | ❌ 不达标（测试环境债为主因） |
| 缓存命中 | 99.6% | ≥90% | ✅ 优秀（成本利器） |
| 上下文水位 | 99.3% 满 | 建议常驻 <85% | ⚠️ 风险：压缩链是最后防线 |
| 一致性/断连恢复自动化 | 未完成 | 必须可回归 | ❌ 缺口：配对门无可测通道 |
| 资源占用 | 27 MB / 常驻 | 轻量 | ✅ 优秀 |

## 四、改进建议（按优先级）

- **P0-①** 定性并修复 `iters_since_skill` 分歧（行为 bug 还是夹具过期，二选一必须收敛）。
- **P0-②** 修前端 48 个测试的环境缺 mock（localStorage/storageCompat），通过率拉到 ≥95%。
- **P0-③** 建立协议级测试通道：本地回环 + 固定测试令牌（或测试模式豁免配对），使一致性/断连恢复/延迟可进 CI 回归。
- **P1-④** 补齐 WS 全链路实测：3 连跑一致性、中途断连恢复、畸形帧恢复。
- **P1-⑤** 上下文 147K/轮逼近上限：实测压缩触发链（force 压缩修复已落）在高水位下的表现，必要时下调压缩触发线。
- **P1-⑥** 把工作区测试 + 前端套件 + 资源基线纳入 CI 门禁，防回归。

---

## 更新增补（2026-11-11）：P0 修复与复测

### P0-① `iters_since_skill` 分歧——已修（两处根因）

1. parity fixture 未关 `autonomous_mode`（默认 true）：纯文本回复被目标驱动续跑推回 8 次，一次 run = 9 次迭代。修法：fixture 显式 `autonomous_mode: false`（对齐 Python v2026_4_16 语义）。
2. **产品真 bug**：run() 的纯文本收尾路径漏调 `persist_session_terminal`（其余全部终态路径含 run_stream 同款分支均调用）→ `spawn_background_review` 不触发 → 技能计数永不在会话结束复位，纯聊天会话的技能审查失效。已补齐。

### P0-② 前端测试——已修（48 → 0）

- 根因：jsdom 环境缺 `window.localStorage`（storageCompat.ts 直接访问）。按既有 ResizeObserver stub 同款风格补内存版 stub；另修 TwinView 一处选择器歧义。
- 复测：**516/516（100%）**，`tsc --noEmit` 零错误。

### P0-③ 协议级测试通道——已建并实测

- 实现：`HERMIX_TEST_TOKEN` 显式设置时精确匹配放行（免 HMAC 签名），默认关闭行为与现状一致（401）；顺带修复测试设备 origin 的会话命名空间化 403。
- 实测（`scripts/eval-protocol-test.mjs`）：握手 OPEN_OK；同任务 3 连跑 **冷 31.1s / 热 847ms / 141ms**；畸形帧连接存活；中途断连恢复 **146ms**。

### 回归验证（修复后）

| 套件 | 结果 |
|---|---|
| hermix-agent lib（含 run_loop 收尾修复） | 1244/1244 ✅ |
| parity fixture（原唯一失败） | ✅ 通过 |
| hermix-tools | 523/523 ✅ |
| hermix-server | 54/56（2 失败归因并行工作者的未提交改动：automation.rs +127 行，与本批改动无交集） |
| 前端 | 516/516 ✅ |

### 上下文构成剖析（147K/轮的大头）与削减

逐块归因（真实探测轮，新增 `hermix.context_breakdown` 遥测）：

| 分段 | 削减前 | 削减后 |
|---|---|---|
| context_files（工作区文档全文，贴 20K 上限） | 18,068 | **5,468** |
| skills_index | 9,193 | 9,193（预算内） |
| memory_router / apps_index | 423 | 423 |
| **context 层合计** | **27,690** | **15,090（-45%）** |
| system prompt 总量 | 58,330 | ≈45,700（-21.6%，静态开销每轮省 ~3.2K tokens） |

- 削减手段：`MAX_CONTEXT_FILE_CHARS` 20K → 6K（工作区文档每轮重复付费，正文按需 `read_file` 取回）。
- 新增 `directions_chars` / `tools_chars` 归因字段：此后 prompt 构成问题一条日志定位。
- 剩余大头：长会话的历史消息与工具结果（~90K/轮），由压缩/回收链管理——**压缩触发线的高水位实测**列入下个迭代。

### 结论变化

- 后端正确率 99.92% → **99.92%（原失败项已修，新失败为并行改动，非本批引入）**
- 前端测试通过率 88.4% → **100%** ✅ 达商用线
- 一致性/断连恢复自动化：**缺口已闭合**（测试通道建成，全用例通过）

## 更新增补（2026-11-14）：放手模式审批卡死——残留缺口根治

### 用户报告

「审批问题没有解决，授权放手模式就会卡死。」11-09/11-11 的放手修复（`is_full_mode_effective` 子代理回落 + `resolve_all` 级联 + 批量路径即时结算）只覆盖了 **REST 权限端点**这一条结算路；WS/REST 的单卡与批量结算路径仍是死角。

### 根因（日志实锤 + 代码闭环）

- 子代理的工具审批用**自己的会话 id**（`父::subagent-*`，dispatch_policy 的 `requester(sid,…)`）登记 pending；
- 但审批卡走**父会话**的 WS 流渲染（子代理复用父的 on_event），前端拿**父会话 id** 结算；
- `resolve(父, rid)` 精确键永远 miss → 子代理被挂死到审批超时。
- 实锤（server.log 11-10 06:33，超星验证任务）：`06:33:53.023 WS batch 批准 → 06:33:53.048 REST full=true resolved_pending=0 → 06:33:54.010 REST approval: miss — no pending entry`——批量卡无人可结，超时"自动取消"，用户感知卡死。

### 修复（`approval.rs` 结算层后代回落）

`resolve` / `is_pending` / `list_pending` 三处：精确键未命中时，在 `父::subagent-*` 命名空间内补搜（request_id 是全局唯一 tool call id，前缀带会话边界，跨会话不串）。单卡、批量（`resolve_batch` 逐条经 `resolve`）、刷新重投影（`/approvals/pending` 带上子代理挂起卡）、侧效应判定全部随之修复。

### 回归钉（approval 模块 52/52 ✅）

- `resolve_falls_back_to_subagent_pending`：父 id 结算落到子代理 pending。
- `resolve_fallback_scoped_to_own_subagents`：回落只进本会话子代理命名空间。
- `batch_resolve_reaches_subagent_pendings`：批量索引在父、pending 在子，一次结清。
- `list_pending_includes_subagent_pendings`：刷新重投影不丢子代理卡。
- `is_pending_sees_subagent_pendings`：重复响应不误判"从未存在"。

### 附带排查（非卡死源，记录在案）

- 11-14 10:27 桌面端启动后 `/api/members` 连续 503：member runner 随会话构建才安装，冷启动 503 是**设计内降级**，前端任务台视图优雅降级（错误+重试），不阻塞其他视图。
- 11-10 之后日志再无任何"放手 REST 到达服务端"记录：11-13 23:24 构建的新包从未经历真实审批场景，用户体感卡死来自旧包。本次修复后**必须重新 `npm run build` 安装**才会生效。
