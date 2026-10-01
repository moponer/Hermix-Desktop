# Decision Note: 任务报告书 — 2026-10-01 审计响应与批次执行

Status: implemented — 待审批 agent 复核；17 commit，净减 4580 行，CI gate 11/11

> 分支 `feat/auto-20261001-c09d7e4f`，基线 `f469b2ae`。
> 净变更 **128 文件 / +5400 / -9980（净减 4580 行）**。

## Decision

执行 2026-10-01 安全审计报告与 2026-09-30 决策笔记的待做批次，并在执行中
**以源码证据推翻了三处前提**：迁移表对群聊文档的"现行依据"判断（模块早已
删除）、对 `probe-stdio-approval.py` 的"零引用"判断（是回归守卫的硬依赖）、
以及 L0 别名层方案的前提（Tailwind v4 不从 `:root` 生成工具类）。每处
推翻都保留了记录与证据，未静默改写。

三项工作被判定为不可实施并记录否决：H3 持久终端（Windows 无 PTY）、
J2 resource_guard 拦截（与 5 天前定策冲突）、以及 E10/E16 两条字面值
（真例外）。

## Alternatives considered

- **L0 用别名层（`:root`）而非 `@theme` 重定向色槽** —— 否决。任务书与
  EXCEPTIONS.md 都写"在 @theme 中重定向"，但两者都隐含"放哪都能生效"。
  实测证明 Tailwind v4 只从 `@theme` 生成工具类，别名层的定义不产生
  任何 `.text-red-400` 规则——重定向静默落空。已用最小编译探针固化该
  结论并写进代码注释。
- **101 处色阶类改写成 inline `style={{ color: "var(--color-error)" }}`** ——
  否决。同一元素可能有多个 palette 类（如
  `border-red-500/40 bg-red-500/5`）且带 `hover:` 变体前缀，逐个替换会
  产出多个 `style` 属性（非法 JSX）或丢失变体。token 重定向零改动、
  零风险。
- **全部 shade 塌成同一个语义 token** —— 否决。同族不同 shade 在本项目
  承载真实视觉层次（`red-300` 是标题、`red-200` 是次要说明），塌平会让
  层次消失。改为按 shade 映射不同 alpha。
- **照搬迁移表只清 5 个点名文件** —— 否决。任务书完成定义是"门禁收紧后
  CI 全绿"，而实际有 101 处 / 15 个文件，漏掉的 5 个文件 35 处会让门禁
  收紧当天就把主干变红。按全清执行。
- **在 `resource_guard` 实现 J2** —— 否决，理由见第九节第 7 点。

## Consequences

- **获得**：安全边界显式化（审批策略成为编译器强制的表态而非隐式
  fail-open）、工具契约与生产验证过的 dsh 对齐、109 处色债清零使 L1 的
  强调色切换有实际效果、仓库卫生有 CI 兜底、决策有据可查。
- **放弃**：非流式 HTTP 上 Dangerous 工具改为拒绝（行为变更）；patch
  改为严格字面匹配（模型 `old_string` 不精确时会失败并被要求重读文件）；
  持久终端在 Windows 上仍不可用。
- **代价**：`CompletePairingBody` 新增必填字段，外部调用方需同步；
  21 个色槽进 CONTRACT，每套主题须声明（这是正确的——主题各自指向本
  主题四色组）；CI gate 从 10 步增至 11 步。
- **风险**：见第九节八条待复核项，其中 1/2/4 涉及行为变更与安全边界。

---

## 一、交付范围

| 来源 | 批次 | 状态 |
|---|---|---|
| 2026-10-01 安全审计报告 | 17 条缺陷 | 13 确认 / 2 推翻 / 2 部分成立 → **全部处置** |
| 2026-09-30 决策笔记 | A/B/C/D/E/G/I/J/K/L0 | A-E/G/I/J/K 完成；F 不做；H3/L0-H3 否决；L1+ 未做 |
| 会话内追加 | 参数类型扫描、临时文件清理 | 完成 |

---

## 二、安全与可靠性修复（14 项）

### P0

