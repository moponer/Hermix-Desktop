//! Harness² 的**任务级递归**：Judge → Probe → Compose。
//!
//! ## 为什么这个模块存在
//!
//! 之前的实现只有"单步沉淀"：一次成功 → 写一个 auto skill；失败 → 写一条经验。
//! 那是 Harness² 的形状，但不是它的方法。它的方法是**对比**：
//!
//! > 在同一个任务上探索 harness 的编辑，靠**多次执行之间的行为差异**来组合
//! > 出更好的 harness —— 而且不需要 ground-truth 反馈。
//!
//! 没有对比，"经验"只是"这次发生了什么"；有了对比，才知道"哪条路走通了、
//! 哪条路撞墙了、区别在哪"。
//!
//! ## 三段在本项目里的形态
//!
//! - **Judge**：判定一次执行"做对了没"。Harness² 在基准上跑 judge；我们没有
//!   基准，所以用**可核验信号**合成判决（自然完成 / 末轮无工具错误 / 用户是否
//!   重发同一请求 / 中断 / 循环收敛）。**不调 LLM**：那会加一次 round-trip 且
//!   不可复现，而这些信号本就记录了事实。
//! - **Probe**：同一任务（按[签名](task_signature)归一化）的多次执行。
//!   Harness² 主动跑 K 次；这里**不自动重放**——同一任务通常带副作用（写文件、
//!   提交表单），自动重放会真的执行两次。改为把**用户的真实重试**当作天然
//!   probe，跨会话累积。这是"K 次"在这种产品形态下唯一安全的形态。
//! - **Compose**：从同一签名的多次 probe 里提炼。成功与失败的**行为差异**就是
//!   教训，这正是 Harness² "无需标准答案"的落点。
//!
//! 存储：`<home>/probes/<sig>.jsonl`，一次执行一行；步骤与错误都按 auto skill
//! 的同量级截断，档案不会无限膨胀。

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use hermix_core::ToolErrorRecord;

/// 一次执行的任务级判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    /// 自然完成、末轮无错 → 这条路走通了。
    Succeeded,
    /// 没走完 / 末轮仍错 / 用户重发同一请求 → 这条路有问题。
    Failed,
    /// 信号不足（例如用户主动中断，或没有工具调用）。
    Unknown,
}

impl TaskOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// 判决依据。保留具体信号而不只保留结论，因为**信号是可比对的**：
/// "这次失败是因为末轮还有错"与"上次失败是因为用户中断"是不同性质的失败，
/// Compose 时不该混为一谈。
#[derive(Debug, Clone)]
pub struct Verdict {
    pub outcome: TaskOutcome,
    pub signals: Vec<&'static str>,
}

impl Verdict {
    pub fn has(&self, signal: &str) -> bool {
        self.signals.iter().any(|s| *s == signal)
    }
}

/// 合成一次执行的判决。
///
/// 信号优先级：明确的失败证据 > 明确的成功证据 > 未知。**用户重发同一请求**
/// 是最强的失败证据（比 `finished_naturally` 更强：agent 可以"完成"得很自信
/// 但结果是错的，用户只好再问一遍），所以它单独就能把判决压成 `Failed`。
pub fn judge(
    finished_naturally: bool,
    interrupted: bool,
    tool_errors: &[ToolErrorRecord],
    last_turn_had_errors: bool,
    repeated_request: bool,
) -> Verdict {
    let mut signals: Vec<&'static str> = Vec::new();
    if finished_naturally {
        signals.push("finished_naturally");
    }
    if interrupted {
        signals.push("interrupted");
    }
    if last_turn_had_errors {
        signals.push("last_turn_errors");
    }
    if !tool_errors.is_empty() {
        signals.push("tool_errors_seen");
    }
    if repeated_request {
        signals.push("user_repeated_request");
    }

    // 顺序即优先级，`interrupted` 必须排在 `!finished_naturally` **之前**：
    // 用户打断时 `finished_naturally` 本来就是 false，若先判它，一次主动中断
    // 会被记成"失败"，进而污染 Compose（把"用户临时改主意"当成"这条路不通"）。
    //
    // `repeated_request` **不参与判决**，只作为信号记录。它是"上一次失败了"的
    // 证据，不是"这一次失败了"的证据——重试后成功恰恰是最有价值的一次执行
    // （Harness² 要对比的正是这种"失败→成功"的差异）。早先版本把它直接压成
    // Failed，结果 Compose 只看得到"两次都失败"，对比彻底失效。
    let outcome = if last_turn_had_errors {
        // 末轮还在报错：无论是否被打断，这条路确实没走通。
        TaskOutcome::Failed
    } else if interrupted {
        // 用户主动打断：既不是成功也不是失败，不能拿它当教训。
        // 必须排在 `!finished_naturally` 之前——打断时它本来就是 false。
        TaskOutcome::Unknown
    } else if !finished_naturally {
        // 没跑完、也不是用户打断——循环自己退出了（错误退出/预算耗尽）。
        TaskOutcome::Failed
    } else {
        TaskOutcome::Succeeded
    };

    Verdict { outcome, signals }
}

