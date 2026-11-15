# Decision Note: 收敛硬停不再把「只有思考」当空转

Status: implemented — `assistant_produced_output` 取代「只认 content」的可见性判据，收敛硬停同时留下用户可见的收尾说明。

## Decision

工具轮的收敛判定改用 `message_helpers::assistant_produced_output`：本轮**有正文**（剥掉 `<think>` 块后非空）**或有 `reasoning_content`** 都算「有输出」。此前只认 `content`，thinking 模型（MiniMax-M3.1-Flash 等）把推演写进 `reasoning_content`、正文留空，于是每一轮都被记成「只调工具、什么都不说」，累计 18 轮触发 `NoProgressWithTools` 硬停（2026-11-05 实测：超星课程目录探查跑到第 22 轮被砍，用户界面上只是「正在回复…」定格，用户须手动发「继续」才知道发生了什么）。

「真空转」不再由「没写正文」判定，而由 `RepeatGuard`（同一批工具结果指纹逐字重复 2 轮 → 本轮请求不再携带工具 schema）与 `ErrorLoop`（连续 3 轮工具全部失败）判定——这两条都是「零新信息」的硬证据，与模型有没有写正文无关。

硬停分支同时补一条 `Message::assistant` 收尾文本（仅在本轮没有任何正文时），说明停止原因与如何继续。此前该分支直接 `finalize_agent_result`，最后一条 assistant 正文为空，界面没有任何解释。

轮次上限的口径同时统一：`max_turns: 0` 在 agent loop 里本来表示「不设上限」（`run_loop.rs` 的 `max_turns != 0 && total_turns >= max_turns`，以及 `empty_response_handler` 的全部三处判定），但 `hermix-config` 的 `validate_config` 把 0 判为配置错误、`GatewayConfig::validate` 把它渲染成「agent will be unable to complete any task」。结果是「跑到做完为止」这一合法设置无法通过配置文件表达。两处反口径现在都改成接受 0 = 不限。

## Alternatives considered

- **只提高阈值（18 → 40）**：把误杀往后推，长任务照样被杀，且真卡住的轮次成本更高。否决。
- **完全删掉 `NoProgressWithTools` 两条计数**：丢掉了「模型连思考都不产出、却一直调工具」这一极窄情形的保护。保留规则、只改「什么算输出」的口径，代价更小。
- **按「工具调用参数是否重复」判定卡死**：比结果指纹更早，但同一个只读调用在长链里有合法的重复（先看一眼、后面再确认），误报面更大。现有的结果指纹足以覆盖。
- **硬停后让模型再写一段总结（复用 `handle_max_iterations` 的收尾调用）**：多一次完整 API 往返，而硬停的语义正是「模型已经不听话了」；本地生成一句话更便宜也更确定。

## Consequences

- thinking 模型的长工具链任务不再在 18 轮处被砍；代价是「模型只思考不汇报」这条弱信号在 thinking 模型上实际失效，防空转完全押在 `RepeatGuard` 与 `ErrorLoop` 上。这两条都是硬信号，且 `RepeatGuard` 命中后的处置是「禁用后续工具逼收尾」，不是杀 run。
- 下一道墙是 `max_turns`（用户 config 现为 30）：轮数耗尽走 `handle_max_iterations`，由模型产出总结，用户看得到结论而不是静默停止。
- 硬停的收尾文本是本地文案，不是模型输出；它会作为一条 assistant 消息进入会话历史，重开会话仍可见。
