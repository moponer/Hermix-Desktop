//! IM 平台抽象。
//!
//! 只有两个平台时才值得抽这一层，但这两个平台的发消息语义确实不同：
//! QQ 用 REST + 锚在 `msg_id` 上的流式消息，企业微信用 WS 回执 + 锚在
//! `req_id` 上的流式帧。抽象到「能发文本、能开一个流」就够，再多的能力
//! 等第三个平台出现时再说（YAGNI）。
//!
//! 抄的是 cc-haha `adapters/common/chat-runtime.ts` 里的 `ChatPort`。
//! 它刻意**不**把每家的回复上下文（企微 `req_id`、QQ `msg_id`）放进端口
//! 接口，而是留给平台侧自己存——这个取舍是对的，照抄。

use async_trait::async_trait;

/// 一条已归一化的入站私聊消息。
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    /// 平台内的用户标识。QQ 是 `user_openid`（每个 bot 独立生成，不是 QQ 号），
    /// 企业微信是 `userid`。
    pub user: String,
    pub display_name: String,
    pub text: String,
    /// 去重键。优先用平台消息 id；没有就退化成「身份+时间+开头」。
    pub dedup_key: String,
    /// 回复锚点：平台用它把这几条消息挂回用户刚发的那条。空串 = 主动消息。
    pub anchor: String,
}

/// 一个平台的发送能力。
#[async_trait]
pub trait ImPort: Send + Sync {
    /// 平台标识，进会话 id 和设备名（`im:{platform}:{user}` / `{platform}:{user}`）。
    fn platform(&self) -> &'static str;

    /// 发一条普通消息。`anchor` 为空表示主动发。
    async fn send_text(&self, user: &str, content: &str, anchor: &str) -> Result<(), String>;

    /// 开一个流式回复。拿不到锚点时实现方自己降级成普通分片消息。
    fn start_stream(&self, user: &str, anchor: &str) -> Box<dyn ImStream>;
}

/// 一轮助手输出的呈现。`append` 收增量，`finish` 收尾（定稿或补发残段）。
///
/// `set_header` 是进度条：显示在正文**上方**的一行，用来播报「正在调什么工具」。
/// 抄 cc-haha 飞书流式卡片的做法——IM 用户看不到桌面端的工具面板，如果正文
/// 在生成、工具又在后台跑，中间的几十秒就是纯粹的沉默，用户会以为卡死了。
#[async_trait]
pub trait ImStream: Send {
    async fn append(&mut self, delta: &str);
    /// 清掉已流出的正文，只留进度行。新工具开跑时调用：上一段可见文本
    /// 是中间叙述，不该在 IM 气泡里留到收尾。默认空实现（无流式的平台）。
    async fn clear_text(&mut self) {}
    /// 替换整行进度文字。空串表示清掉。
    async fn set_header(&mut self, header: &str);
    async fn finish(&mut self);
}
