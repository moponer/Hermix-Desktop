use std::time::Instant;

use super::model::{FileEntry, SearchResponse};

const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 5_000;

pub fn search_entries(
    entries: &[FileEntry],
    searchable: &[String],
    query: String,
    limit: Option<usize>,
) -> SearchResponse {
    let start = Instant::now();
    let normalized_query = query.trim().to_lowercase();
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut matches: Vec<FileEntry> = entries
        .iter()
        .zip(searchable.iter())
        .filter(|(_, search_text)| {
            normalized_query.is_empty() || search_text.contains(&normalized_query)
        })
        .map(|(entry, _)| entry.clone())
        .collect();

    if normalized_query.is_empty() {
        matches.sort_by(compare_entries);
    } else {
        matches.sort_by(|a, b| compare_search_entries(a, b, &normalized_query));
    }

    let total_matches = matches.len();
    let results: Vec<FileEntry> = matches.into_iter().take(limit).collect();

    SearchResponse {
        query,
        total_matches,
        returned: results.len(),
        results,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

pub fn compare_entries(a: &FileEntry, b: &FileEntry) -> std::cmp::Ordering {
    b.is_dir
        .cmp(&a.is_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
}

pub fn searchable_text(entry: &FileEntry) -> String {
    entry.name.to_lowercase()
}

fn compare_search_entries(a: &FileEntry, b: &FileEntry, query: &str) -> std::cmp::Ordering {
    search_rank(a, query)
        .cmp(&search_rank(b, query))
        .then_with(|| b.is_dir.cmp(&a.is_dir))
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
}

pub fn search_rank(entry: &FileEntry, query: &str) -> u8 {
    let name = entry.name.to_lowercase();

    if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, name: &str, path: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            id,
            volume: "C:".to_string(),
            file_ref: id,
            parent_ref: 5,
            name: name.to_string(),
            path: path.to_string(),
            extension: None,
            size: None,
            modified: 0,
            is_dir,
            last_usn: 0,
            attributes: if is_dir { 16 } else { 0 },
        }
    }

    #[test]
    fn matches_name_case_insensitively() {
        let entries = vec![
            entry(1, "Cargo.toml", r"C:\repo\Cargo.toml", false),
            entry(2, "main.rs", r"C:\repo\src\main.rs", false),
        ];
        let searchable: Vec<String> = entries.iter().map(searchable_text).collect();

        let by_name = search_entries(&entries, &searchable, "cargo".to_string(), None);
        let by_path = search_entries(&entries, &searchable, "SRC".to_string(), None);

        assert_eq!(by_name.total_matches, 1);
        assert_eq!(by_name.results[0].name, "Cargo.toml");
        assert_eq!(by_path.total_matches, 0);
    }

    #[test]
    fn does_not_match_only_parent_path() {
        let entries = vec![
            entry(1, ".idea", r"G:\Javazuoye\张策202310538041\car\.idea", true),
            entry(
                2,
                "202310538041-张策",
                r"G:\网络期末作业\MY\202310538041-张策",
                true,
            ),
            entry(3, "张策简历.docx", r"G:\docs\张策简历.docx", false),
        ];
        let searchable: Vec<String> = entries.iter().map(searchable_text).collect();

        let response = search_entries(&entries, &searchable, "张策".to_string(), None);

        assert_eq!(response.total_matches, 2);
        assert_eq!(
            response
                .results
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["张策简历.docx", "202310538041-张策"]
        );
        assert!(response
            .results
            .iter()
            .all(|entry| entry.name.contains("张策")));
    }

    #[test]
    fn applies_limit_and_sorts_directories_first() {
        let entries = vec![
            entry(1, "z-file.txt", r"C:\z-file.txt", false),
            entry(2, "a-folder", r"C:\a-folder", true),
            entry(3, "b-file.txt", r"C:\b-file.txt", false),
        ];
        let searchable: Vec<String> = entries.iter().map(searchable_text).collect();

        let response = search_entries(&entries, &searchable, "".to_string(), Some(2));

        assert_eq!(response.total_matches, 3);
        assert_eq!(response.returned, 2);
        assert_eq!(response.results[0].name, "a-folder");
    }

    #[test]
    fn returns_no_matches_cleanly() {
        let entries = vec![entry(1, "Cargo.toml", r"C:\repo\Cargo.toml", false)];
        let searchable: Vec<String> = entries.iter().map(searchable_text).collect();

        let response = search_entries(&entries, &searchable, "missing".to_string(), None);

        assert_eq!(response.total_matches, 0);
        assert!(response.results.is_empty());
    }
}
