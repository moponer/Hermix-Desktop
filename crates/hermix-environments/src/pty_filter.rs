//! VT/ANSI 输出过滤器 —— ConPTY 输出喂给模型前的固有降噪层。
//!
//! ConPTY 经 win32 屏幕缓冲区把子进程输出**重编码**为 VT 流（非透传），
//! 会合成大量对 LLM 纯属噪音的控制序列（真机探针
//! `tests/pty_conpty_probe.rs` 钉死）：
//!
//! - `ESC[m`（SGR reset）、`ESC[?25h/l`（光标显隐）等 CSI 序列
//! - `ESC]0;C:\...\powershell.exe BEL`（OSC 标题变更，随 cwd/进程名变化）
//! - 启动时的 `ESC[6n`（**DSR 光标位置请求**——必须由输入管道回复
//!   `ESC[1;1R`，否则 conhost 永远不渲染，cmd/PowerShell 全部静默卡死）
//!
//! 本模块只做 VT 机械层面的剥离；prompt-marker 的剥离在会话层
//! （`pty_session`）完成，因为它需要知道具体 marker 串。
//!
//! 编码：ConPTY 输出流按设计恒为 UTF-8（子进程的 OEM/GBK 字节先被
//! conhost 解码进 Unicode 缓冲区再重编码），探针实测中文完好。跨 feed
//! 的不完整 UTF-8 尾字节在本模块内续接。

/// VT 流状态机的剥离结果事件。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FilterEvents {
    /// 本次 feed 中观察到 `ESC[6n`（DSR 光标位置请求）的次数。
    /// 调用方（会话读线程）须在输入管道回复 `ESC[1;1R`，否则 ConPTY
    /// 首帧渲染永远不开始。
    pub dsr_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 正文：字节直接进文本缓冲。
    Ground,
    /// 收到 `ESC`，等下一个字节判别序列类型。
    Esc,
    /// `ESC [`，等 final byte（0x40-0x7E）；0x20-0x3F 为中间/参数字节。
    Csi,
    /// `ESC ]`（OSC）——BEL 或 `ESC \` 终止。
    Osc,
    /// OSC 内收到 `ESC`，等 `\` 确认 ST。
    OscEsc,
    /// `ESC P/X/^/_`（DCS/SOS/PM/APC 字符串）——`ESC \` 终止。
    Str,
    /// 字符串序列内收到 `ESC`，等 `\` 确认 ST。
    StrEsc,
    /// `ESC ( ) # % ` 等，再消费一个字节后回 Ground。
    EscIntermediate,
}

/// 增量 VT 过滤器。状态跨 feed 持续，可安全地按任意块边界喂数据。
pub struct VtFilter {
    state: State,
    /// Ground 态攒下的文本字节（待解码）。
    text: Vec<u8>,
    /// CRLF 归一：刚见过 `CR`（若下一字节是 `LF` 则合并，孤立 CR 丢弃）。
    saw_cr: bool,
    /// CSI 参数区攒字节（判别 DSR 用）。
    csi_params: Vec<u8>,
    /// 累计 DSR 请求数（跨 feed；`take_dsr_requests` 消费）。
    dsr_total: u64,
}

