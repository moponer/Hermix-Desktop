# Decision Note: 动态工作流引擎立项（脚本工作流）

Status: proposed — 对齐 zai-org/ZCode `dynamic-workflow` 的脚本编排能力；含阶段一（零新地基）与阶段二（需嵌入 JS 运行时）两步走法与运行时选型建议，待拍板。

## 背景事实（读源码所得）

ZCode 的「工作流」不是"保存的提示词"，而是 agent 内核级能力：工作流 = **TypeScript 脚本**，跑在随包分发的 V8/Node 运行时里（安装目录里的 `v8_context_snapshot.bin` 是佐证）。工具族 `create-workflow / save-workflow / list-saved-workflows / get-workflow-run / resume-workflow-run / resolve-workflow-question / get-workflow-run-situation / get-workflow-run-roster` + `apps/zcode-cli/packages/bootstrap/src/app/dynamic-workflow-*`（run 派发/生命周期/日志/重放/进度/产物/血缘）构成完整引擎。脚本语言提供**确定性控制流**（分支/循环/并行/等人工答复），agent 是脚本内被调度的组件而非执行主体。

Hermix 现状（2026-10-04）：工作流 = 保存的 prompt + `{{参数}}` 占位符 + 再跑一次（`360b7028`），执行走统一 dispatch，运行落在 `workflow:{id}` 会话，**没有脚本编排层**。

## Decision（提案）

分两阶段，阶段一不引运行时先拿到编排价值，阶段二才评估脚本运行时。

**阶段一：阶段化编排（无运行时，agent 即执行器）**
- 工作流 prompt 升级为**阶段协议**：阶段清单、每阶段完成判据、进入下一阶段的条件、每轮先声明当前阶段。
- 运行 = `agent_service.send_message("workflow:{id}", prompt)`，与定时任务/IM 同路（铁律 DD），不新增执行器。
- 阶段状态落在会话消息里（结构化状态行）：日志/重放/续跑 = 重开该会话 + "从第 N 阶段继续"。
- 挂起等人工 = 复用现有 `clarify`；并行分支 = 复用内置 `delegate_task` 递归子代理。
- 代价：阶段判定由 LLM 执行，不确定性高于脚本——靠判据写死 + 状态可见兜底。

**阶段二：脚本运行时（需单独立项决策）**
- 工作流升级为可执行脚本。运行时选型（体积 / TS 支持 / 攻击面 / 维护活跃度）：

| 候选 | 体积 | TS | 攻击面 | 备注 |
|---|---|---|---|---|
| **boa**（纯 Rust） | 小（~几 MB） | 不支持（写 JS 或预编译降级） | 极小（无内建网络/fs） | 推荐起点 |
| quickjs | 极小（~1 MB） | 不支持 | 极小 | 更轻的备选 |
| deno_core | 大（数十 MB） | 原生 | 大（Deno API 带网络/fs） | 需严格裁剪权限 |
| 随包 node/v8（ZCode 路线） | 最大（~40 MB+） | 原生 | 中（需进程隔离） | 一致性最好、分发与升级成本高 |

- 倾向 boa/quickjs + `esbuild` 预编译 TS，runtime 侧**不暴露网络与任意 fs**；工具调用走现有 ToolRegistry 白名单 + 目录范围（对齐现有 approval / permission_profile）。
- run 日志/血缘/重放：会话持久化天然承担（一次运行 = 一条会话 + 阶段/脚本产物行）。

## Alternatives considered

- **维持现状（保存 prompt + 参数）** — 否决：无编排，"工作流"退化为"带变量的定时 prompt"，与 ZCode 同名不同物，长期会让用户在两者间误判。
- **直接移植 ZCode 的 TS 引擎** — 否决：依赖随包 Node/V8 分发、体积与升级成本高，且其沙箱模型是为 Node 运行时设计的，直接嵌入 Rust agent 属架构移植而非功能。
- **让模型写 Rust 插件** — 否决：需要编译链，交互式场景不可用。

## Consequences

- 阶段一：零新依赖、零新信任面，一次迭代可交付；代价是阶段推进带 LLM 不确定性。
- 阶段二：**沙箱是新的信任面**——逃逸、资源耗尽、脚本版本兼容都要从零设计；引入 runtime 意味着包体 + 安全审计 + 脚本调试工具链三块长期投入。
- 两阶段都要求工作流 run 与会话强绑定（否则日志/重放无处落地）。
- 建议评审顺序：阶段一先上（本次之后即可），阶段二在阶段一被真实使用暴露"需要脚本级控制流"后再立项。
