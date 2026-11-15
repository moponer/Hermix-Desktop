//! AI 辅助清理 —— 把清理候选清单交给 LLM 裁决，界面按裁决自动勾选。
//!
//! 形态选择（调研结论，详见决策笔记
//! `docs/notes/implemented/feature/2026-10-23-system-tuner.md`）：
//!
//! | 形态 | 做法 | 结论 |
//! |---|---|---|
//! | A | 把全部候选喂给模型，让它逐条决定 | **否**：token 成本随候选数线性膨胀，误删风险不可控 |
//! | **B** | **本地规则先打分分档，模型只裁决边界项** | **采用**：本地硬规则锁死禁区，模型只在边界档给建议；没有 AI 时规则评分仍独立可用 |
//! | C | 让模型生成清理脚本 | **否**：等于把任意代码执行权交给模型，无法做白名单校验 |
//!
//! 本模块是形态 B 的落地：**AI 只能"建议"**。
//!
//! 安全闸（服务端强制，模型说什么都不算数）：
//! 1. 模型给的 `id` 必须存在于本次候选集合里，否则丢弃并计数；
//! 2. `decision` 只能是 `clean` / `keep` / `unsure` 三选一；
//! 3. **本地标记为 risky（高风险）的项，模型即使说 clean 也不采纳** ——
//!    越权建议被丢弃并计入 `blocked`，并在界面上如实告诉用户；
//! 4. `unsure` 一律不自动勾选，只作为"需要人工看"的提示；
//! 5. 解析失败 → 整体失败，界面**不显示任何 AI 勾选**（无声降级，
//!    不能让用户误以为 AI 认可了什么）。
//!
//! 上下文纪律（AGENTS.md 零膨胀红线）：只送**结构化清单**（id/标题/分类/
//! 体积/文件数），绝不把扫描日志原文喂模型；候选超上限时按体积取前 N 条
//! 并显式告知模型已截断。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai;
use crate::settings::ViewerSettings;

/// 一次送进模型的候选上限（超出按体积取前 N 条并标注截断）。
const MAX_CANDIDATES: usize = 60;
/// 模型回复的 token 上限（只让它输出短 JSON）。
const MAX_TOKENS: u32 = 2000;
/// 理由字段长度上限（超长截断，避免模型长篇大论把界面撑爆）。
const MAX_REASON_CHARS: usize = 60;

/// 送进模型的候选项（前端传）。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiCandidate {
    pub id: String,
    pub title: String,
    pub group: String,
    pub bytes: u64,
    pub files: u64,
    /// 本地规则判定为高风险（默认不勾选、需二次确认）。
    pub risky: bool,
    pub needs_admin: bool,
    /// 本地规则给的说明（帮模型理解这一项是什么）。
    pub hint: String,
}

/// 模型对一项的裁决。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiVerdict {
    pub id: String,
    /// `clean`（建议清理）/ `keep`（建议保留）/ `unsure`（待人工判断）。
    pub decision: String,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiAdviceReport {
    pub verdicts: Vec<AiVerdict>,
    /// 被本地安全网拦下的越权建议条数。
    pub blocked: usize,
    /// 模型给了未知 / 非法 id 而被丢弃的条数。
    pub dropped: usize,
    /// 候选过多被截断（只送了前 N 条）。
    pub truncated: bool,
    /// 实际送进模型的候选数。
    pub considered: usize,
    pub model: String,
    /// 模型的整体说明（可空）。
    pub notes: String,
}

const SYSTEM_PROMPT: &str = "\
你是一个谨慎的磁盘清理助手。给定一组「清理项」，逐项判断哪些可以安全清理。

判断标准：
- clean：纯缓存 / 临时数据 / 可自动重建的产物，删除不会丢用户数据、不影响系统功能。
- keep：可能含用户数据，或属于系统必需内容，保留。
- unsure：无法判断，需要用户人工确认。

硬性规则：
1. 只能对清单里出现过的 id 作答，不得发明新 id。
2. 以下一律 keep：Windows 更新缓存、组件存储、$Installer、用户下载目录、
   文档、聊天记录、浏览器书签与密码、系统还原点。
3. 涉及用户个人数据（下载、文档、桌面、图片视频）的一律 keep 或 unsure。
4. 只输出 JSON，不要输出任何解释文字或 Markdown 代码块。

输出格式：
{\"version\":1,\"items\":[{\"id\":\"<清单里的id>\",\"decision\":\"clean|keep|unsure\",\"confidence\":0.0,\"reason\":\"一句话，不超过30字\"}],\"notes\":\"整体说明\"}";

#[tauri::command]
pub async fn cleanup_ai_advise(items: Vec<AiCandidate>) -> Result<AiAdviceReport, String> {
    let settings = ViewerSettings::load();
    let endpoint = ai::resolve(&settings)?;
    if !endpoint.info.ready {
        return Err(endpoint
            .info
            .problem
            .clone()
            .unwrap_or_else(|| "AI 后端未配置".to_string()));
    }
    if items.is_empty() {
        return Ok(AiAdviceReport {
            verdicts: Vec::new(),
            blocked: 0,
            dropped: 0,
            truncated: false,
            considered: 0,
            model: endpoint.info.model.clone(),
            notes: String::new(),
        });
    }

    // 按体积降序取前 N 条：模型注意力有限，优先让它看最值钱的几项。
    let mut sorted = items.clone();
    sorted.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let truncated = sorted.len() > MAX_CANDIDATES;
    sorted.truncate(MAX_CANDIDATES);

    let payload: Vec<Value> = sorted
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "title": c.title,
                "group": c.group,
                "size_mb": (c.bytes as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0,
                "files": c.files,
                "desc": c.hint,
                "local_risky": c.risky,
                "needs_admin": c.needs_admin,
            })
        })
        .collect();

    let user_text = format!(
        "以下是待判断的清理项清单（JSON 数组）：\n{}\n\n请对每一项给出判断。{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string()),
        if truncated {
            format!("（清单共 {} 项，已按体积截取到前 {} 项）", items.len(), MAX_CANDIDATES)
        } else {
            String::new()
        }
    );

    let reply = ai::chat(
        None, // 后台批量任务：不向前端发流式事件
        &endpoint,
        SYSTEM_PROMPT,
        &[],
        &user_text,
        Some(0.0),
        Some(MAX_TOKENS),
    )
    .await?;

    let (verdicts, blocked, dropped) = validate(&reply, &sorted);
    let notes = extract_notes(&reply);

    Ok(AiAdviceReport {
        verdicts,
        blocked,
        dropped,
        truncated,
        considered: sorted.len(),
        model: endpoint.info.model.clone(),
        notes,
    })
}

