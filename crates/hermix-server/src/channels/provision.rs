//! IM 官方机器人的扫码绑定。
//!
//! 两个平台的绑定都是**厂商自己提供的 HTTP 接口**，不是要逆向的私有协议：
//! 后端起一个绑定任务拿到一个授权 URL，用户拿手机扫这个 URL 确认，轮询任务
//! 状态就能取回机器人的凭据。抄的是 cc-haha `adapters/{qq,wecom}/qr-auth.ts`
//! 所对接的那几个接口。
//!
//! QQ（`q.qq.com`）：
//! 1. `POST /lite/create_bind_task {key}` → `{data:{task_id}}`，`key` 是 32 字节
//!    随机数的 base64，后面解密用。
//! 2. 用户扫 `https://q.qq.com/qqbot/openclaw/connect.html?task_id=…`。
//! 3. `POST /lite/poll_bind_result {task_id}` → `{data:{status, bot_appid,
//!    bot_encrypt_secret}}`。那个 secret 是用第 1 步的 key 做 AES-256-GCM
//!    加密的，密文布局 `[iv 12][ciphertext][tag 16]`。
//!
//! 企业微信（`work.weixin.qq.com`）：两个 GET，没有加密。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand::RngCore;
use serde::Serialize;
use serde_json::Value;

const QQ_HOST: &str = "https://q.qq.com";
const WECOM_HOST: &str = "https://work.weixin.qq.com";

/// 扫码会话有效期。两个厂商的码都是分钟级过期，这里取最宽松的那个。
const SESSION_TTL: Duration = Duration::from_secs(600);

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// QQ 绑定任务状态。数值与官方 SDK 的 `BindStatus` 一致。
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(i64)]
enum QqStatus {
    None = 0,
    Pending = 1,
    Completed = 2,
    Expired = 3,
}

impl QqStatus {
    fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Pending,
            2 => Self::Completed,
            3 => Self::Expired,
            _ => Self::None,
        }
    }
}

/// 一个平台的一组凭据。QQ 是 app_id/app_secret，企业微信是 bot_id/secret。
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct ImCredentials {
    pub primary: String,
    pub secondary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BindPoll {
    /// 还在等用户扫码。
    Waiting {
        #[serde(skip_serializing_if = "Option::is_none")]
        verification_url: Option<String>,
        message: String,
    },
    /// 绑定成功，凭据已落盘。
    Connected { credentials: ImCredentials },
    Expired { message: String },
    Failed { message: String },
    NotStarted { message: String },
}

// Clone 会顺带复制一份 AES 密钥到 poll 的栈上。它本来就只活在进程内存里，
// 复制不扩大暴露面，省掉为此引一个 zeroize 依赖。
#[derive(Debug, Clone)]
struct BindSession {
    platform: &'static str,
    started_at: Instant,
    /// QQ：授权 URL + 解密 key + task_id。
    qq: Option<QqSession>,
    /// 企业微信：scode + 授权 URL。
    wecom: Option<WecomSession>,
    /// 轮询时凭据已经取回就缓存下来，重复轮询返回同一结果。
    settled: Option<BindPoll>,
}

#[derive(Debug, Clone)]
struct QqSession {
    task_id: String,
    verification_url: String,
    /// AES 密钥（32 字节原始值，不含 base64 包装）。
    key: [u8; 32],
}

#[derive(Debug, Clone)]
struct WecomSession {
    scode: String,
    verification_url: String,
}

/// 进程内的扫码会话表。一个平台同时只有一个进行中的绑定——设置页一次只
/// 显示一张码，留着上一次不取消只会白跑到厂商超时。
pub struct BindSessions {
    inner: Mutex<HashMap<String, BindSession>>,
}

impl Default for BindSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl BindSessions {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn purge(&self) {
        let mut map = self.inner.lock().expect("bind sessions lock");
        map.retain(|_, s| s.started_at.elapsed() < SESSION_TTL);
    }

    /// 这个 session_key 属于哪个平台。
    pub fn platform_of(&self, session_key: &str) -> Option<&'static str> {
        self.inner
            .lock()
            .expect("bind sessions lock")
            .get(session_key)
            .map(|s| s.platform)
    }

    /// 只保留一个进行中的绑定。
    fn clear_platform(&self, platform: &str) {
        let mut map = self.inner.lock().expect("bind sessions lock");
        map.retain(|_, s| s.platform != platform);
    }

    fn new_key() -> String {
        format!("bind_{}", uuid::Uuid::new_v4().simple())
    }
}

