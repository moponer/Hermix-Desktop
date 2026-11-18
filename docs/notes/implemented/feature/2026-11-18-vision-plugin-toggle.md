# Decision Note: 识图插件开关（输入框上方的眼睛图标）

Status: implemented — 会话级开关，开=免费视觉链转文字，关=原生多模态，模型不支持时明确报错

## Decision

在输入框上方的工具栏加一个眼睛图标，切换本轮图片的处理方式：

| 状态 | 图片怎么走 | 对模型的要求 |
|---|---|---|
| **开** | 先经免费视觉链转成**文字描述**，再随正文发给模型 | 任意模型（含纯文本） |
| **关** | 作为 `ContentPart::ImageUrl` 直接进上下文 | **必须多模态** |

关着且模型**明确**不支持看图时，发送前直接报错并说明怎么办（开插件，或换模型）——
静默丢图会让用户以为发出去了，然后对着空气等一个看不懂的回答。

### 为什么是会话级而不是全局

不同对话可能同时在用不同模型：一个用纯文本模型读截图，另一个直接驱动多模态模型。
全局开关必然把其中一边设成错的。状态落盘在 `$HERMIX_HOME/vision-plugin.json`，
按 session id 分桶。

### 为什么复用当前模型当「眼睛」

免费渠道里已经有能看图的模型——`minimax/minimax-m3.1-flash:free` 实测
`supports_vision: true`，而且正是当前在用的那个。再要求用户单独配一个视觉
provider，等于把「开箱可用」变成「先去配 key」。

### 实现分层

| 层 | 文件 | 职责 |
|---|---|---|
| 状态 | `hermix-agent/src/vision_plugin.rs` | 会话级开关读写、模型能力判定 |
| API | `hermix-server/src/vision_api.rs` | `GET/POST /api/vision-plugin` |
| 装配 | `local_agent_service/mod.rs` | `apply_vision_plugin` 就地改写本轮 user 消息 |
| UI | `components/VisionToggle.tsx` | 眼睛图标 + tooltip + `aria-pressed` |

**关键约束**：`prepare_turn` 是同步函数，而识图要 `await` 一次模型调用。所以开关的
实际效果落在两个 `async` 调用方（`send_message` / `send_message_stream`）里，
由 `apply_vision_plugin` 就地替换 `messages` 的最后一条 user 消息。

### 一个刻意的判定：`None` 不等于「不支持」

`model_accepts_images(None) == true`。上游网关常常省略 `supports_vision` 字段，
前端替它拒绝会挡掉本来能用的配置。只有明确 `Some(false)` 才拒绝，其余交给 provider
报错——那个错误准确且可操作。

## Alternatives considered

- **全局开关**：见上，会把并发的两种用法之一设错。
- **把 vision-router 插件直接装进来**：那是 **DSH 的插件**（Node/npm 生态），
  Hermix 是 Rust + Tauri，两者插件体系不兼容。能力可以借鉴，代码不能复用。
- **另配一个专用视觉 provider**：多一层配置和 key，换来的是「更专业」，
  但免费渠道已有多模态模型，边际收益不抵成本。
- **关掉插件时静默忽略图片**：用户以为图发出去了，实际模型没收到。这是最糟的失败模式。
- **把识图做进 `prepare_turn`**：它是同步的，而识图必须 await。硬改签名会波及
  全部调用方；在 async 调用方里改写消息更小。

## Consequences

- 开关默认**关闭**（原生多模态优先）：不额外走一次网络调用，也不需要第三方视觉服务。
  用户主动 opt-in。
- 开启时每张图多一次模型往返（延迟增加，但纯文本模型因此能用了）。
- 视觉链失败不会中断这一轮：错误信息会作为文字拼进正文，模型和用户都能看到
  「第 N 张图识图失败——原因」，而不是整轮失败。
- `apply_vision_plugin` 只处理**本轮新追加**的那条 user 消息，历史里的旧图不动——
  否则每轮都会重新识图，成本和延迟都会失控。
- 前端 `VisionToggle` 做乐观更新（点击立刻变色），失败回滚；读不到状态时保持默认，
  绝不让一个开关挡住输入框。
