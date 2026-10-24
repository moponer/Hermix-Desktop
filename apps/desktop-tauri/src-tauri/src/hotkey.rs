//! 全局热键的统一管理 —— 截图与文件搜索共用一个实现。
//!
//! 这里是三处曾经各写一遍、各踩一遍的坑的收口：
//!
//! 1. **派发键名对不上（按了没反应）**。
//!    `global_hotkey::HotKey` 的 `Display` 是 `"alt+KeyQ"` —— 修饰键小写、
//!    键名用 `keyboard_types::Code` 的变体名，**不是** `"alt+Q"`。
//!    此前 lib.rs 的派发写死 `name == "alt+q"`，那个条件**永远不成立**：
//!    热键注册成功，按下去什么都不发生（用户看到的就是"热键没反应/被占"）。
//!    比较必须两端都过同一个规范化函数。
//!
//! 2. **重复注册被当成失败（功能是好的，却报"未生效"）**。
//!    `tauri_plugin_global_shortcut` 对同一实例重复注册同一个键返回
//!    `AlreadyRegistered`。setup 已注册默认热键、前端启动再回放一次，
//!    必然撞上 → 前端弹错误、把 UI 回滚成"没生效"，而功能其实好着。
//!    重复 apply 必须幂等。
//!
//! 3. **冲突即哑掉（被别的程序占了就没救）**。
//!    热键被占时功能直接失效，用户只看得到 stderr 一行日志。改成：
//!    请求的键被占 → 依次试兜底键 → 全被占才报错；并且把**实际生效的键**
//!    回报给前端（设置页显示 + 启动时 toast），功能不再无声死掉。
//!
//! 单一真相源：谁注册谁记账。setup 注册的默认键也进这张账，
//! 关闭功能时解的就是它，不会解错另一个键。

use std::sync::Mutex;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

/// 热键状态 —— 回报给前端的"现在到底哪个键在生效"。
#[derive(Serialize, Clone, Debug, Default)]
pub struct HotkeyStatus {
    /// 当前生效热键的展示名（"Alt+Q"）；None = 没注册（功能关闭或注册失败）。
    pub active: Option<String>,
    /// 用户请求的热键展示名。
    pub requested: Option<String>,
    /// true = 请求的键不可用（被别的程序占着），实际生效的是兜底键。
    pub substituted: bool,
    /// 登记的规范化键（比较用），如 "alt+keyq"。
    #[serde(skip)]
    pub canon: Option<String>,
}

/// 一个功能（截图 / 文件搜索）一个槽。
pub struct HotkeySlot {
    /// 功能名，进日志与错误信息。
    pub name: &'static str,
    /// 默认热键：setup 兜底注册用。
    pub default_hotkey: &'static str,
    /// 请求键被占时依次尝试的兜底键。
    pub fallbacks: &'static [&'static str],
    /// 当前真正注册着的键。
    registered: Mutex<Option<Registered>>,
}

/// 登记项 —— 存 `Shortcut` 本体（Copy），注销时直接用，
/// 不靠字符串反解（`Code` 的 FromStr 只认变体名，"printscreen" 这类
/// 小写显示名反解不回去，注销会静默失败）。
#[derive(Clone)]
struct Registered {
    shortcut: Shortcut,
    canon: String,
    label: String,
}

