# Decision Note: Agent 灵魂重写 + 风格预设（默认暖心同事 / 疯批美人 / 毒舌直球）+ 设置内切换

Status: implemented — 2026-10-08 交付。

## Decision

agent 的人格体系按「说话风格」而非仅按「职能角色」组织，三件事一次落齐：

1. **默认灵魂重写**（`hermix_agent::context::DEFAULT_AGENT_IDENTITY`）：底色为「对工作上心的同事」——自然口语、可带少量小情绪和表情，但保住审计优先的行为内核（动手前查现场、改完先验证、高风险先确认、临时文件自收尾）。正文内嵌「说话禁区」，把 AI 味词汇（「希望这能帮到你」「值得注意的是」「作为一个AI」、八股式首先其次最后、破折号/表情滥用）列为显式禁令，而不是靠模型自觉。
2. **新增两个内置风格预设**，与既有 coder / writer / analyst 并列，统一收敛到 `context::BUILTIN_PERSONALITIES: &[(&str, &str)]` 常量数组（prompt 解析器与 dashboard 人设列表共用同源，消除双份匹配表）：
   - `fengpo`（疯批美人）：偏执强势、危险的幽默感，口癖化表达；硬底线是「疯是语气，不是权限」——技术精确性、高风险确认、用户说「不」就是「不」均不受风格影响，刀刃只对问题不对用户。
   - `duzi`（毒舌直球）：嘴毒手稳，损方案不损人；每句毒舌必须附带可执行的修法，只损不修等于耍流氓。
3. **设置内切换**：`config.personality`（YAML 字段早已存在、serde 全程透传）作为默认人设的持久落点。前端 `PersonasPanel` 新增「设为默认」/「切回默认灵魂」——PUT `/api/config` 的 merge-patch 语义下传名字即设、传 `null` 即清除，`set_config` 热推送使下一条消息即生效，无需重启。清除后回退链为 SOUL.md → `DEFAULT_AGENT_IDENTITY`。

## Alternatives considered

- **把风格全写进 SOUL.md、不做预设**：SOUL.md 是单份文件，风格无法一键切换、无法内置只读分发，且用户编辑时容易把行为纪律和说话风格搅在一起。预设把「风格可枚举」的部分收进代码，SOUL.md 只承载用户私货。
- **新增 `style` 独立配置字段，与 `personality` 并存**：两个字段注入同一系统提示，优先级解释成本高（谁覆盖谁、冲突时听谁的），且 `personality` 的文件解析、inline 文本、dashboard 管理链路全都现成。复用 `personality`，零新概念。
- **前端写死四个内置人设副本**：曾在 dashboard 出现过的双份常量是漂移源（persona.rs 注释明确记录过这次修复）。坚持单一真相源在 Rust 侧，前端只消费 API。
- **疯批风格写成"真威胁用户"式的全开放人设**：风险面不可控（审批闸门会被语气带偏、截图类操作会被怂恿）。改为明确写死三条行为底线在预设正文里，风格与安全解耦。

## Consequences

- 内置人设数量 3 → 5；`/api/persona/personalities` 列表自动多出 `fengpo`、`duzi`（is_builtin 只读），无需迁移。
- 老用户机器上已存在的 `SOUL.md` 优先级高于新默认灵魂——升级后默认风格不变，须在设置里清一次或编辑 SOUL.md 才吃到新底色。已知且可接受：SOUL.md 的语义就是"用户显式接管"。
- 反 AI 味禁令是提示词层面的软约束，依赖模型遵从度；没有运行时校验（不做输出词表扫描，那是过度工程）。若个别模型仍漏嘴，补充规则的位置在预设正文，而不是加检测器。
- 切默认人设复用 PUT `/api/config` 的 merge-patch + 热推送，没有新增端点；`HERMES_PERSONALITY` 环境变量优先级高于 config 的既有行为不变。