| # | 问题 | 修法 | 位置 |
|---|---|---|---|
| 1 | 审批 fail-open：无审批通道时 Dangerous 工具静默放行 | 引入 `ApprovalFallback { Deny(默认), Allow }` **必填参数**，编译器强制每个调用方声明暴露面。HTTP/裸 WS = Deny，stdio = Allow | `hermix-core/approval.rs`、`local_agent_service.rs` |
| 2 | 配对无指纹校验（TOFU 只在注释里） | 指纹进 URL、落地页分组显示、`complete_pairing` **在 consume 之前**强制比对 | `hermix-server/pairing.rs` |

### P1 前端

| # | 问题 | 修法 |
|---|---|---|
| 3 | ApprovalDrawer Enter 劫持：焦点在"拒绝"上按 Enter **同时**触发 deny 和 approve | Enter 分支加 `closest("button, a[href], summary, [role='button']")`，命中则让位原生激活 |
| 4 | 澄清/审批回复被静默丢弃 | 根因是**错误的守卫**：`backend_channel_send` 写后端 channel，与前端订阅无关。去掉守卫 + 真实失败上报 |
| 5 | 跨会话误杀流 | `cancelStream()` 补 `activeSessionId`，恢复归属守卫 |
| 6 | 定时器泄漏 | subAgents 清理加归属检查；3 处 approval REST 定时器纳入 ref + 卸载清理 |

### P2 后端

| # | 问题 | 修法 |
|---|---|---|
| 7 | C.2 失败轮未答 tool call | `backfill_unanswered_tool_results` 紧邻 assistant 消息补录 |
| 8 | D.4 批超时 drop JoinSet | 5s 有界 grace + drain 真实结果 |
| 9 | D.5 审批串行 | 两阶段：先发完全部卡，`join_all` 并发等待 |
| 10 | E.1 `?token=` 凭证通道 | 移除；URI 脱敏保留为纵深防御 |
| 11 | tool_executor panic 归因 | `task_id → call_id` 映射，替代"塞第一个空槽" |
| 12 | "安全网"硬编码 `id="missing"` | 改用真实 `tool_calls[idx].id` |
| 13 | sub_agent 超时不 abort | `join.abort()` |
| 14 | 密钥文件无权限收紧 | rename **之前** `0o600` |

### 追加：参数类型扫描（两轮）

`Value::as_bool()` 只认 JSON 裸布尔，模型常输出字符串形式。`.and_then(as_bool).unwrap_or(default)` 把"类型不符"伪装成"没传"，报错还指向错误方向，模型陷入重试循环。

- **15 处 hermix-tools 调用点** → `param_coercion::bool_param`
- **2 处 hx-shell** → 本地等价 helper（该 crate 只依赖 serde_json）
- **3 处 hermix-config** → IM 通道开关静默失效

**最危险的一处**：`voice_mode.rs:657` 的 `unwrap_or(!previous)` 是**取反**——模型传 `"enabled": "false"` 会把状态翻转，用户说"关掉"实际"开启"。

---

## 三、文档制度（批次 A/B）

`docs/notes/` 三态决策记录体系建成并挂进 CI（第 10/11 步，硬门禁）。

- **A**：删 16 篇一次性报告、归档 22 篇过期设计（净减 5368 行）
- **B**：11 篇迁入 notes，补齐 Decision / Alternatives / Consequences 强制节
- 门禁由 WARNING 升级为 **fail**，负向测试确认可拦截

### 5 处与迁移表不符的纠正（均附证据）

| 迁移表 | 实际 | 处置 |
|---|---|---|
| `CHANNEL_WORKSPACE_SCOPE.md` 有效，待迁 | **git 历史中从不存在** | 从白名单剔除 |
| 两篇群聊文档"现行依据" | `80fab6be`(09-21) 已删除整个群聊模块，源码零残留；迁移表写于 09-30，晚 9 天 | 改判 `rejected` |
| `FILE_PREVIEW_REFACTOR` 待做 | `components/preview/` 10 文件已落地，Panel 736→315 行 | 改判 `implemented` |
| "8 删 9 留" | `verify-stability-suite.py:325` shell out 到 `probe-stdio-approval.py`（scenario 5 回归守卫） | **删 7 保留 1** |
| `docs/` 56 篇 | 顶层 49 + 子目录 7 = 56 ✓ | 口径无误 |

