//! ACUI (Agent-Controlled UI) directive protocol.
//!
//! 借鉴 BaiLongma `ui_show` / `ui_update` / `ui_hide` 协议。Agent 通过
//! 事件流把结构化 UI 卡片推送给前端，前端按 kind 查注册表渲染对应组件。
//!
//! 设计要点：
//! - **协议层与渲染层解耦**：本模块只定义数据格式，不绑定 React / Vue /
//!   Web Components。前端栈可任意替换。
//! - **开放扩展性**：`kind` 是任意字符串，前端遇到未知 kind 走 fallback
//!   （原始 JSON 调试视图）而不是崩溃。
//! - **id 寻址**：每张卡片有唯一 id，`update` / `hide` 按 id 寻址。
//! - **props 是 Value**：用 `serde_json::Value` 表达任意结构，前端按
//!   kind 自解释。

use serde::{Deserialize, Serialize};

/// ACUI directive — 三种操作。
///
/// 通过事件流序列化后发往前端。tag = "op"，前端的判别字段是 `op`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AcuiDirective {
    /// 显示一张新卡片。如果同 id 已存在则替换（前端按 id 判重）。
    Show(AcuiShow),
    /// 增量更新已显示的卡片（按 id 匹配）。
    Update(AcuiUpdate),
    /// 隐藏卡片。
    Hide(AcuiHide),
}

impl AcuiDirective {
    /// 取出 directive 涉及到的卡片 id。
    /// - `show` / `update` / `hide` 都返回自身的 id
    pub fn id(&self) -> &str {
        match self {
            AcuiDirective::Show(s) => &s.id,
            AcuiDirective::Update(u) => &u.id,
            AcuiDirective::Hide(h) => &h.id,
        }
    }
}

/// Show directive — 显示一张新卡片。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcuiShow {
    /// 唯一 ID，用于后续 update / hide。UUID 字符串。
    pub id: String,
    /// 卡片类型（如 `"weather"` / `"self_check_step"` / `"awakening"`）。
    /// 前端 AcuiRegistry 按 kind 查组件；未知 kind 走 fallback。
    #[serde(rename = "type")]
    pub kind: String,
    /// 卡片相对消息文本的位置。默认 `After`。
    #[serde(default)]
    pub position: AcuiPosition,
    /// 卡片 props（任意 JSON，前端按 kind 解释）。
    pub props: serde_json::Value,
    /// 可选创建时间戳。None 时前端用本地接收时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AcuiShow {
    /// 构造一个标准 Show 指令。
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        props: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            position: AcuiPosition::default(),
            props,
            created_at: None,
        }
    }

    /// 设置位置。
    pub fn with_position(mut self, pos: AcuiPosition) -> Self {
        self.position = pos;
        self
    }

    /// 设置创建时间。
    pub fn with_created_at(mut self, ts: chrono::DateTime<chrono::Utc>) -> Self {
        self.created_at = Some(ts);
        self
    }
}

/// 卡片相对消息文本的位置。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcuiPosition {
    /// 渲染在消息文本之后（默认）。最常见：补充说明卡片。
    #[default]
    After,
    /// 渲染在消息文本之前。适合"上下文背景"卡片。
    Before,
    /// 替换消息文本。适合"agent 直接渲染结构化答案"场景。
    Replace,
}

/// Update directive — 增量更新已显示的卡片。
///
/// `patch` 是 JSON Merge Patch (RFC 7396) 风格的对象：浅合并到现有 props。
/// 前端用 `id` 找到已显示的卡片，合并 patch。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcuiUpdate {
    /// 目标卡片 id（必须先前已 show）。
    pub id: String,
    /// 浅合并的 patch。
    pub patch: serde_json::Value,
}

impl AcuiUpdate {
    pub fn new(id: impl Into<String>, patch: serde_json::Value) -> Self {
        Self { id: id.into(), patch }
    }
}

/// Hide directive — 隐藏卡片。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcuiHide {
    /// 目标卡片 id。
    pub id: String,
}

impl AcuiHide {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// 状态：当前所有已 show 未 hide 的卡片。
///
/// 前端在内存里维护这样一个集合（按 id 索引），事件流是 input。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcuiState {
    /// 卡片 id -> Show payload。
    /// 用 IndexMap 保持 show 顺序，前端按这个顺序渲染。
    pub cards: indexmap::IndexMap<String, AcuiShow>,
}

impl AcuiState {
    pub fn new() -> Self {
        Self { cards: indexmap::IndexMap::new() }
    }

