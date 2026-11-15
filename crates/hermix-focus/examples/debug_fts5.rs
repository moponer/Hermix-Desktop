//! Debug FTS5 directly.

use hermix_focus::{DynMemoryLayer, EmbeddedMemoryBackend, MemoryLayer, MemoryRouter};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_db_path(label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "hermix-debug-{label}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    p.push("memory.db");
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::remove_file(&p);
    p
}

fn main() {
    println!("Debug FTS5 indexing");
    let path = temp_db_path("fts5");
    println!("DB: {}", path.display());

    let embedded = Arc::new(EmbeddedMemoryBackend::open(&path).expect("open"));
    let dyn_e: DynMemoryLayer = embedded.clone();
    let router = MemoryRouter::new_with_embedded(vec![dyn_e], embedded.clone());

    router.sync_turn("We start ProjectX, a Rust rewrite.", "OK, noted.", "sess-A");
    router.sync_turn("ProjectX uses tokio for async runtime.", "Noted: tokio.", "sess-A");

    let conn = Connection::open(&path).expect("open conn");
    println!("\n--- frames table ---");
    let mut stmt = conn.prepare("SELECT id, payload, salience FROM frames ORDER BY rowid").unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let payload: String = row.get(1).unwrap();
        let salience: f64 = row.get(2).unwrap();
        println!("  id={} salience={} payload={}", &id[..8], salience, payload);
    }

    println!("\n--- frames_fts table ---");
    let mut stmt = conn.prepare("SELECT id, substr(payload_text, 1, 200), substr(tags_text, 1, 100) FROM frames_fts").unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let text: String = row.get(1).unwrap();
        let tags: String = row.get(2).unwrap();
        println!("  id={} text='{}' tags='{}'", &id[..8], text, tags);
    }

    println!("\n--- raw FTS5 queries ---");
    let queries = [
        "ProjectX",
        "\"ProjectX\"",
        "projectx",
        "tokio",
        "Rust",
        "ProjectX tokio Rust",
        "\"ProjectX\" OR \"tokio\" OR \"Rust\"",
        "rewrite",
    ];
    for q in queries {
        let mut stmt = conn
            .prepare("SELECT id FROM frames_fts WHERE frames_fts MATCH ?1 LIMIT 10")
            .unwrap();
        let result: Result<Vec<String>, _> = stmt
            .query_map([q], |r| r.get::<_, String>(0))
            .unwrap()
            .collect();
        match result {
            Ok(ids) => {
                let short_ids: Vec<String> =
                    ids.iter().map(|s| s[..8].to_string()).collect();
                println!("  MATCH '{}' -> {:?} ({} hits)", q, short_ids, ids.len());
            }
            Err(e) => println!("  MATCH '{}' -> ERROR: {}", q, e),
        }
    }

    println!("\n--- JOIN with frames (visibility filter) ---");
    let mut stmt = conn
        .prepare(
            "SELECT f.id FROM frames_fts f JOIN frames fr ON fr.id = f.id
             WHERE frames_fts MATCH ?1 AND fr.visibility = 1 LIMIT 10",
        )
        .unwrap();
    for q in &queries[..5] {
        let result: Result<Vec<String>, _> = stmt
            .query_map([q], |r| r.get::<_, String>(0))
            .unwrap()
            .collect();
        match result {
            Ok(ids) => println!("  JOIN MATCH '{}' -> {} hits", q, ids.len()),
            Err(e) => println!("  JOIN MATCH '{}' -> ERROR: {}", q, e),
        }
    }
}