---

## 四、工具契约对齐（批次 K，对标 dsh）

| 项 | 内容 |
|---|---|
| **H1** ✅ | `read_file` 的 Fs 接缝**拒绝** offset/limit，截断提示却叫模型用它——契约自相矛盾。改为接缝自行行级切片，提示给出确切续读行号 |
| **H2** ✅ | `patch` 模糊匹配可能**静默改错位置**。改为字面精确 + 唯一性校验，模糊链仅在 `allow_fuzzy: true` 可达 |
| **H3** ❌ 否决 | `pty: true` 在 Windows 直接报错（Unix 靠 `script` 命令，Windows 无对应物）。实现需从零引入 ConPTY 绑定 + 会话表 + 超时回收 |
| **H4** ✅ | `SpillSink` 早已写好且有测试，但**生产路径从未调用**。接入后截断内容可取回 |
| **H5** ✅ | `search_files` 加 `context`（grep `-C`），省一轮 read_file |
| **H6** ✅ | `read_file` 补 Model Experience 段 |

---

## 五、仓库卫生（批次 J1/J3，J2 否决）

- **J1** `directions.rs` 新增文件写入纪律（FirstTurn 全文 / Subsequent 压缩）
- **J3** 新增 `scripts/check-hygiene.sh`，挂 ci-gate `[11/11]` 硬门禁
- **J2 否决**：`resource_guard` 明确豁免 `write_file`/`patch`/`terminal`（2026-09-26 定策，有 3 个测试钉死）。J2 要推翻它，粒度也不匹配（路径形态 vs 路径授权）

---

## 六、L0 颜色债清偿（L1 的硬前置）

101 处 Tailwind 原生色阶类**一行未改**，改为在 `tokens.css @theme` 内把**实际在用的 21 个色槽**重定向到语义令牌。

| 族 | 槽 | 指向 |
|---|---|---|
| error | red-200/300/400/500, rose-200/300/500 | `--color-error` |
| warning | amber-200/300/400/500, yellow-400 | `--color-warning` |
| success | green-400/500, emerald-200/300/400/500 | `--color-success` |
| neutral | zinc-300/400/500 | `--color-text-tertiary`/`-faint`/`-secondary` |

同族不同 shade 承载**真实视觉层次**（`red-300` 是标题、`red-200` 是次要说明），故按 shade 映射不同 alpha，而非全部塌成一个 token。

**踩坑（会导致静默失效）**：先按 EXCEPTIONS.md 措辞把定义放别名层（`:root`），实测发现 **Tailwind v4 只从 `@theme` 生成工具类**——别名层的定义**根本不产生 `.text-red-400` 规则**，重定向静默落空且无任何报错。用最小编译探针证实后改为 `@theme`。若未实测直接提交，会交付"看起来做了、实际零效果"的 L0。

代价：21 槽进 CONTRACT（85→106），每套主题须声明——这是正确的，主题各自指向本主题四色组。

**门禁收紧**（独立 commit 可回滚）：`[palette-debt` 从 WARN 改为阻断，语义变为**未登记的色阶类才拦**。负向测试：注入 `text-sky-500` → 门禁 EXIT=1。

### L0 第二、三块

- **E9-E16**：6 条已迁（遮罩→`--color-overlay-scrim`、阴影→`--shadow-pop`、mention→`--color-link` 等），2 条判为**真例外保留**（E10 是用户自配品牌色、E16 是 dev-only console 徽章）
- **CSS 层**：任务书称 34 处 `[data-theme]` 覆盖，实测**只有 10 处**，其中 8 处已用语义 token；仅 git add/删 绿红 2 处字面值已迁

---

## 七、验证证据

