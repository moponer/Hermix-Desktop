# Decision Note: 陈旧 sidecar 让复测结论整体失效 + 三个 bridge/vision 真缺陷

Status: implemented — 复测跑的是修复前 4 小时的二进制；本轮修 `tool_call` 参数平铺、子 agent 视觉死锁、bridge 可见性，并加 sidecar 新鲜度门禁

## 背景

[2026-10-05 那轮修复](../implemented/bug-fix/2026-10-05-dead-tool-registration-and-tool-result-truthfulness.md)
落地后，主人拿一份 v2.0 缺陷报告回来：12 项里只修好 3 项，BUG-02 / 04 / 05 / 08
仍然未修复，其中 BUG-02 被标为"最高优先级、唯一改一处解锁三个工具的杠杆点"。

**这份报告的复测结论不成立。** 测试跑的二进制比修复提交早 4 小时：

| 事实 | 值 |
|---|---|
| `release/target/release/hermix.exe` 构建时间 | 2026-10-05 18:38 |
| `binaries/hermix-x86_64-pc-windows-msvc.exe` | 2026-10-05 18:38（同一次构建） |
| `f4820122`（BUG-02/03/04/05/08/09 的修复） | 2026-10-05 22:42 |
| `664123ca`（BUG-01 的修复） | 2026-10-05 23:16 |
| `Hermix Desktop_0.1.9_x64-setup.exe` 打包时间 | 2026-10-06 16:25 |

0.1.9 安装包是修复之后打的，但 `release/target/release/hermix.exe` 从 18:38 起就没再
动过——`cargo tauri build` 不会刷新 sidecar，打包时直接把旧的塞了进去。判据用
`f4820122` 独有的运行时文案（修复前源码里不存在"我理解为：〈完整选项〉"）：两个
二进制都不含该串，即**都是修复前的构建**。

所以"仍未修复"是旧二进制的回声，不是修复失败。

但把 12 条逐条对源码重走之后，挖出**三个上一轮漏掉的真缺陷**——它们的症状恰好
是报告里"未修复"那几条描述的东西，只是根因在别处。

## Decision

**先证明测试环境无效，再逐条对源码验证，最后只修源码里仍然成立的部分。**
不照单全收，也不因为"报告错了"就整体否定——报告的**现象**是真的，只是**归因**错了。

本轮实施三项：

1. **`tool_call` 参数平铺兜底**（`tool_search.rs`）。新增 `extract_arguments`：
   `arguments` 存在且是对象时原样用；是 JSON 字符串时解析；缺失或形态不对时，
   把顶层除 `name` 外的所有键当作目标工具的参数。**兜底只在 `arguments` 缺失时
   触发**，合规调用一个字节都不变，`name` / `arguments` 也绝不会漏进目标参数。

2. **子 agent 视觉死锁**（`run_loop.rs` / `tool_result_processor.rs` /
   `agent_config.rs` / `sub_agent_orchestrator.rs`）。新增 `AgentConfig::is_sub_agent`，
   由 `build_child_config` 置位。子 agent 调 `read_image` 时：跳过 delegate_task nudge，
   并用新增的 `extract_image_payload` 把 base64 重新注入成真正的
   `Message::user_with_images` 多模态块。注入点在**全部 tool_result 写回之后**——
   一条 `user` 消息插在 assistant 的 `tool_calls` 和它的 `tool` 结果之间，会破坏
   `provider.rs::enforce_strict_tool_pairing` 保护的那条严格配对（MiniMax-M3 直接
   400 `(2013)`）。

3. **bridge 三件套可见性**（`tool_search.rs` / `deferred_loader.rs`）。
   `assemble_tool_schemas*` 原来先把已有 bridge 工具过滤掉，只在 activated 分支加回来；
   低于阈值时它们从 schema 列表里彻底消失，而 `directions.rs` 仍在宣传。模型看得见
   调不通。改成拆出来、低于阈值时原样还回去。

外加 **`scripts/check-sidecar-freshness.sh`**（软门禁，接入 ci-gate 第 12 关）：
任何 Rust 源码比 sidecar 新就报 STALE 并打出重建命令。

## Alternatives considered

**否决一：把 `name` / `arguments` 无条件合并到目标参数顶层。** 这正是上一轮笔记
「否决一」否决的方案，理由（会往所有正常调用的参数里注入两个垃圾键）成立，本轮不动。
本轮做的是**有条件的兜底**：`arguments` 在场就不碰它。两者不是一回事，上一轮把它们
混为一谈，导致真实的平铺形态一直没人修。

