//! 会话事件日志：手机重连后补齐断连期间错过的事件。
//!
//! ## 为什么重放「已映射的明文帧」而不是重封密文
//!
//! 手机冷启动会换一把新的 P-256 静态密钥（见 `apps/hm/core/session.ts` 的
//! 「冷启动必须重发 key.exchange」）。重封旧密文有两种做法：沿用原 `eid`
//! （HKDF salt 与密文绑定，改不了明文）或换 `eid` 重新做一次 ECDH——后者
//! 等于把一帧的历史重算一遍，代价远大于收益。
//!
//! 所以这里存的是**映射后的 App 事件明文**（`assistant.message` /
//! `tool.start` …），补发时一次性封成一个 `sync.result` 帧。附带两个好处：
//!
//! - 只带一次 nonce / 一次性公钥，而不是 N 帧各带一份；
//! - 天然避开手机侧按 `eid` 去重的窗口——那个窗口是给「逐帧重投」用的，
//!   批量补发若走同一路径会被当成重放整批丢掉。
//!
//! ## 内存有界
//!
//! 每会话默认 512 条，超出丢最旧并把 `truncated` 置真。手机端据此提示
//! 「部分历史已丢失，建议回电脑查看」，而不是假装完整——**缺口要看得见**。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

/// 每会话保留的事件条数上限。
pub const JOURNAL_CAP: usize = 512;

/// 进程级日志表。
///
/// 用全局而不是挂 `HttpServerState`，是因为它的语义本来就是「进程内所有
/// relay 连接共享」——relay 每次重连都会重建 `SessionMux`，日志必须活在
/// 它之外；而进程里只有一条出站 relay 连接，多个实例没有意义。
/// 先例：`ws::SESSION_BROADCASTS` 同样是进程级的 `OnceLock`。
pub fn global() -> Arc<JournalRegistry> {
    static JOURNAL: OnceLock<Arc<JournalRegistry>> = OnceLock::new();
    JOURNAL
        .get_or_init(|| Arc::new(JournalRegistry::new()))
        .clone()
}

/// 一条会话的环形日志。
///
/// `seq` 从 1 开始单调递增，0 保留给「还没同步过任何事件」。
#[derive(Debug, Default)]
struct SessionLog {
    entries: VecDeque<(u64, String)>,
    /// 下一条事件的序号（从 1 开始）。
    next: u64,
    /// 已被淘汰的最旧序号上界：`since_seq` 早于它就意味着缺口。
    dropped_through: u64,
    /// 最近一次出站事件的时刻。桌面端用它判断「手机是不是正在干活」，
    /// 从而决定要不要阻止系统休眠。
    last_push: Option<std::time::Instant>,
}

impl SessionLog {
    fn push_with_seq(&mut self, seq: u64, frame: String, cap: usize) {
        self.next = seq;
        self.last_push = Some(std::time::Instant::now());
        self.entries.push_back((seq, frame));
        while self.entries.len() > cap {
            if let Some((dropped, _)) = self.entries.pop_front() {
                self.dropped_through = dropped;
            }
        }
    }

    /// 取 `since_seq` 之后的事件；第二个返回值表示**中间有缺口**。
    fn since(&self, since_seq: u64, cap: usize) -> (Vec<(u64, String)>, bool) {
        let truncated = since_seq < self.dropped_through;
        let events = self
            .entries
            .iter()
            .filter(|(seq, _)| *seq > since_seq)
            .take(cap)
            .map(|(seq, frame)| (*seq, frame.clone()))
            .collect();
        (events, truncated)
    }
}

/// 全部会话的日志表。进程级持有——它的生命周期必须**长于** relay 连接，
/// 否则一次断线重连就把待补发的事件一起丢了（那正是要修的问题）。
#[derive(Debug)]
pub struct JournalRegistry {
    logs: Mutex<HashMap<String, SessionLog>>,
    cap: usize,
}