/// 解析并校验模型回复。
///
/// 校验失败不抛错，而是**逐条丢弃**不合法的裁决 —— 模型输出不可信，
/// 但已经合法的那几条仍然有用。整段无法解析时返回空（界面不显示任何 AI 勾选）。
fn validate(reply: &str, candidates: &[AiCandidate]) -> (Vec<AiVerdict>, usize, usize) {
    let Some(root) = extract_json_object(reply) else {
        return (Vec::new(), 0, 0);
    };
    let Some(items) = root.get("items").and_then(|v| v.as_array()) else {
        return (Vec::new(), 0, 0);
    };

    let known: std::collections::HashMap<&str, &AiCandidate> = candidates
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();

    let mut verdicts: Vec<AiVerdict> = Vec::new();
    let mut blocked = 0usize;
    let mut dropped = 0usize;

    for item in items {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            dropped += 1;
            continue;
        };
        let Some(candidate) = known.get(id) else {
            dropped += 1; // 模型发明了不存在的 id
            continue;
        };
        let decision = item
            .get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !matches!(decision.as_str(), "clean" | "keep" | "unsure") {
            dropped += 1;
            continue;
        }
        // 本地高风险项：模型无权建议清理 → 丢弃并计数。
        if decision == "clean" && candidate.risky {
            blocked += 1;
            continue;
        }
        let confidence = item
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0) as f32;
        let reason: String = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .chars()
            .take(MAX_REASON_CHARS)
            .collect();
        verdicts.push(AiVerdict {
            id: id.to_string(),
            decision,
            confidence,
            reason,
        });
    }

    (verdicts, blocked, dropped)
}

/// 从回复里抠出第一个完整的 JSON 对象（模型爱加代码块壳和前后废话）。
fn extract_json_object(reply: &str) -> Option<Value> {
    let trimmed = reply.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
}

/// 整体说明（可选字段）。
fn extract_notes(reply: &str) -> String {
    extract_json_object(reply)
        .and_then(|root| root.get("notes").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, risky: bool) -> AiCandidate {
        AiCandidate {
            id: id.into(),
            title: "x".into(),
            group: "系统".into(),
            bytes: 1024,
            files: 3,
            risky,
            needs_admin: false,
            hint: "h".into(),
        }
    }

    #[test]
    fn accepts_valid_verdicts() {
        let cands = vec![candidate("a", false), candidate("b", false)];
        let reply = r#"{"version":1,"items":[{"id":"a","decision":"clean","confidence":0.9,"reason":"纯缓存"},{"id":"b","decision":"keep","confidence":0.2,"reason":"有用户数据"}]}"#;
        let (v, blocked, dropped) = validate(reply, &cands);
        assert_eq!(v.len(), 2);
        assert_eq!(blocked, 0);
        assert_eq!(dropped, 0);
        assert_eq!(v[0].decision, "clean");
    }

    #[test]
    fn drops_unknown_ids_and_bad_decisions() {
        let cands = vec![candidate("a", false)];
        let reply = r#"{"items":[{"id":"zzz","decision":"clean"},{"id":"a","decision":"delete"}]}"#;
        let (v, _blocked, dropped) = validate(reply, &cands);
        assert!(v.is_empty());
        assert_eq!(dropped, 2);
    }

    #[test]
    fn risky_item_cannot_be_recommended_clean() {
        let cands = vec![candidate("a", true)];
        let reply = r#"{"items":[{"id":"a","decision":"clean","confidence":1.0,"reason":"x"}]}"#;
        let (v, blocked, dropped) = validate(reply, &cands);
        assert!(v.is_empty());
        assert_eq!(blocked, 1);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn strips_code_fence_and_prose() {
        let reply = "好的，我来判断：\n```json\n{\"items\":[{\"id\":\"a\",\"decision\":\"unsure\",\"confidence\":0.5,\"reason\":\"x\"}]}\n```\n以上。";
        let cands = vec![candidate("a", false)];
        let (v, _b, _d) = validate(reply, &cands);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].decision, "unsure");
    }

    #[test]
    fn garbage_reply_yields_no_advice() {
        let cands = vec![candidate("a", false)];
        let (v, blocked, dropped) = validate("完全不是 JSON", &cands);
        assert!(v.is_empty());
        assert_eq!(blocked, 0);
        assert_eq!(dropped, 0);
    }
}