**否决二：给子 agent 也注入 delegate_task nudge，只是不剥离 base64。** 半修。nudge
的文本是"你无法在主上下文中看到图片内容，必须用 delegate_task"，对子 agent 这句话
本身是错的：它就是那个隔离上下文，而且 `delegate_task` 被
`BLOCKED_TOOLS_FOR_SUBAGENT` 从它的 registry 里删了，指令无法执行。留着 nudge 等于
留一个死胡同。

**否决三：在主 agent 侧也注入多模态块，让 read_image 直接可用。** 更简单，但违反
AGENTS.md「图片必须走子 agent 隔离」的铁律。主上下文每轮都要为滞留的 base64 付费，
剥离 + 委托的模型不变。

**否决四：改 `[ref=eN]` 的选择器生成，改成 CDP `backendNodeId` 直连。** 这是
BUG-04/05 的真正解（`ax_node_selector` 从 accessible name 反推 CSS 属性，对
accessible name 来自 `<label>` 的输入框必然匹配不到）。但它要动 `cdp_command` 的
会话模型（每次调用新开 WS，`objectId` 跨会话失效），是一次结构性改动，不该塞进
一个"核验报告"的批次。当前形态是有意的失败模式切换——可见的 `type_failed` 优于
静默命中错误节点——留给独立立项。

> **2026-10-06 晚间推翻，已实施（`7189de80`）**：「不该塞进一个核验批次」是对批次
> 边界的误判——同一个报告里的同一个缺陷，拖着不修等于没修。实际改动比预想小：
> `ws_roundtrip` 抽出单次往返，`call_on_node` 在同一个 socket 上顺序跑
> resolve + callOnFunctionOn，没有引入新的会话抽象。改完 GUI 表单自动化才真正
> 可用：`<label for>` 关联的输入框现在按 CDP 节点身份命中，不再靠字符串猜测。
>
> **教训**：「需要结构性改动」和「不该现在做」之间没有等号。当时真正该做的是
> 估工作量（半小时）而不是给它贴"独立立项"的标签——那是我给自己找的台阶，
> 和报告里那个"说计划不动手"的行为缺陷是同一个毛病。

**否决五：拆 BUG-12 的审批护栏。** `Remove-Item -Recurse -Force` 被拒是
`DENIED_PATTERNS` 里钉住的硬护栏，`storage_clean` 是指定替代路径，scratch 范围
（`local_agent_service.rs:1543-1559`）本来就跳过审批。600s 超时可通过
`set_approval_timeout_secs` 调。拆它等于给递归强删开绿灯。报告里 BUG-12 的
`必须要有 confirm: true` 其实是缺陷 1 的症状——参数根本没送到
`storage_clean`，它当然看不到 `confirm`。

## Consequences

- **`[ref=eN]` 不再依赖名字猜测**（`7189de80`）。ref 走 CDP 节点身份，
  `<label for>` 关联的输入框能正常命中。代价是每个 ref 动作要多开一次
  WebSocket（一次 `/json` 探测 + 一次连接），比原来的单次 `Runtime.evaluate`
  多一个往返；换来的是点击/输入不会再打到语义无关的元素上。
- **没有 `backendDOMNodeId` 的节点在快照里标 `not clickable` 且不给 ref。**
  这类节点（CDP 合成的少数节点）此前会拿到一个解析必死的句柄。模型看到的是
  "这个控件存在但点不了"，比拿到一个假句柄后反复重试要好。
- **子 agent 上下文会短暂持有图片 base64。** 这正是它被设计来承受的成本——它的
  上下文随它一起销毁。多张图一次性注入时仍受 `spill` 与压缩阈值约束。
- **sidecar 门禁是软的。** `git clone` 后所有文件 mtime 相同，会白过——它是本机
  出包前的绊线，不是 CI 正确性检查。真要硬保证得把 git sha 编译进二进制并在启动时
  校验，那是另一件事。
- **上一轮笔记的「否决一」「否决四」都已被本篇推翻/部分推翻**（tool_call 兜底已补、
  BUG-03 不再算"有意设计"、ref 直连已实施）。按制度在同一批里更正了那篇的事实地
  述，而不是让两篇打架。
