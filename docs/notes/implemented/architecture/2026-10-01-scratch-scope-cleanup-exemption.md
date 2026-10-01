# Decision Note: 临时文件自清理的审批豁免（scratch 范围降级）

Status: implemented — 2026-10-01 落地并通过 11 项 storage_clean 测试

## Decision

给 `storage_clean` 的 Dangerous 审批加一条**范围化豁免**：当请求的**每一个**路径都落在代码定义的 `%TEMP%\hermix-scratch\` 内时，跳过审批卡直接执行；只要有任何一个路径在外面，整次调用仍走完整 Dangerous 流程。

三层配合：

1. **代码层** — `storage_clean::scratch_root()` 定义唯一合法的临时文件根。这个路径由程序决定，模型无法通过"取个好名字"骗过它。
2. **风险层** — `is_scratch_scoped(&paths)` 做全有或全无判定，跑在 `local_agent_service::decide_pre_tool_dispatch` 的 Dangerous 闸门里，仅对 `storage_clean` 生效。
3. **提示词层** — 改写「临时文件自清理」块：要求临时文件一律写进 `hermix-scratch\`，收尾时**直接清理、不要问用户**；创建时一句话声明。

关键约束：豁免降级的是**弹窗**，不是工具本身。`storage_clean` 内部的 `safety_check`（canonicalize 防穿越、黑名单优先、safelist 白名单）照常运行，仍可逐条拒绝。

## Alternatives considered

- **自动删除所有会话产生的文件**（无路径约束）——否决。`storage_clean` 的 safelist 覆盖整个 `%TEMP%` 和 `Downloads`，无条件自动删等于让"删除需批准"这条边界出现一个巨大的口子，而"这个文件是临时的"这个判断由模型做出。
- **靠提示词约定前缀**（让模型自己把文件命名为 `hermix_*`）——否决。前缀由模型控制，它可以给任何文件套上这个前缀，豁免就退化成了"模型说啥就是啥"。前缀必须由代码生成。
- **把 `storage_clean` 整体降级为 Caution** ——否决。它的正常用途（清理 Downloads 里的安装包、整个 `%TEMP%` 的陈年垃圾）确实需要人确认。降级必须是范围化的，不是工具级的。
- **加一个独立的 `scratch_clean` 工具**（注册为 Safe）——否决，收益不抵成本：新工具要写 schema、注册、UI 描述、前端摘要，而 `storage_clean` 的安全校验已经完备，缺的只是"风险等级按参数动态判定"这一环。`ToolRegistry` 的 `risk_level` 是静态字段（`registry/validation.rs:112`），为动态判定改架构超出本次范围；在闸门里按工具名+参数判定是同一效果的小改动。

## Consequences

- **收益**：agent 收尾清理自己造的垃圾不再需要问你。原先"提示词要求清理 + 审批会拦"这个矛盾解开——你在测试里拒绝一次删除，模型就学成"永远问"，scratch 目录会无限堆积。
- **放弃**：`%TEMP%` 根目录下的散落文件（不在 `hermix-scratch\` 里）仍然逐次弹审批。这是刻意的——那可能是别的程序或用户自己的文件，模型无权替你判断。
- **放弃**：豁免只覆盖 `storage_clean`。`terminal` 里的 `del` / `rm` 不享受豁免——shell 能删任何东西，与路径无关。
- **风险**：如果模型把用户的真实文件"搬进" scratch 目录再删，豁免会放行。缓解：模型没有动机这么做（提示词要求写临时文件时就地写），且 `scratch_check` 的黑名单仍然覆盖其下的系统目录。
- **后续**：若将来 registry 支持按参数动态风险判定（`register_with_dynamic_risk`），应把这段逻辑下沉到注册表，闸门只保留通用逻辑。

## Reintroduction conditions

本条属于 simplification 之外的安全边界调整，但同样记录回退条件：

- 若出现"agent 为绕开审批而把用户文件复制进 scratch 再删"的实例 → 撤回豁免，改回纯提示词方案。
- 若 `scratch_root()` 被证明可被外部进程写入并植入符号链接/junction 实现逃逸 → 撤回，改用 Windows ACL 限制该目录仅本进程可写（依赖 `WRITE_RESTRICTED` 沙箱，见 `SANDBOX_WINDOWS_ACL_DESIGN`）。