impl Default for VtFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl VtFilter {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            text: Vec::new(),
            saw_cr: false,
            csi_params: Vec::new(),
            dsr_total: 0,
        }
    }

    /// 喂入原始字节，把解码后的干净文本追加到 `out`。
    /// 返回本批观察到的事件（DSR 请求等）。
    pub fn feed(&mut self, bytes: &[u8], out: &mut String) -> FilterEvents {
        let mut events = FilterEvents::default();
        for &b in bytes {
            self.step(b, &mut events);
        }
        self.decode_text(out);
        events
    }

    /// 会话关闭时冲刷残余（不完整序列/尾字节按丢弃处理）。
    pub fn flush(&mut self, out: &mut String) {
        self.decode_text(out);
    }

    /// 消费累计的 DSR 请求数。
    pub fn take_dsr_requests(&mut self) -> u64 {
        std::mem::take(&mut self.dsr_total)
    }

    fn step(&mut self, b: u8, events: &mut FilterEvents) {
        match self.state {
            State::Ground => match b {
                0x1b => self.state = State::Esc,
                0x0d => self.saw_cr = true,
                0x0a => {
                    // CRLF → '\n'；孤立 LF 同样是换行。
                    self.saw_cr = false;
                    self.text.push(b'\n');
                }
                0x09 => self.text.push(b'\t'),
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1a | 0x1c..=0x1f | 0x7f => {
                    // 其余控制字符（BEL、退格、ENQ…）对模型是噪音，丢弃。
                    self.saw_cr = false;
                }
                _ => {
                    if self.saw_cr {
                        // 孤立 CR（进度条重绘）：丢弃 CR 本身，后续文本
                        // 拼接——不做屏幕缓冲仿真（全屏 TUI 明确不支持）。
                        self.saw_cr = false;
                    }
                    self.text.push(b);
                }
            },
            State::Esc => match b {
                b'[' => {
                    self.csi_params.clear();
                    self.state = State::Csi;
                }
                b']' => self.state = State::Osc,
                b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
                0x1b => {} // 连续 ESC：保持 Esc
                b'(' | b')' | b'#' | b'%' | b' ' => self.state = State::EscIntermediate,
                _ => self.state = State::Ground, // 单字节转义（ESC 7 等），吞掉
            },
            State::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    // final byte：判别 DSR 请求。
                    if b == b'n' && self.csi_params == b"6" {
                        events.dsr_requests += 1;
                        self.dsr_total += 1;
                    }
                    self.state = State::Ground;
                } else if b == 0x1b {
                    self.state = State::Esc; // 畸形序列：重新起始
                } else if (0x20..=0x3f).contains(&b) {
                    self.csi_params.push(b); // 参数（含 '?' 私有前缀）与中间字节
                } else {
                    self.state = State::Ground;
                }
            }
            State::Osc | State::Str => match b {
                0x07 if self.state == State::Osc => self.state = State::Ground, // BEL 终止 OSC
                0x1b => {
                    self.state = if self.state == State::Osc {
                        State::OscEsc
                    } else {
                        State::StrEsc
                    };
                }
                _ => {}
            },
            State::OscEsc | State::StrEsc => {
                self.state = if b == b'\\' {
                    State::Ground
                } else if self.state == State::OscEsc {
                    State::Osc // 畸形：回到字符串态继续吞
                } else {
                    State::Str
                };
            }
            State::EscIntermediate => self.state = State::Ground,
        }
    }

    /// 把攒下的文本字节解码进 `out`。不完整的 UTF-8 尾序列留在缓冲里
    /// 等下一批补全（ConPTY 流为 UTF-8）。
    fn decode_text(&mut self, out: &mut String) {
        if self.text.is_empty() {
            return;
        }
        let (ready, keep) = match std::str::from_utf8(&self.text) {
            Ok(_) => (self.text.as_slice(), &[][..]),
            Err(e) => match e.error_len() {
                None => {
                    // 不完整尾序列：解码前缀，尾字节续接。
                    let v = e.valid_up_to();
                    (&self.text[..v], &self.text[v..])
                }
                Some(_) => {
                    // 非法字节（不应出现于 ConPTY 流）：解码前缀，丢弃
                    // 本批剩余——宁可出 U+FFFD 也不阻塞流。
                    let v = e.valid_up_to();
                    (&self.text[..v], &[][..])
                }
            },
        };
        if !ready.is_empty() {
            out.push_str(&String::from_utf8_lossy(ready));
        }
        let kept = keep.to_vec();
        self.text = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> String {
        let mut f = VtFilter::new();
        let mut out = String::new();
        for c in chunks {
            f.feed(c, &mut out);
        }
        f.flush(&mut out);
        out
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(feed_all(&[b"hello world\r\n"]), "hello world\n");
    }

    #[test]
    fn crlf_normalized_and_lone_cr_dropped() {
        // CRLF → \n；孤立 CR（进度条）丢弃，文本拼接。
        assert_eq!(feed_all(&[b"a\r\nb\rc"]), "a\nbc");
    }

    #[test]
    fn osc_title_stripped() {
        // 探针实测：ESC]0;C:\...\cmd.exe BEL
        let raw = b"\x1b]0;C:\\WINDOWS\\system32\\cmd.exe\x07hello\r\n";
        assert_eq!(feed_all(&[raw]), "hello\n");
    }

    #[test]
    fn osc_with_st_terminator_stripped() {
        let raw = b"\x1b]0;title\x1b\\body";
        assert_eq!(feed_all(&[raw]), "body");
    }

    #[test]
    fn csi_cursor_and_sgr_stripped() {
        // 探针实测：ESC[m、ESC[?25h/l、光标移动。
        let raw = b"\x1b[m\x1b[?25hPS>\x1b[?25l\x1b[2Jclean";
        assert_eq!(feed_all(&[raw]), "PS>clean");
    }

    #[test]
    fn dcs_string_stripped() {
        let raw = b"\x1bP1;2|data\x1b\\end";
        assert_eq!(feed_all(&[raw]), "end");
    }

    #[test]
    fn dsr_request_counted() {
        let mut f = VtFilter::new();
        let mut out = String::new();
        let ev = f.feed(b"\x1b[6n", &mut out);
        assert_eq!(ev.dsr_requests, 1);
        let ev = f.feed(b"\x1b[6n\x1b[?6n", &mut out);
        assert_eq!(ev.dsr_requests, 1, "?6n 不是标准 DSR 请求，不计数");
        assert_eq!(f.take_dsr_requests(), 2);
        assert_eq!(f.take_dsr_requests(), 0);
    }

    #[test]
    fn escape_sequence_split_across_feeds() {
        // OSC 标题拆在两批之间；CSI 拆在三批之间。
        let out = feed_all(&[b"\x1b]0;C:\\win", b"dows\\cmd.exe\x07va", b"\x1b[", b"?25", b"hlue"]);
        assert_eq!(out, "value");
    }

    #[test]
    fn utf8_multibyte_split_across_feeds() {
        // 中文（3 字节/字）拆在批边界。
        let s = "中文输出测试";
        let bytes = s.as_bytes();
        let mid = 4; // 落在第二个字中间
        let out = feed_all(&[&bytes[..mid], &bytes[mid..]]);
        assert_eq!(out, s);
    }

    #[test]
    fn tab_kept_and_control_chars_dropped() {
        let raw = b"a\tb\x07c\x08d\x7fe";
        assert_eq!(feed_all(&[raw]), "a\tbcde");
    }

    #[test]
    fn bell_terminated_osc_does_not_swallow_text() {
        // 探针实测形态：OSC 标题紧跟正文，BEL 必须正确终止而非吞正文。
        let raw = b"\x1b]0;T\x07\x1b[mOUT";
        assert_eq!(feed_all(&[raw]), "OUT");
    }

    #[test]
    fn charset_designation_stripped() {
        let raw = b"\x1b(Btext";
        assert_eq!(feed_all(&[raw]), "text");
    }
}