fn err(msg: impl Into<String>) -> BindPoll {
    BindPoll::Failed {
        message: msg.into(),
    }
}

/// 拉起一次扫码绑定，返回 session_key 和要渲染成二维码的 URL。
pub async fn start(
    sessions: &BindSessions,
    platform: &str,
    source: &str,
) -> Result<(String, String), String> {
    sessions.purge();
    // 一次只留一个进行中的绑定：设置页同时只显示一张码，留着上一次不取消
    // 只会白跑到厂商超时。
    sessions.clear_platform(platform);
    let (session_key, url, session) = match platform {
        "qq" => {
            let (qq, url) = start_qq(source).await?;
            (
                BindSessions::new_key(),
                url,
                BindSession {
                    platform: "qq",
                    started_at: Instant::now(),
                    qq: Some(qq),
                    wecom: None,
                    settled: None,
                },
            )
        }
        "wecom" => {
            let (wc, url) = start_wecom(source).await?;
            (
                BindSessions::new_key(),
                url,
                BindSession {
                    platform: "wecom",
                    started_at: Instant::now(),
                    qq: None,
                    wecom: Some(wc),
                    settled: None,
                },
            )
        }
        other => return Err(format!("未知的 IM 平台：{other}")),
    };
    sessions
        .inner
        .lock()
        .expect("bind sessions lock")
        .insert(session_key.clone(), session);
    Ok((session_key, url))
}

