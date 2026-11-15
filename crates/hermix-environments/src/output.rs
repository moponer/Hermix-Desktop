//! 终端输出的字节上限截断。
//!
//! 四个后端（local / local_isolation / docker / ssh）各自实现过一遍「超过
//! `max_output_size` 就截断」，其中三份直接对 `String` 做 `s[..max]` —— 这在
//! 截断点落在多字节字符中间时 panic。终端输出是任意字节流，`cat 中文日志`、
//! `git log` 里的 emoji 都能触发，症状是工具任务整体崩掉（被 JoinSet 收成
//! 「工具崩溃」，命令的全部输出丢失）。

/// 按字节上限截断字符串，**退到最近的 UTF-8 字符边界**。
pub(crate) fn truncate_to_boundary(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s
}

/// 字节序列的同类截断：退到 UTF-8 起始字节，避免解码时出现半个字符。
///
/// GBK 等双字节编码不走这个判断（首尾字节的范围与 UTF-8 不同），最坏结果只是
/// 末尾多一个替换字符 —— 不会 panic，这也是这条路径的底线。
pub(crate) fn truncate_bytes_to_boundary(mut v: Vec<u8>, max_bytes: usize) -> Vec<u8> {
    if v.len() <= max_bytes {
        return v;
    }
    let mut cut = max_bytes;
    while cut > 0 && (v[cut] & 0xC0) == 0x80 {
        cut -= 1;
    }
    v.truncate(cut);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_at_char_boundary_not_byte() {
        // 「中」是 3 字节；上限 4 落在第二个字中间。
        let out = truncate_to_boundary("中文字".to_string(), 4);
        assert_eq!(out, "中", "必须退到字符边界，不能切出半个字");
        assert_eq!(truncate_to_boundary("abc".to_string(), 10), "abc");
    }

    #[test]
    fn emoji_at_limit_does_not_panic() {
        // emoji 4 字节；上限 5 落在第二个 emoji 中间。
        let out = truncate_to_boundary("😀😀".to_string(), 5);
        assert_eq!(out, "😀");
    }

    #[test]
    fn byte_truncation_skips_continuation_bytes() {
        let bytes = "中文".as_bytes().to_vec();
        let out = truncate_bytes_to_boundary(bytes, 4);
        assert_eq!(out, "中".as_bytes(), "不能留下孤立的续字节");
    }
}