| 检查 | 结果 |
|---|---|
| `cargo check --workspace --all-targets` | 通过，零错误 |
| `cargo test -p hermix-agent --lib` | **1043 passed** |
| `cargo test -p hermix-core --lib` | **181 passed** |
| `cargo test -p hermix-tools --lib` | **725 passed** |
| `hermix-server` / `hermix-config` / `hx-shell` | 262 / 118 / 9 passed |
| `pnpm tsc --noEmit` | **exit 0** |
| `vitest run`（chatStateReset） | 9 passed（含 2 个新增回归） |
| `check-theme-tokens.mjs` | contract **106/106**、无孤儿、无裸色、无未登记色阶 |
| `bash scripts/ci-gate.sh` | **11/11 步全过** |

新增回归测试 30+：审批 Allow/Deny 双向、scratch 降级边界（含混合列表/穿越/空列表）、read 分页、patch 唯一性、search 上下文、参数真值判定、voice_mode 反转、hx-shell 参数键、config 布尔、文件写入纪律、仓库卫生门禁（违规+合规双样本）。

---

## 八、过程中自查出的问题

| 类别 | 事件 | 处置 |
|---|---|---|
| **事故** | PowerShell `-replace` 参数错误导致 22 个归档文件被清空 | 立即停下，`git checkout-index` 从索引恢复，**写脚本逐文件校验内容完整性**，确认一字未失 |
| **事故** | `cargo fmt -p hermix-agent` 波及 102 个无关文件重排版 | 回退全部，逐处重放实质改动，`local_agent_service.rs` 从 1012 行 diff 收敛到 96 行 |
| **自查** | 首版 helper 把"键存在但类型不对"也归为 default | 被自写测试抓出（`{"overwrite": 1}` 会走破坏性分支），改为 absent ≠ wrong-type |
| **自查** | `ScratchGuard` Drop 删整个目录，并发测试互删 | 改唯一子目录 |
| **自查** | L0 别名层方案会静默失效 | 最小编译探针实测后改 `@theme` |
| **自查** | `git ls-files ':/*'` 并非"顶层"（`*` 不绑斜杠） | 门禁自身 bug，改为显式 `grep -v '/'` |
| **自查** | 门禁第一版检查的扩展名全被 gitignore 覆盖 | 聚焦可跟踪类型 |
| **误判修正** | 上一轮说 Rust 测试"失败" | 实为 290 秒超时，非失败；改用 3600 秒上限 |

---

## 九、请审核 agent 重点复核

1. **审批 fail-open 的策略选型**：HTTP/裸 WS = Deny、stdio = Allow，是否符合你的暴露面判断？非流式 HTTP 上 Dangerous 工具现返回 Deny（行为变更）。
2. **配对指纹为必填字段**：`CompletePairingBody.cert_fingerprint` 新增必填，**已配对的老设备或外部客户端需同步补字段**，否则 400。
3. **patch 改为字面唯一匹配**：原模糊匹配会静默改错位置，但改成严格后，模型若 `old_string` 不精确会失败（报错提示重读文件）。UX 权衡是否可接受？
4. **scratch 豁免的安全边界**：混合列表整体不降级、`..` 穿越不降级、空列表不降级——是否有绕过路径？
5. **L0 的 shade→alpha 映射**：`red-300`→82%、`red-200`→65%，视觉浓淡是否需要在真机走查确认？
6. **E10/E16 判为例外**：用户自配品牌色与 dev-only console 徽章保留为字面值，是否同意？
7. **J2 否决**：不动 `resource_guard` 的 2026-09-26 定策，是否同意？
8. **H3 否决**：不实现持久终端（Windows PTY 需引入 ConPTY），REPL/gdb/venv 跨调用场景仍不可用，是否接受？

---

## 十、剩余工作

- **L1**（强调色可选 `--color-accent-choice`、深浅色 token 校准、对比度滑杆）— L0 已铺路
- **L2/L3/L4/L5**（设置中心重构、Switch/SettingsRow、壁纸新 command、主工作区）— 未动
- **批次 F**（事件溯源 / Windows ACL 沙箱 / PTC）— 按原提案单独立项
- **H3**（持久终端）— 如需，应作为独立立项先写 ConPTY 技术方案
- `type-debt` 13 处 `text-[Npx]` — 任务书未列入，顺带发现