impl HotkeySlot {
    pub const fn new(
        name: &'static str,
        default_hotkey: &'static str,
        fallbacks: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            default_hotkey,
            fallbacks,
            registered: Mutex::new(None),
        }
    }

    /// 当前生效的热键状态。
    pub fn status(&self) -> HotkeyStatus {
        let reg = self.registered.lock().unwrap();
        match reg.as_ref() {
            Some(r) => HotkeyStatus {
                active: Some(r.label.clone()),
                requested: None,
                substituted: false,
                canon: Some(r.canon.clone()),
            },
            None => HotkeyStatus::default(),
        }
    }

    /// 按下的键是不是本功能的热键。
    ///
    /// 入参是插件给的 `Shortcut` —— 两侧都过同一个规范化函数，
    /// 避免"注册用的写法"与"比较用的写法"不一致。
    pub fn matches(&self, pressed: &Shortcut) -> bool {
        let canon = canon_of(pressed);
        let reg = self.registered.lock().unwrap();
        reg.as_ref().map(|r| r.canon == canon).unwrap_or(false)
    }

    /// 应用一个热键。返回真正生效的结果。
    ///
    /// 幂等：同一个键重复 apply 直接返回当前状态，不重复注册（插件会把
    /// 重复注册报成 AlreadyRegistered，前端会误判成"热键没生效"）。
    /// 失败（格式非法 / 全被占）时**不破坏现状**：旧热键仍在。
    pub fn apply(&self, app: &AppHandle, requested: &str) -> Result<HotkeyStatus, String> {
        let want = requested.trim();
        if want.is_empty() {
            return Err(format!("{} 热键不能为空", self.name));
        }
        let sc: Shortcut = want
            .parse()
            .map_err(|e| format!("{} 热键 {want} 格式无效: {e}", self.name))?;
        let canon = canon_of(&sc);
        let label = label_of(&sc);

        // 幂等短路：已经是这个键了，什么都不用做。
        {
            let reg = self.registered.lock().unwrap();
            if let Some(r) = reg.as_ref() {
                if r.canon == canon {
                    return Ok(HotkeyStatus {
                        active: Some(label.clone()),
                        requested: Some(label),
                        substituted: false,
                        canon: Some(canon),
                    });
                }
            }
        }

        // 先注册新的（成功后再解旧的）——失败时旧热键仍在，现状不被破坏。
        let (new_sc, new_canon, new_label, substituted) = match register_shortcut(app, &sc) {
            Ok(()) => (sc, canon.clone(), label.clone(), false),
            Err(first_err) => {
                let mut last_err = first_err;
                let mut picked: Option<(Shortcut, String, String)> = None;
                for fb in self.fallbacks {
                    let fb_sc: Shortcut = match fb.parse() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let fb_canon = canon_of(&fb_sc);
                    if fb_canon == canon {
                        continue;
                    }
                    match register_shortcut(app, &fb_sc) {
                        Ok(()) => {
                            picked = Some((fb_sc, fb_canon, label_of(&fb_sc)));
                            break;
                        }
                        Err(e) => last_err = e,
                    }
                }
                let Some((s, c, l)) = picked else {
                    return Err(format!(
                        "{} 热键 {want} 注册失败（{last_err}）；备选键也都被占用，请换一个",
                        self.name
                    ));
                };
                (s, c, l, true)
            }
        };

        // 换键：解掉旧的。注意**先放锁再解注册** —— 插件的 unregister
        // 会同步到主线程，而热键回调也在主线程，它要读同一把锁；
        // 持锁调用就是自死锁。
        let old = {
            let mut reg = self.registered.lock().unwrap();
            reg.replace(Registered {
                shortcut: new_sc,
                canon: new_canon.clone(),
                label: new_label.clone(),
            })
        };
        if let Some(old) = old {
            if old.canon != new_canon {
                let _ = app.global_shortcut().unregister(old.shortcut);
            }
        }

        Ok(HotkeyStatus {
            active: Some(new_label),
            requested: Some(label),
            substituted,
            canon: Some(new_canon),
        })
    }

    /// setup 兜底：注册默认热键；被占则依次试兜底键。
    /// 永不失败 —— 全部被占时只打日志（热键问题不该阻塞应用启动）。
    pub fn apply_default(&self, app: &AppHandle) -> HotkeyStatus {
        match self.apply(app, self.default_hotkey) {
            Ok(st) => {
                if st.substituted {
                    eprintln!(
                        "[hotkey] {} 默认热键 {} 被占用，已改用 {}",
                        self.name,
                        self.default_hotkey,
                        st.active.as_deref().unwrap_or("?")
                    );
                }
                st
            }
            Err(e) => {
                eprintln!("[hotkey] {e}");
                HotkeyStatus::default()
            }
        }
    }

    /// 关闭功能：解掉当前登记的热键。
    pub fn disable(&self, app: &AppHandle) {
        let old = {
            let mut reg = self.registered.lock().unwrap();
            reg.take()
        };
        if let Some(old) = old {
            let _ = app.global_shortcut().unregister(old.shortcut);
        }
    }
}

