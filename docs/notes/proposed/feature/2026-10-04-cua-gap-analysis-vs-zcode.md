# Decision Note: 电脑控制（CUA）差距分析 — hermix vs ZCode

Status: proposed — 2026-10-04 读 ZCode（`zai-org/ZCode` @ v3.14.3）CU 相关源码后，对 hermix 电脑控制能力做差距盘点。结论：**工具面 hermix 更宽，ZCode 更窄更安全；差距不在"能不能点"，在"敢不敢让它一直点"**。本文只分析，不含实施计划（另篇）。

## 背景事实（读源码所得）

### 一、最重要的一条：ZCode 的 CU 运行时本体是闭源的

`packages/zcode-cua/package.json` 自己写着：

> "API-compatible placeholder package for Computer Use. **This build ships without Computer Use: every runtime surface reports unavailable and fails closed.**"

开源仓里 `packages/zcode-cua/` 只有 **585 行 `.d.ts`**，零 `.ts` 实现。`setupComputerUseRuntime`（CU 的模型侧引导语，架构强制每个 CU cell 第一句必须原样照抄）**全仓只有一处引用、无定义**——定义在未开源部分。

推论（决定本篇全部结论）：**ZCode 的 CU 内核不可抄**。能抄的只有它的**外围契约**：进程边界、权限面、协议、事件。这些恰好就是差距所在。

### 二、ZCode 的 CU 架构：四个独立层，agent 碰不到 broker

```
模型 ──mcp__node_repl__js──> 宿主 node_repl（每 cell 新 Worker）
                                    │ 注入 broker 凭据
                                    ↓
                            broker server（独立进程，51 文件）
                                    │ 本地 socket
                                    ↓
                            Helper（macOS AX / Windows 原生）
```

关键点：

- **CU 不注册为原生工具**。`runtime/types.ts:168` 注释：「是否把 CUA broker 凭据注入共享 node_repl；**不代表注册独立 CUA MCP**」。走 `node_repl` 的 `js` 工具，SDK 在 cell 内暴露。
- **每个 cell 是全新 Worker，SDK 绑定不跨 cell**——所以引导语必须每 cell 重发，这条是架构强制不是文档软要求（`computer-use-operation-event.ts` 注释）。
- **子 agent 禁用 CU**：`computer-use-policy.ts` 导出 `SUBAGENT_COMPUTER_USE_UNAVAILABLE`，并用 `isPathWithin` 校验插件根路径，防同名第三方 Skill 冒充官方 CU。
- **broker 协议是 fail-closed 的**：`notAuthorized` / `notSelectable` / `notSettable` / `elementUnavailable` / `actionUnavailable` / `foregroundRequired` 六种结构化错误码 + `isReadOnlyBrokerMethod` 只读方法白名单。
- **权限四态**：`granted | stale | denied | unknown`——`stale`（授权方进程还在但授权已失效）是 hermix 没有的概念。

### 三、帧完整性存证：本次分析发现的最值得抄的一条

`frame-contract.d.ts` + `image-normalization.ts` 组成一套**防模型伪造截图**的机制：

- producer（Helper）签发 integrity metadata（`OFFICIAL_CUA_FRAME_INTEGRITY_META_KEY`）
- `hasOfficialCuaFrameAuthority()` 校验：`_meta` 有签发标记 **且** image 块后紧邻合法 frame 引用文本
- 非 authority 的结果里若出现 frame 引用文本 → **直接剥除**（`isOfficialCuaImageRefText` → `continue`）

注释写明动机：「第三方伪造的 actionable frame_id 即使会被 producer registry 拒绝，也不应进入模型上下文**污染坐标契约**」。

**这不是防截图造假，是防坐标契约污染**：CU 的可点击区域以 `frame_id + 坐标` 表达。模型若能自己编一个 frame_id 引用，就能让后续 click 打在任意位置——**注入攻击面**。hermix 走 UIA `auto_id`/`class_name` 路线，契约面比 frame_id 宽，但「模型输出的定位符必须来自可信签发」这条缺失是同构的。

### 四、ZCode 的两个截图预算常量（数值可直接抄）