    /// 应用一个 directive，更新内部状态。
    /// 返回 `true` 表示状态发生了变化（用于触发重新渲染）。
    pub fn apply(&mut self, directive: &AcuiDirective) -> bool {
        match directive {
            AcuiDirective::Show(s) => {
                self.cards.insert(s.id.clone(), s.clone());
                true
            }
            AcuiDirective::Update(u) => {
                if let Some(existing) = self.cards.get_mut(&u.id) {
                    if let (Some(existing_props), Some(patch_props)) =
                        (existing.props.as_object_mut(), u.patch.as_object())
                    {
                        // 浅合并
                        for (k, v) in patch_props {
                            existing_props.insert(k.clone(), v.clone());
                        }
                    } else {
                        existing.props = u.patch.clone();
                    }
                    true
                } else {
                    // update 一个不存在的 id：忽略（前端也忽略），不变化
                    false
                }
            }
            AcuiDirective::Hide(h) => self.cards.shift_remove(&h.id).is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn directive_serde_round_trip_show() {
        let d = AcuiDirective::Show(AcuiShow::new(
            "card-1",
            "weather",
            json!({"city": "Beijing", "temp": 23}),
        ));
        let s = serde_json::to_string(&d).unwrap();
        let back: AcuiDirective = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn directive_serde_uses_snake_case_tag() {
        let d = AcuiDirective::Hide(AcuiHide::new("card-1"));
        let v: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert_eq!(v["op"], "hide");
        assert_eq!(v["id"], "card-1");
    }

    #[test]
    fn directive_serde_show_uses_type_field_for_kind() {
        // kind 序列化为 "type" 字段，避免和 serde tag "op" 冲突
        let d = AcuiDirective::Show(AcuiShow::new("c1", "self_check_step", json!({})));
        let v: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert_eq!(v["op"], "show");
        assert_eq!(v["type"], "self_check_step");
        assert_eq!(v["id"], "c1");
        assert_eq!(v["position"], "after"); // default
    }

    #[test]
    fn directive_serde_position_is_optional_in_show() {
        // position 默认是 "after"，但序列化应该输出，便于前端无歧义解释
        let d = AcuiDirective::Show(AcuiShow::new("c1", "x", json!({})));
        let v: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert!(v.get("position").is_some());
    }

    #[test]
    fn directive_id_accessor() {
        let s = AcuiDirective::Show(AcuiShow::new("a", "b", json!({})));
        let u = AcuiDirective::Update(AcuiUpdate::new("c", json!({})));
        let h = AcuiDirective::Hide(AcuiHide::new("d"));
        assert_eq!(s.id(), "a");
        assert_eq!(u.id(), "c");
        assert_eq!(h.id(), "d");
    }

    #[test]
    fn show_with_position_and_timestamp() {
        let now = Utc::now();
        let s = AcuiShow::new("c1", "awakening", json!({"step": 1}))
            .with_position(AcuiPosition::Before)
            .with_created_at(now);
        assert_eq!(s.position, AcuiPosition::Before);
        assert_eq!(s.created_at, Some(now));
    }

    #[test]
    fn update_merges_into_existing_props() {
        let mut state = AcuiState::new();
        state.apply(&AcuiDirective::Show(AcuiShow::new(
            "c1",
            "progress",
            json!({"step": 1, "total": 10, "label": "init"}),
        )));

        // 浅合并：只覆盖 patch 里有的字段
        let changed = state.apply(&AcuiDirective::Update(AcuiUpdate::new(
            "c1",
            json!({"step": 5}),
        )));
        assert!(changed);

        let card = state.cards.get("c1").unwrap();
        assert_eq!(card.props["step"], 5);
        assert_eq!(card.props["total"], 10); // 保留
        assert_eq!(card.props["label"], "init"); // 保留
    }

    #[test]
    fn update_on_missing_id_is_noop() {
        let mut state = AcuiState::new();
        let changed = state.apply(&AcuiDirective::Update(AcuiUpdate::new(
            "missing",
            json!({"x": 1}),
        )));
        assert!(!changed);
        assert!(state.cards.is_empty());
    }

    #[test]
    fn hide_removes_card() {
        let mut state = AcuiState::new();
        state.apply(&AcuiDirective::Show(AcuiShow::new("c1", "x", json!({}))));
        assert_eq!(state.cards.len(), 1);

        let changed = state.apply(&AcuiDirective::Hide(AcuiHide::new("c1")));
        assert!(changed);
        assert!(state.cards.is_empty());
    }

    #[test]
    fn hide_missing_is_noop() {
        let mut state = AcuiState::new();
        let changed = state.apply(&AcuiDirective::Hide(AcuiHide::new("missing")));
        assert!(!changed);
    }

    #[test]
    fn show_overrides_existing_with_same_id() {
        let mut state = AcuiState::new();
        state.apply(&AcuiDirective::Show(AcuiShow::new("c1", "x", json!({"v": 1}))));
        state.apply(&AcuiDirective::Show(AcuiShow::new("c1", "y", json!({"v": 2}))));
        let card = state.cards.get("c1").unwrap();
        assert_eq!(card.kind, "y");
        assert_eq!(card.props["v"], 2);
        assert_eq!(state.cards.len(), 1); // 没有重复
    }

    #[test]
    fn show_preserves_insertion_order() {
        let mut state = AcuiState::new();
        state.apply(&AcuiDirective::Show(AcuiShow::new("a", "x", json!({}))));
        state.apply(&AcuiDirective::Show(AcuiShow::new("b", "x", json!({}))));
        state.apply(&AcuiDirective::Show(AcuiShow::new("c", "x", json!({}))));
        let ids: Vec<&String> = state.cards.keys().collect();
        assert_eq!(ids, vec![&"a".to_string(), &"b".to_string(), &"c".to_string()]);
    }

    #[test]
    fn update_replaces_props_when_existing_is_not_object() {
        // 边界情况：现有 props 不是 object（如 string / array），直接替换
        let mut state = AcuiState::new();
        state.apply(&AcuiDirective::Show(AcuiShow::new("c1", "x", json!("scalar"))));
        let changed = state.apply(&AcuiDirective::Update(AcuiUpdate::new(
            "c1",
            json!({"k": "v"}),
        )));
        assert!(changed);
        assert_eq!(state.cards["c1"].props, json!({"k": "v"}));
    }

    #[test]
    fn state_default_is_empty() {
        let s = AcuiState::default();
        assert!(s.cards.is_empty());
    }
}
