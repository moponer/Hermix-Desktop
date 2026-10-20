//! 匹配与排序：只作用于**命中集**，不复制条目、不排序全表。

use std::cmp::Ordering;

use super::model::IndexEntry;

/// 精确名 > 前缀 > 其他子串。
pub fn rank_of(entry: &IndexEntry, query_lower: &str) -> u8 {
    if entry.name_lower == query_lower {
        0
    } else if entry.name_lower.starts_with(query_lower) {
        1
    } else {
        2
    }
}

/// 命中比较：排名 → 目录优先 → 名字 → FRN（保证顺序稳定，避免每次抖动）。
pub fn compare_hits(a: &IndexEntry, b: &IndexEntry, query_lower: &str) -> Ordering {
    rank_of(a, query_lower)
        .cmp(&rank_of(b, query_lower))
        .then_with(|| b.is_dir.cmp(&a.is_dir))
        .then_with(|| a.name_lower.cmp(&b.name_lower))
        .then_with(|| a.file_ref.cmp(&b.file_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file_ref: u64, name: &str, is_dir: bool) -> IndexEntry {
        IndexEntry {
            file_ref,
            parent_ref: 5,
            name: name.to_string(),
            name_lower: name.to_lowercase(),
            modified: 0,
            is_dir,
            last_usn: 0,
            attributes: if is_dir { 16 } else { 0 },
            size: 0,
        }
    }

    #[test]
    fn ranks_exact_then_prefix_then_substring() {
        let exact = entry(1, "main", true);
        let prefix = entry(2, "main.rs", false);
        let other = entry(3, "domain.rs", false);

        assert_eq!(rank_of(&exact, "main"), 0);
        assert_eq!(rank_of(&prefix, "main"), 1);
        assert_eq!(rank_of(&other, "main"), 2);
    }

    #[test]
    fn directories_win_ties_and_order_is_stable() {
        let dir = entry(3, "main", true);
        let file = entry(1, "main", false);
        assert_eq!(compare_hits(&dir, &file, "main"), Ordering::Less);

        let a = entry(1, "main", false);
        let b = entry(2, "main", false);
        assert_eq!(compare_hits(&a, &b, "main"), Ordering::Less);
    }
}