```ts
MCP_IMAGE_INLINE_BASE64_BYTES = 200 * 1024        // 通用 MCP 图片 inline 上限
HOST_NODE_REPL_IMAGE_MAX_DIMENSION = 2048         // 轮尾展示预算
HOST_NODE_REPL_MODEL_IMAGE_MAX_DIMENSION = 2000   // Provider 模型上限（2000px）
```

超限策略是三段：可信宿主图片 → 压到 200 KiB；浏览器截图 → 落 artifact 并在 image **之后**追加路径文本；无 artifact store → 换文本说明。

「提示文本插在 image 之前会把 image-first 的 tool_result 变成 text-first；Anthropic 兼容网关只解析开头的连续 image，text 一领先后面的图就被丢弃」——**这是踩过网关的坑**，hermix 走自有 provider 通道，不一定复现，但同款网关会复现。

### 五、hermix 现状

`hermix-cua` 5,280 行 / 15 个原生工具：

- 观察面：`cua_screenshot` / `cua_screen_info` / `cua_list_windows` / `cua_ui_tree` / `cua_ui_find` / `cua_window_at_point`
- 动作面：`cua_left_click` / `cua_double_click` / `cua_move_cursor` / `cua_scroll` / `cua_type_text` / `cua_press_key` / `cua_click_window` / `cua_ui_click` / `cua_ui_focus` / `cua_ui_invoke`
- 捕获：`capture/dxgi.rs`（DXGI 桌面捕获）+ `uia.rs`（880 行 UI Automation）+ `ocr.rs` + `element.rs`（488 行元素解析）
- 安全：`safety.rs` 三闸（黑名单键 chords / 滑动窗口限速 / approval mode）+ `trace.rs`
- 浏览器：独立的 `browser/`（CDP，与 CU 分离）

**风险分级现状**：`cua.rs:64-101` 里 14 个工具标 `CoreRisk::Dangerous`，只有 `cua_move_cursor` 是 `Safe`。

## 差距盘点

按"能不能立项"分三档，不按功能多少分。

### 差距 A：沙箱边界（最关键，hermix 完全缺失）

| 维度 | ZCode | hermix |
|---|---|---|
| 进程边界 | CU 跑在**独立进程**（broker + Helper），agent 侧只是拿到 socket 凭据 | CU 跑在 **sidecar 进程内**，与 agent loop 同一进程（`hermix-cua` 被 `hermix-tools` 直接调用） |
| 崩溃隔离 | Helper 崩了 broker 还在，可重启 Helper，不影响 agent | `hermix-cua` panic 会打穿 sidecar → **触发整个 watchdog 重启**，正在跑的会话全丢 |
| 能力泄露面 | 模型只能碰 `node_repl` 的 js，broker 方法集有只读白名单 | 模型可直接调 15 个 `cua_*` 工具，无 broker 层收口 |
| 授权粒度 | 四态（granted/**stale**/denied/unknown），有 `stale` | 无授权状态机，审批在 `hermix-tools` 派发层 |

**这一档是全部差距的根**。`cua_left_click` 在 sidecar 内直接调 `SendInput`，一个 panic 或野指针就带走整个后端进程。对比 hermix 已有的稳定性努力（`hermix:stream-bridge-dead` 桥死事件、mid-stream watchdog）——**CU 反而是最不稳的一块，因为它拿到了最危险的原始能力**。

**能抄的**：ZCode 的 broker 协议六错误码 + `isReadOnlyBrokerMethod` 白名单 + 四态授权。这些是纯契约，不需要抄它的闭源内核。

**不能抄的**：Helper 本体（macOS AX / Windows 原生私有实现）。

### 差距 B：注入防护（hermix 完全缺失，且是真实攻击面）

ZCode 的 `hasOfficialCuaFrameAuthority` + 非 authority 帧引用剥除（见背景三）。hermix 侧对应缺口：

- `cua_ui_click` / `cua_ui_find` 接受模型给的 `auto_id` / `class_name` / 坐标，**无签发校验**——模型可以编一个 `auto_id` 让点击落到任意控件
- `cua_screenshot` 的结果直接进 ctx，**无 integrity 元数据**——模型无法区分"真截图"与"上一轮的文字描述"
- 叠加风险：`cua_type_text` 能往任意前台窗口送键盘输入，组合 `cua_ui_find`(编造定位符) + `cua_ui_click` + `cua_type_text` 就是一条完整的注入链