/// 任务签名：把用户请求归一化成可比对的短串。
///
/// 归一化只做**与意图无关**的清洗（小写、压空白、去首尾标点），不做同义改写
/// ——改写会引入"这两个请求算同一任务吗"的判断，而那个判断一旦错，Compose
/// 就会把两次不同任务的执行对比在一起，产出错误的教训。宁可漏配，不可错配。
pub fn task_signature(user_text: &str) -> String {
    let normalized: String = user_text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = normalized.trim_matches(|c: char| {
        c.is_whitespace() || c.is_ascii_punctuation() || "，。！？；：、（）【】「」".contains(c)
    });
    // 取前 120 字符参与哈希：足够区分，也不至于让超长粘贴的尾部噪声决定签名。
    let head: String = trimmed.chars().take(120).collect();
    format!("{:016x}", fnv1a(head.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 一次执行留下的探针记录。
#[derive(Debug, Clone)]
pub struct ProbeRecord {
    pub sig: String,
    pub outcome: TaskOutcome,
    pub signals: Vec<String>,
    /// (工具名, 参数摘要)，按执行顺序。
    pub steps: Vec<(String, String)>,
    /// (工具名, 错误摘要)，按发生顺序。
    pub errors: Vec<(String, String)>,
    pub turns: u32,
    pub created_at: u64,
}

const STEP_ARG_CHARS: usize = 160;
const ERROR_CHARS: usize = 160;
/// 每个任务签名保留多少条探针。足够看出"重试了几次、怎么变的"，又不会
/// 让一个反复失败的任务把档案撑爆。
pub const PROBES_PER_TASK: usize = 20;

impl ProbeRecord {
    pub fn new(
        sig: impl Into<String>,
        verdict: &Verdict,
        steps: Vec<(String, String)>,
        errors: &[ToolErrorRecord],
        turns: u32,
    ) -> Self {
        Self {
            sig: sig.into(),
            outcome: verdict.outcome,
            signals: verdict.signals.iter().map(|s| s.to_string()).collect(),
            steps: steps
                .into_iter()
                .map(|(n, a)| (n, truncate(&a, STEP_ARG_CHARS)))
                .collect(),
            errors: errors
                .iter()
                .map(|e| (e.tool_name.clone(), truncate(&e.error, ERROR_CHARS)))
                .collect(),
            turns,
            created_at: now_unix_secs(),
        }
    }

    fn to_json_line(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|(n, a)| serde_json::json!({"tool": n, "args": a}))
            .collect();
        let errors: Vec<serde_json::Value> = self
            .errors
            .iter()
            .map(|(n, e)| serde_json::json!({"tool": n, "error": e}))
            .collect();
        serde_json::json!({
            "sig": self.sig,
            "outcome": self.outcome.as_str(),
            "signals": self.signals,
            "steps": steps,
            "errors": errors,
            "turns": self.turns,
            "created_at": self.created_at,
        })
        .to_string()
    }

    fn from_json_line(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let steps = v
            .get("steps")?
            .as_array()?
            .iter()
            .filter_map(|s| {
                Some((
                    s.get("tool")?.as_str()?.to_string(),
                    s.get("args").and_then(|a| a.as_str()).unwrap_or("").to_string(),
                ))
            })
            .collect();
        let errors = v
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        Some((
                            e.get("tool")?.as_str()?.to_string(),
                            e.get("error").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            sig: v.get("sig")?.as_str()?.to_string(),
            outcome: TaskOutcome::parse(v.get("outcome").and_then(|o| o.as_str()).unwrap_or("")),
            signals: v
                .get("signals")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            steps,
            errors,
            turns: v.get("turns").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
            created_at: v.get("created_at").and_then(|t| t.as_u64()).unwrap_or(0),
        })
    }
}

/// 探针仓库：一个任务签名一个文件。
#[derive(Debug)]
pub struct ProbeStore {
    root: PathBuf,
}

impl ProbeStore {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, sig: &str) -> PathBuf {
        self.root.join(format!("{sig}.jsonl"))
    }

    pub fn history(&self, sig: &str) -> std::io::Result<Vec<ProbeRecord>> {
        let path = self.path_for(sig);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let body = fs::read_to_string(&path)?;
        Ok(body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(ProbeRecord::from_json_line)
            .collect())
    }

    /// 追加一条探针，并把每个签名的历史压到 [`PROBES_PER_TASK`] 条。
    ///
    /// 用"读全部 → 保留最近 N → 整体重写"而不是 append：单任务的历史最多
    /// 20 条（几 KB），重写比维护行号水位简单，也不会因为半截行而错位。
    pub fn record(&self, rec: &ProbeRecord) -> std::io::Result<()> {
        let mut history = self.history(&rec.sig)?;
        history.push(rec.clone());
        if history.len() > PROBES_PER_TASK {
            let drop = history.len() - PROBES_PER_TASK;
            history.drain(..drop);
        }
        let mut body = String::with_capacity(history.len() * 256);
        for r in &history {
            body.push_str(&r.to_json_line());
            body.push('\n');
        }
        let path = self.path_for(&rec.sig);
        let mut f = fs::File::create(&path)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        Ok(())
    }

    /// 该任务是否已经失败过一次——`judge` 用它标记"用户重发"。
    ///
    /// 只看**上一次**：连续重发三次时，第三次的判决仍应记 fail，但如果有一次
    /// 成功了又重发，那说明是新的需求（或用户改了主意），不该算在上一次的账上。
    pub fn last_outcome(&self, sig: &str) -> std::io::Result<Option<TaskOutcome>> {
        Ok(self.history(sig)?.last().map(|r| r.outcome))
    }
}

/// Compose 的产物：一条可直接落成 skill 的正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedHarness {
    /// 稳定的技能名（由签名派生，保证同一任务多次 compose 落到同一个技能）。
    pub slug: String,
    pub body: String,
}