async fn start_qq(source: &str) -> Result<(QqSession, String), String> {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);

    let body: Value = http()
        .post(format!("{QQ_HOST}/lite/create_bind_task"))
        .json(&serde_json::json!({ "key": key_b64 }))
        .send()
        .await
        .map_err(|e| format!("创建绑定任务失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("解析绑定任务响应失败：{e}"))?;
    if body.get("retcode").and_then(Value::as_i64) != Some(0) {
        return Err(format!(
            "创建绑定任务被拒：{}",
            body.get("msg").and_then(Value::as_str).unwrap_or("未知错误")
        ));
    }
    let task_id = body
        .pointer("/data/task_id")
        .and_then(Value::as_str)
        .ok_or("绑定任务响应缺少 task_id")?
        .to_string();
    let url = format!(
        "{QQ_HOST}/qqbot/openclaw/connect.html?task_id={}&source={}&_wv=2",
        urlencode(&task_id),
        urlencode(source)
    );
    Ok((
        QqSession {
            task_id,
            verification_url: url.clone(),
            key,
        },
        url,
    ))
}

async fn start_wecom(source: &str) -> Result<(WecomSession, String), String> {
    let body: Value = http()
        .get(format!("{WECOM_HOST}/ai/qc/generate"))
        .query(&[("source", source), ("plat", &platform_code().to_string())])
        .send()
        .await
        .map_err(|e| format!("企业微信生成二维码失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("解析企业微信二维码响应失败：{e}"))?;
    let scode = body
        .pointer("/data/scode")
        .and_then(Value::as_str)
        .ok_or("企业微信响应缺少 scode")?
        .to_string();
    let raw = body
        .pointer("/data/auth_url")
        .and_then(Value::as_str)
        .ok_or("企业微信响应缺少 auth_url")?;
    // 只接受厂商自己的 HTTPS 域名。授权 URL 是要被渲染成二维码给人扫的，
    // 厂商回什么就显示什么等于开一个任意外链的展示位。
    let verification_url = safe_wecom_url(raw).ok_or("企业微信返回的授权地址不合法")?;
    Ok((
        WecomSession {
            scode,
            verification_url: verification_url.clone(),
        },
        verification_url,
    ))
}

/// 只放行 `https://work.weixin.qq.com[:443]` 且不含 userinfo 的地址。
fn safe_wecom_url(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("https://")?;
    // 有 userinfo 就直接拒（`https://user:pw@host/`）。
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    if host != "work.weixin.qq.com" {
        return None;
    }
    if let Some(p) = port {
        if p != "443" {
            return None;
        }
    }
    Some(raw.to_string())
}

fn platform_code() -> u8 {
    if cfg!(target_os = "windows") {
        2
    } else if cfg!(target_os = "linux") {
        3
    } else {
        1
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 轮询一次。首次成功后会缓存结果，重复轮询返回同一份。
pub async fn poll(
    sessions: &BindSessions,
    platform: &str,
    session_key: &str,
) -> BindPoll {
    sessions.purge();
    let snapshot = {
        let map = sessions.inner.lock().expect("bind sessions lock");
        match map.get(session_key) {
            None => {
                return BindPoll::NotStarted {
                    message: "当前没有进行中的绑定，请重新生成二维码。".to_string(),
                }
            }
            Some(s) => s.clone(),
        }
    };
    if let Some(done) = &snapshot.settled {
        return done.clone();
    }

    let result = match (platform, &snapshot.qq, &snapshot.wecom) {
        ("qq", Some(qq), _) => poll_qq(qq).await,
        ("wecom", _, Some(wc)) => poll_wecom(wc).await,
        _ => err("绑定会话与平台不匹配"),
    };

    // 终态（成功/过期/失败）写回缓存，避免重复打厂商接口。
    let terminal = !matches!(result, BindPoll::Waiting { .. });
    let mut map = sessions.inner.lock().expect("bind sessions lock");
    if let Some(s) = map.get_mut(session_key) {
        if terminal {
            s.settled = Some(result.clone());
        } else if let (Some(qq), BindPoll::Waiting { verification_url, .. }) =
            (s.qq.as_mut(), &result)
        {
            if let Some(u) = verification_url {
                qq.verification_url = u.clone();
            }
        }
    }
    result
}

async fn poll_qq(s: &QqSession) -> BindPoll {
    let resp = match http()
        .post(format!("{QQ_HOST}/lite/poll_bind_result"))
        .json(&serde_json::json!({ "task_id": s.task_id }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(format!("查询绑定状态失败：{e}")),
    };
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return err(format!("解析绑定状态失败：{e}")),
    };
    if body.get("retcode").and_then(Value::as_i64) != Some(0) {
        return err(format!(
            "查询被拒：{}",
            body.get("msg").and_then(Value::as_str).unwrap_or("未知错误")
        ));
    }
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    match QqStatus::from_i64(data.get("status").and_then(Value::as_i64).unwrap_or(0)) {
        QqStatus::Pending => BindPoll::Waiting {
            verification_url: None,
            message: "等待 QQ 扫码确认…".to_string(),
        },
        QqStatus::Expired => BindPoll::Expired {
            message: "二维码已过期，请重新生成。".to_string(),
        },
        QqStatus::None => BindPoll::Expired {
            message: "绑定任务已失效，请重新生成二维码。".to_string(),
        },
        QqStatus::Completed => {
            let app_id = data.get("bot_appid").and_then(Value::as_str).unwrap_or("").to_string();
            let encrypted = data
                .get("bot_encrypt_secret")
                .and_then(Value::as_str)
                .unwrap_or("");
            if app_id.is_empty() || encrypted.is_empty() {
                return err("QQ 返回的机器人凭据不完整，请重新扫码。");
            }
            match decrypt_qq_secret(&s.key, encrypted) {
                Ok(secret) => BindPoll::Connected {
                    credentials: ImCredentials {
                        primary: app_id,
                        secondary: secret,
                    },
                },
                Err(e) => err(format!("解密 QQ 机器人凭据失败：{e}")),
            }
        }
    }
}

async fn poll_wecom(s: &WecomSession) -> BindPoll {
    let resp = match http()
        .get(format!("{WECOM_HOST}/ai/qc/query_result"))
        .query(&[("scode", &s.scode)])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(format!("查询企业微信绑定状态失败：{e}")),
    };
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return err(format!("解析企业微信绑定状态失败：{e}")),
    };
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    let status = data
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    match status.as_str() {
        "success" => {
            let bot_id = data
                .pointer("/bot_info/botid")
                .and_then(Value::as_str)
                .unwrap_or("");
            let secret = data
                .pointer("/bot_info/secret")
                .and_then(Value::as_str)
                .unwrap_or("");
            if bot_id.is_empty() || secret.is_empty() {
                return err("企业微信返回的机器人凭据不完整，请重新扫码。");
            }
            BindPoll::Connected {
                credentials: ImCredentials {
                    primary: bot_id.to_string(),
                    secondary: secret.to_string(),
                },
            }
        }
        "expired" | "timeout" => BindPoll::Expired {
            message: "二维码已过期，请重新生成。".to_string(),
        },
        "fail" | "failed" | "error" => BindPoll::Failed {
            message: "企业微信授权失败，请重新扫码。".to_string(),
        },
        _ => BindPoll::Waiting {
            verification_url: None,
            message: "等待企业微信扫码确认…".to_string(),
        },
    }
}

/// 解 QQ 的 `bot_encrypt_secret`。
///
/// 密文是 AES-256-GCM，base64 编码，布局 `[iv 12][ciphertext][tag 16]`；
/// 密钥就是建任务时那个 32 字节的 `key`。
fn decrypt_qq_secret(key: &[u8; 32], encrypted_b64: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64)
        .map_err(|e| format!("密文不是合法 base64：{e}"))?;
    if raw.len() < 12 + 16 {
        return Err(format!("密文长度异常（{} 字节）", raw.len()));
    }
    let (iv, rest) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(iv), rest)
        .map_err(|_| "解密失败：密钥与密文不匹配".to_string())?;
    String::from_utf8(plain).map_err(|e| format!("解密结果不是 UTF-8：{e}"))
}

/// 取消一次进行中的绑定。
pub fn cancel(sessions: &BindSessions, session_key: &str) {
    sessions
        .inner
        .lock()
        .expect("bind sessions lock")
        .remove(session_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::Key;

    #[test]
    fn 状态枚举与官方一致() {
        assert_eq!(QqStatus::from_i64(0), QqStatus::None);
        assert_eq!(QqStatus::from_i64(1), QqStatus::Pending);
        assert_eq!(QqStatus::from_i64(2), QqStatus::Completed);
        assert_eq!(QqStatus::from_i64(3), QqStatus::Expired);
        assert_eq!(QqStatus::from_i64(99), QqStatus::None);
    }

    #[test]
    fn 企业微信授权地址只放行本厂商() {
        assert!(safe_wecom_url("https://work.weixin.qq.com/ai/qc/auth?x=1").is_some());
        assert!(safe_wecom_url("https://work.weixin.qq.com:443/a").is_some());
        assert!(safe_wecom_url("http://work.weixin.qq.com/a").is_none(), "必须 https");
        assert!(safe_wecom_url("https://evil.com/a").is_none(), "换域名");
        assert!(safe_wecom_url("https://work.weixin.qq.com.evil.com/a").is_none());
        assert!(safe_wecom_url("https://work.weixin.qq.com:8443/a").is_none());
        assert!(safe_wecom_url("https://u:p@work.weixin.qq.com/a").is_none());
    }

    #[test]
    fn url编码覆盖保留字符() {
        assert_eq!(urlencode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(urlencode("abc-_.~"), "abc-_.~");
    }

    /// 用真实的 AES-256-GCM 造一段密文，验证布局假设 `[iv][ct][tag]` 是对的。
    #[test]
    fn qq密文解密往返() {
        let key = [7u8; 32];
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let mut iv = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut iv);
        let iv = Nonce::from_slice(&iv);
        let plain = b"the-app-secret";
        let ct = cipher.encrypt(&iv, plain.as_ref()).expect("encrypt");
        let mut raw = iv.to_vec();
        raw.extend_from_slice(&ct);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert_eq!(decrypt_qq_secret(&key, &b64).unwrap(), "the-app-secret");
    }

    #[test]
    fn qq密文密钥不匹配时失败() {
        let key = [7u8; 32];
        let other = [9u8; 32];
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let mut raw_iv = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut raw_iv);
        let iv = Nonce::from_slice(&raw_iv);
        let ct = cipher.encrypt(&iv, b"secret".as_ref()).expect("encrypt");
        let mut raw = iv.to_vec();
        raw.extend_from_slice(&ct);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert!(decrypt_qq_secret(&other, &b64).is_err());
    }

    #[test]
    fn qq密文过短时拒绝() {
        let key = [7u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 10]);
        assert!(decrypt_qq_secret(&key, &b64).is_err());
    }

    #[test]
    fn 轮询未知会话返回未开始() {
        let s = BindSessions::new();
        let fut = poll(&s, "qq", "nope");
        // poll 是 async，测试里用 block_on 跑这个无网络分支。
        let got = futures::executor::block_on(fut);
        assert!(matches!(got, BindPoll::NotStarted { .. }));
    }

    #[test]
    fn 平台代码映射正确() {
        let c = platform_code();
        assert!([1, 2, 3].contains(&c));
    }
}