impl Default for JournalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl JournalRegistry {
    pub fn new() -> Self {
        Self::with_cap(JOURNAL_CAP)
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            logs: Mutex::new(HashMap::new()),
            cap: cap.max(1),
        }
    }

    /// 记一条出站事件，返回它的序号。
    pub fn push(&self, sid: &str, frame: String) -> u64 {
        self.append(sid, |_| frame).0
    }

    /// 先分配序号、再由调用方把序号写进帧内容，返回 `(序号, 帧)`。
    ///
    /// 出站路径要的是这个：`seq` 必须**在帧里**（手机据此记 last_seq），
    /// 而帧又要原样存进日志，所以序号得先于序列化拿到。
    pub fn append<F>(&self, sid: &str, build: F) -> (u64, String)
    where
        F: FnOnce(u64) -> String,
    {
        let mut g = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        let cap = self.cap;
        let log = g.entry(sid.to_string()).or_default();
        let seq = log.next + 1;
        let frame = build(seq);
        log.push_with_seq(seq, frame.clone(), cap);
        (seq, frame)
    }

    /// 取 `since_seq` 之后的事件 + 是否有缺口。
    pub fn since(&self, sid: &str, since_seq: u64) -> (Vec<(u64, String)>, bool) {
        let g = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(sid) {
            Some(log) => log.since(since_seq, self.cap),
            None => (Vec::new(), false),
        }
    }

    /// 会话结束（`Close` 帧）时丢掉它的事件，避免表无限增长。
    ///
    /// **刻意的：保留序号游标**。若把整条记录删掉，`seq` 会从 1 重新开始，
    /// 手机攥着 `last_seq=50` 再连上来时既拿不到补发、也看不到缺口——
    /// 比直接告诉它「这段没了」更糟。
    pub fn clear(&self, sid: &str) {
        let mut g = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(log) = g.get_mut(sid) {
            log.dropped_through = log.next;
            log.entries.clear();
            log.last_push = None;
        }
    }

    /// 最近 `window` 内是否有任何会话产生过事件——即「手机正在干活」。
    ///
    /// 桌面端用它参与防休眠判定：`keep_awake` 开关原本只认桌面自己的流式状态，
    /// 手机在跑的时候电脑照样会睡，一轮任务做到一半被挂起。
    ///
    /// 刻意**不**用「有没有设备连着」当判据：手机连着但闲着时，没有任何理由
    /// 阻止电脑休眠。
    pub fn active_within(&self, window: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        let g = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        g.values()
            .any(|l| l.last_push.is_some_and(|t| now.duration_since(t) <= window))
    }

    /// 某会话当前保留的条数（诊断 / 测试用）。
    pub fn len(&self, sid: &str) -> usize {
        let g = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        g.get(sid).map_or(0, |l| l.entries.len())
    }

    pub fn is_empty(&self, sid: &str) -> bool {
        self.len(sid) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_starts_at_one_and_is_monotonic() {
        let j = JournalRegistry::new();
        assert_eq!(j.push("s", "a".into()), 1);
        assert_eq!(j.push("s", "b".into()), 2);
        assert_eq!(j.push("s", "c".into()), 3);
    }

    #[test]
    fn sessions_are_isolated() {
        let j = JournalRegistry::new();
        j.push("a", "a1".into());
        j.push("b", "b1".into());
        let (ev, _) = j.since("a", 0);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1, "a1");
        assert_eq!(j.len("b"), 1);
    }

    #[test]
    fn since_returns_only_newer_events() {
        let j = JournalRegistry::new();
        for f in ["a", "b", "c", "d"] {
            j.push("s", f.into());
        }
        let (ev, truncated) = j.since("s", 2);
        assert!(!truncated, "没被覆盖就不该报缺口");
        let frames: Vec<&str> = ev.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(frames, vec!["c", "d"]);
    }

    #[test]
    fn since_zero_returns_everything_retained() {
        let j = JournalRegistry::new();
        j.push("s", "a".into());
        j.push("s", "b".into());
        assert_eq!(j.since("s", 0).0.len(), 2);
    }

    /// 容量溢出后，请求一个早于保留窗口的 seq 必须**明确报缺口**，
    /// 而不是安静地给一份看起来完整的列表。
    #[test]
    fn overflow_reports_truncation() {
        let j = JournalRegistry::with_cap(3);
        for f in ["a", "b", "c", "d", "e"] {
            j.push("s", f.into());
        }
        assert_eq!(j.len("s"), 3, "窗口必须有界，保留 seq 3/4/5");

        let (ev, truncated) = j.since("s", 1);
        assert!(truncated, "since=1 落在被丢弃的区间（a/b 已淘汰），必须报缺口");
        let frames: Vec<&str> = ev.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(frames, vec!["c", "d", "e"]);

        // 手机说「我已有 seq 3」，就只补 4、5；且没有缺口
        // （3 之后紧接 4，被淘汰的只有 1、2）。
        let (ev, truncated) = j.since("s", 3);
        assert!(!truncated, "since=3 之后没有断档，不该报缺口");
        let frames: Vec<&str> = ev.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(frames, vec!["d", "e"]);
    }

    #[test]
    fn unknown_session_yields_empty_without_truncation() {
        let j = JournalRegistry::new();
        let (ev, truncated) = j.since("nope", 5);
        assert!(ev.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn 新会话不算活跃_出过事件才算() {
        let j = JournalRegistry::new();
        assert!(!j.active_within(std::time::Duration::from_secs(60)));

        j.push("s", "a".into());
        assert!(j.active_within(std::time::Duration::from_secs(60)));

        j.clear("s");
        assert!(!j.active_within(std::time::Duration::from_secs(60)), "会话结束后不该再算活跃");
    }

    #[test]
    fn clear_drops_events_but_keeps_the_cursor() {
        let j = JournalRegistry::new();
        j.push("s", "a".into());
        j.push("s", "b".into());
        j.clear("s");
        assert!(j.is_empty("s"), "事件应被丢掉");

        // 序号游标不重置：手机攥着 last_seq=2 再连上来，得到「空 + 无缺口」；
        // 若它只看到 1，说明中间那段确实没了，必须报缺口。
        let (ev, truncated) = j.since("s", 2);
        assert!(ev.is_empty());
        assert!(!truncated);

        let (_, truncated) = j.since("s", 1);
        assert!(truncated, "被 clear 掉的那一段必须报缺口");

        assert_eq!(j.push("s", "c".into()), 3, "序号接着走，不回到 1");
    }
}
