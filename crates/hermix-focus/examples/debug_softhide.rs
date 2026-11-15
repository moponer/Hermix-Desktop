use hermix_focus::{DynMemoryLayer, EmbeddedMemoryBackend, MemoryLayer, MemoryRouter};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_db_path(label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("hermix-debug-{label}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    p.push("memory.db");
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::remove_file(&p);
    p
}

fn main() {
    let path = temp_db_path("softhide");
    println!("DB: {}", path.display());
    let embedded = Arc::new(EmbeddedMemoryBackend::open(&path).expect("open"));
    let dyn_e: DynMemoryLayer = embedded.clone();
    let router = MemoryRouter::new_with_embedded(vec![dyn_e], embedded.clone());
    router.sync_turn("ProjectX uses tokio for async runtime.", "Noted: tokio.", "sess-A");

    let conn = Connection::open(&path).expect("open conn");
    println!("\nBefore soft-hide:");
    let mut stmt = conn.prepare("SELECT id, payload, tags, visibility FROM frames").unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let payload: String = row.get(1).unwrap();
        let tags: String = row.get(2).unwrap();
        let vis: i64 = row.get(3).unwrap();
        println!("  id={} vis={} tags={} payload.len={}", &id[..8], vis, tags, payload.len());
    }

    println!("\nCalling on_memory_write('remove', 'session:sess-A', 'ProjectX uses tokio')...");
    router.on_memory_write("remove", "session:sess-A", "ProjectX uses tokio");

    println!("\nAfter soft-hide:");
    let mut stmt = conn.prepare("SELECT id, payload, tags, visibility FROM frames").unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let payload: String = row.get(1).unwrap();
        let tags: String = row.get(2).unwrap();
        let vis: i64 = row.get(3).unwrap();
        println!("  id={} vis={} tags={} payload.len={}", &id[..8], vis, tags, payload.len());
    }

    // Manually run the SQL
    println!("\nManual SQL test:");
    let now = chrono::Utc::now().to_rfc3339();
    let pattern = "%ProjectX uses tokio%";
    let n = conn.execute(
        "UPDATE frames SET visibility = 0, hidden_at = ?1
         WHERE visibility = 1
           AND (
               tags LIKE ?2
               OR tags LIKE ?3
               OR tags LIKE ?4
               OR tags LIKE ?5
           )
           AND payload LIKE ?6",
        rusqlite::params![now, "%\"target:session:sess-A\"%", "%target:session:sess-A%", "%\"session:session:sess-A\"%", "%session:session:sess-A%", pattern],
    ).unwrap_or(0);
    println!("  rows updated by manual SQL = {}", n);

    println!("\nFinal state:");
    let mut stmt = conn.prepare("SELECT id, visibility FROM frames").unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let vis: i64 = row.get(1).unwrap();
        println!("  id={} vis={}", &id[..8], vis);
    }
}