**这一档不需要任何 ZCode 代码**，是 hermix 自己的设计债，且改动局限在 `cua.rs` + 一个新的签发层。

### 差距 C：token 与轮次（ZCode 明显更省，hermix 吃亏）

ZCode 走 node_repl heredoc：多步 CU 编排写在**一段代码里一次投递**，中间结果不占父上下文——这与已 rejected 的 PTC 笔记是同一个机制。

hermix 15 个细粒度工具意味着**一次"填个表点提交"要 6-8 轮 tool call**，每轮都付 token + 时延，每轮都进审批面。

**但**：PTC 刚在上一轮评审被 rejected（`rejected/architecture/2026-10-01-ptc-programmatic-tool-calling.md`），所以这一档**本轮不可动**。记在这里是为了让差距清单完整，不是待办。

### 不构成差距的三项（要明确写下来免得重复讨论）

1. **工具粒度**：hermix 15 个 vs ZCode 实际 1 个（`mcp__node_repl__js`）。ZCode 粒度粗不是落后，是把控制流放进代码里的必然结果——细粒度对"单步"场景反而更省。**不算差距**。
2. **平台覆盖**：ZCode `computerUseAvailability` 明确 `local-linux: supported=false`、所有 remote 形态 `supported=false`。hermix Windows-only 专注本地，**方向一致，不算差距**。
3. **UIA 深度**：`hermix-cua/uia.rs` 880 行 + `element.rs` 488 行的 UI Automation 覆盖，ZCode 开源侧看不到对应物（闭源）。**不能断言谁更强**，只能说 hermix 在可见范围内是完整的。

## Decision

按"改造成本 / 消除的风险"排序，两档可立项：

1. **差距 B（注入防护）** — 成本最低（一个签发层 + `cua.rs` 局部改），消除的是**真实攻击面**。这是我认为唯一应该立刻做的一项。
2. **差距 A（进程边界）** — 成本最高（要拆 crate + 做 broker 协议 + 状态机），但它是"电脑控制能否当成稳定能力"的前提。建议只做第一步：**把 CU 从 sidecar 内联挪出去**，哪怕先不做完整 broker。

第 3 项（token 成本）等 PTC 重启时一并解决，不单独动。

## Alternatives considered

- **照抄 ZCode 的 CU 架构（独立 broker + Helper + MCP）** — 否决：Helper 本体闭源，能抄的只有契约；且 hermix 的 CDP 浏览器面已经是独立进程，CU 独立化应复用那条思路而不是引入 node_repl 那一层。
- **补齐 token 成本差距（引入 heredoc 编排）** — 否决：PTC 上一轮已 rejected，理由（循环依赖 + ZCode 默认关闭）不变。
- **认为 hermix 全面落后、需重构** — 否决：工具面、UIA 深度、平台聚焦三项 hermix 不落后甚至更好。真差距只有安全边界两档。
- **认为无差距、CU 已够用** — 否决：`cua_ui_click` 接受模型编造的 `auto_id` 是可利用的注入链，且 CU panic 会打穿整个 sidecar——这不是"够用"，是"脆"。

## Consequences

- **获得**：一份按"可立项/暂缓/不构成差距"分档的差距清单，避免下一轮重复讨论工具粒度与平台覆盖。
- **放弃**：不在本篇定实施排期——排期需要先定 A 档是否拆 crate，属架构决策，另篇。
- **风险**：差距 B 的签发层若设计成"强校验"，模型在 UIA 控件无稳定 `auto_id` 时会频繁失败；需要在"拒绝"与"降级为坐标点击"之间明确定位——**这条要写进实施笔记，不能由实现者临场决定**。
- **前置**：`cua.rs` 风险分级现状（14 Dangerous / 1 Safe）需在实施前一并复核——`cua_screenshot` / `cua_list_windows` / `cua_ui_tree` 标 Dangerous 是否有依据，值得单独查一次。