/// 从同一任务的多次探针里提炼出一条 skill。
///
/// ## 规则
///
/// - 少于 2 条记录 → `None`（单次执行没有"对比"，那是 `maybe_write_auto_skill`
///   的职责，不是 Compose 的）。
/// - 全部同结果（都成功或都失败）→ 只提炼"这条路径"本身：都成功=稳定路径；
///   都失败=**别这么干**（负面知识同样有价值，且这是唯一能给出的结论）。
/// - 有成功也有失败 → 这是 Harness² 最有价值的情形：**失败记录里出现、
///   成功记录里没出现的步骤**就是分歧点，直接写成"避开 X，走 Y"。
///
/// 不做的判断：不推断"为什么"失败（那需要读日志做因果分析，容易编）。只陈述
/// 可核验的差异，把解释留给读这份 skill 的模型。
pub fn compose(sig: &str, history: &[ProbeRecord]) -> Option<ComposedHarness> {
    if history.len() < 2 {
        return None;
    }
    let succeeded = history
        .iter()
        .rev()
        .find(|r| r.outcome == TaskOutcome::Succeeded);
    let failed = history
        .iter()
        .rev()
        .find(|r| r.outcome == TaskOutcome::Failed);

    let mut body = String::with_capacity(2048);
    body.push_str(&format!(
        "本技能由 Harness² 式的**多次执行对比**提炼（`/{}`），共 {} 次执行记录。\n\n",
        sig,
        history.len()
    ));

    match (succeeded, failed) {
        (Some(ok), Some(bad)) => {
            body.push_str("## 结论\n\n这条路**能走通**，但要避开下面这些分歧点。\n\n");
            body.push_str("## 有效的路径\n\n");
            for (i, (name, args)) in ok.steps.iter().enumerate() {
                body.push_str(&format!("{}. `{name}` — `{args}`\n", i + 1));
            }
            body.push_str("\n## 失败那次做错了什么\n\n");
            if bad.errors.is_empty() {
                body.push_str(
                    "- 失败那次**没有报工具错误**，说明问题出在步骤顺序或选错了路径\
                     （不是参数错）。对照下面两段步骤差异。\n",
                );
            } else {
                for (name, err) in bad.errors.iter().take(8) {
                    body.push_str(&format!("- `{name}` 失败：{err}\n"));
                }
            }
            // 分歧点：失败路径里有、成功路径里没有的工具。
            let ok_tools: Vec<&str> = ok.steps.iter().map(|(n, _)| n.as_str()).collect();
            let divergent: Vec<&str> = bad
                .steps
                .iter()
                .map(|(n, _)| n.as_str())
                .filter(|n| !ok_tools.contains(n))
                .collect();
            if !divergent.is_empty() {
                body.push_str("\n## 分歧点\n\n失败路径用了成功路径没用的工具：");
                for t in &divergent {
                    body.push_str(&format!("`{t}` "));
                }
                body.push_str(
                    "\n先走「有效的路径」，不要先试这些工具。\n",
                );
            }
        }
        (Some(ok), None) => {
            body.push_str("## 结论\n\n这条路径**稳定跑通过**（多次执行均成功）。\n\n");
            body.push_str("## 操作步骤\n\n");
            for (i, (name, args)) in ok.steps.iter().enumerate() {
                body.push_str(&format!("{}. `{name}` — `{args}`\n", i + 1));
            }
        }
        (None, Some(bad)) => {
            body.push_str(
                "## 结论\n\n这条路径**反复失败**——这是一条负面教训：先别照做。\n\n",
            );
            if bad.errors.is_empty() {
                body.push_str("- 没有工具错误，但仍被判定失败（用户重发或未完成）。\n");
            } else {
                for (name, err) in bad.errors.iter().take(8) {
                    body.push_str(&format!("- `{name}` 失败：{err}\n"));
                }
            }
            body.push_str(
                "\n## 该怎么改\n\n换一条路径：确认前置条件（登录态/路径/依赖）再重试，\
                 或改用 `web_extract` / 专用工具替代通用工具。\n",
            );
        }
        (None, None) => return None,
    }

    body.push_str(&format!(
        "\n## 执行统计\n\n- 记录 {} 次：成功 {} 次，失败 {} 次。\n\
         - 参数与错误都已截断；**按当前环境核对**路径/ID/URL 后再执行。\n\
         - 这是自动提炼的骨架，要补充背景用 `skill_manage patch` 增量改写。\n",
        history.len(),
        history
            .iter()
            .filter(|r| r.outcome == TaskOutcome::Succeeded)
            .count(),
        history
            .iter()
            .filter(|r| r.outcome == TaskOutcome::Failed)
            .count(),
    ));

    // 名字由签名派生：同一任务反复 compose 落在同一个技能上，
    // 而不是每次生成一个新技能把技能库撑爆。
    Some(ComposedHarness {
        slug: format!("probe-{sig}"),
        body,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 便捷入口：把一次执行判成 verdict、写进探针仓库、必要时 compose 成 skill。
///
/// 三段的**唯一编排点**——`AgentLoop` 只调这一个函数，不自己拼装步骤，避免
/// 出现"写入端和读取端各自理解一遍流程"的漂移。
pub fn run_task_recursion(
    store: &ProbeStore,
    sig: &str,
    verdict: &Verdict,
    steps: Vec<(String, String)>,
    errors: &[ToolErrorRecord],
    turns: u32,
) -> std::io::Result<Option<ComposedHarness>> {
    store.record(&ProbeRecord::new(sig, verdict, steps, errors, turns))?;
    let history = store.history(sig)?;
    Ok(compose(sig, &history))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_record(sig: &str, tools: &[&str]) -> ProbeRecord {
        ProbeRecord {
            sig: sig.to_string(),
            outcome: TaskOutcome::Succeeded,
            signals: vec!["finished_naturally".into()],
            steps: tools
                .iter()
                .map(|t| (t.to_string(), "{}".to_string()))
                .collect(),
            errors: Vec::new(),
            turns: 2,
            created_at: 1,
        }
    }

    fn fail_record(sig: &str, tools: &[&str], errors: &[(&str, &str)]) -> ProbeRecord {
        ProbeRecord {
            sig: sig.to_string(),
            outcome: TaskOutcome::Failed,
            signals: vec!["user_repeated_request".into()],
            steps: tools
                .iter()
                .map(|t| (t.to_string(), "{}".to_string()))
                .collect(),
            errors: errors
                .iter()
                .map(|(t, e)| (t.to_string(), e.to_string()))
                .collect(),
            turns: 4,
            created_at: 2,
        }
    }

    /// 重发是"上一次失败"的证据，不是"这一次失败"的证据。重试后成功必须判
    /// 成功——否则 Compose 永远看不到"失败→成功"的对比，整个任务级递归失效。
    #[test]
    fn repeated_request_is_recorded_but_does_not_override_a_clean_finish() {
        let v = judge(true, false, &[], false, true);
        assert_eq!(
            v.outcome,
            TaskOutcome::Succeeded,
            "重试后成功就是成功"
        );
        assert!(
            v.has("user_repeated_request"),
            "信号仍要记录，Compose 用它识别这是重试后的成功"
        );

        // 重发 + 没跑完 → 失败（两者一致时才是失败）。
        let f = judge(false, false, &[], false, true);
        assert_eq!(f.outcome, TaskOutcome::Failed);
    }

    #[test]
    fn interrupt_alone_is_unknown_not_failure() {
        let v = judge(false, true, &[], false, false);
        assert_eq!(
            v.outcome,
            TaskOutcome::Unknown,
            "用户主动打断既不是成功也不是失败，不能当教训"
        );
    }

    #[test]
    fn clean_finish_is_success() {
        let v = judge(true, false, &[], false, false);
        assert_eq!(v.outcome, TaskOutcome::Succeeded);
    }

    #[test]
    fn signature_is_stable_under_formatting_noise() {
        assert_eq!(
            task_signature("  帮我把这些照片按日期整理一下。 "),
            task_signature("帮我把这些照片按日期整理一下")
        );
        // 不同任务必须不同签名（否则会把两次无关执行对比在一起）。
        assert_ne!(
            task_signature("帮我把这些照片按日期整理一下"),
            task_signature("帮我把这些视频转码")
        );
    }

    #[test]
    fn single_probe_does_not_compose() {
        let h = vec![ok_record("s1", &["read_file"])];
        assert!(compose("s1", &h).is_none(), "一次执行没有可对比的对象");
    }

    #[test]
    fn success_vs_failure_yields_the_divergence() {
        let h = vec![
            fail_record(
                "s1",
                &["browser_navigate", "browser_click", "terminal"],
                &[("terminal", "Missing 'command' parameter")],
            ),
            ok_record("s1", &["browser_navigate", "browser_click", "read_file"]),
        ];
        let composed = compose("s1", &h).expect("两次记录必须能提炼");
        assert!(composed.body.contains("有效的路径"), "{}", composed.body);
        assert!(composed.body.contains("失败那次做错了什么"), "{}", composed.body);
        assert!(
            composed.body.contains("terminal"),
            "分歧点必须点名失败路径独有的工具: {}",
            composed.body
        );
        assert!(composed.body.contains("read_file"));
        assert_eq!(composed.slug, "probe-s1");
    }

    #[test]
    fn all_failed_composes_a_negative_lesson() {
        let h = vec![
            fail_record("s2", &["terminal"], &[("terminal", "timeout")]),
            fail_record("s2", &["terminal"], &[("terminal", "timeout again")]),
        ];
        let composed = compose("s2", &h).expect("都失败也要给结论");
        assert!(composed.body.contains("负面教训"), "{}", composed.body);
        assert!(composed.body.contains("反复失败"));
    }

    #[test]
    fn store_roundtrip_and_history_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProbeStore::open(tmp.path()).unwrap();
        for i in 0..(PROBES_PER_TASK + 3) {
            let mut r = ok_record("sig", &["read_file"]);
            r.turns = i as u32;
            store.record(&r).unwrap();
        }
        let h = store.history("sig").unwrap();
        assert_eq!(h.len(), PROBES_PER_TASK, "历史必须被压到上限");
        // 保留的是最近的（turns 最大）。
        assert_eq!(h.last().unwrap().turns as usize, PROBES_PER_TASK + 2);
    }

    #[test]
    fn run_task_recursion_composes_on_the_second_run() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProbeStore::open(tmp.path()).unwrap();
        let sig = task_signature("登录超星并完成学习");

        let v1 = judge(false, false, &[], true, false); // 末轮有错 → failed
        let first = run_task_recursion(&store, &sig, &v1, vec![], &[], 3).unwrap();
        assert!(first.is_none(), "第一次执行没有对比对象");

        let v2 = judge(true, false, &[], false, false);
        let second = run_task_recursion(
            &store,
            &sig,
            &v2,
            vec![("browser_navigate".into(), "{}".into())],
            &[],
            2,
        )
        .unwrap();
        assert!(second.is_some(), "第二次执行必须产出提炼结果");
    }
}