/// 规范化键（比较用）：`HotKey` 的 Display + 小写。
/// "alt+KeyQ" → "alt+keyq"。注册与比较必须走同一个函数，否则键名对不上。
pub fn canon_of(sc: &Shortcut) -> String {
    sc.to_string().to_lowercase()
}

/// 展示名："alt+KeyQ" → "Alt+Q"，"printscreen" → "PrtSc"。
pub fn label_of(sc: &Shortcut) -> String {
    let mut parts: Vec<String> = Vec::new();
    if sc.mods.contains(Modifiers::SHIFT) {
        parts.push("Shift".into());
    }
    if sc.mods.contains(Modifiers::CONTROL) {
        parts.push("Ctrl".into());
    }
    if sc.mods.contains(Modifiers::ALT) {
        parts.push("Alt".into());
    }
    if sc.mods.contains(Modifiers::SUPER) {
        parts.push("Win".into());
    }
    parts.push(key_label(sc.key));
    parts.join("+")
}

/// 键名展示：`Code` 的变体名 → 人读的短标签。
fn key_label(code: Code) -> String {
    let s = code.to_string();
    if let Some(rest) = s.strip_prefix("Key") {
        return rest.to_string();
    }
    if let Some(rest) = s.strip_prefix("Digit") {
        return rest.to_string();
    }
    match s.as_str() {
        "PrintScreen" => "PrtSc".into(),
        "Escape" => "Esc".into(),
        "Enter" => "Enter".into(),
        "ArrowUp" => "↑".into(),
        "ArrowDown" => "↓".into(),
        "ArrowLeft" => "←".into(),
        "ArrowRight" => "→".into(),
        other => other.to_string(),
    }
}

/// 注册一个热键。插件对"本实例已注册"与"被别的程序占用"报同一个错，
/// 这里统一转成可读文案。
fn register_shortcut(app: &AppHandle, sc: &Shortcut) -> Result<(), String> {
    app.global_shortcut()
        .register(*sc)
        .map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_of_alt_q_is_not_the_display_name() {
        // 这就是老 bug 的根：Display 给的是 "alt+KeyQ"，不是 "alt+Q"。
        let sc: Shortcut = "alt+Q".parse().unwrap();
        assert_eq!(sc.to_string(), "alt+KeyQ");
        assert_eq!(canon_of(&sc), "alt+keyq");
    }

    #[test]
    fn label_is_human_readable() {
        let sc: Shortcut = "alt+Q".parse().unwrap();
        assert_eq!(label_of(&sc), "Alt+Q");
        let sc: Shortcut = "ctrl+Alt+A".parse().unwrap();
        assert_eq!(label_of(&sc), "Ctrl+Alt+A");
        let sc: Shortcut = "printscreen".parse().unwrap();
        assert_eq!(label_of(&sc), "PrtSc");
    }

    #[test]
    fn matching_uses_canon_not_display() {
        let slot = HotkeySlot::new("测试", "alt+Q", &[]);
        // 手工塞一个登记项（不碰插件，纯逻辑验证比较口径）。
        let sc: Shortcut = "alt+Q".parse().unwrap();
        *slot.registered.lock().unwrap() = Some(Registered {
            shortcut: sc,
            canon: "alt+keyq".into(),
            label: "Alt+Q".into(),
        });
        let pressed: Shortcut = "alt+Q".parse().unwrap();
        assert!(slot.matches(&pressed));
        let other: Shortcut = "alt+W".parse().unwrap();
        assert!(!slot.matches(&other));
    }
}
