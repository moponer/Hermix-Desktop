use hermix_focus::{
    DynMemoryLayer, EmbeddedMemoryBackend, FocusConclusion, FocusFrame, FocusStack,
    MemoryLayer, MemoryQuery, MemoryRouter,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_db_path(label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "hermix-memory-e2e-{label}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    p.push("memory.db");
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::remove_file(&p);
    p
}

fn header(s: &str) {
    println!("\n{}", "=".repeat(78));
    println!("  {s}");
    println!("{}", "=".repeat(78));
}
fn ok(s: &str) {
    println!("    [PASS] {s}");
}
fn fail(s: &str) {
    println!("    [FAIL] {s}");
}

async fn count_visible_frames(backend: &Arc<EmbeddedMemoryBackend>) -> i64 {
    let q = MemoryQuery::recall("").with_limit(1000);
    backend.recognize(&q).await.map(|v| v.len() as i64).unwrap_or(-1)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("Hermix memory layer - end-to-end demo");
    header("Setup");
    let path = temp_db_path("main");
    println!("  DB path: {}", path.display());
    let embedded = Arc::new(EmbeddedMemoryBackend::open(&path).expect("open embedded"));
    let dyn_embedded: DynMemoryLayer = embedded.clone();
    let router = MemoryRouter::new_with_embedded(vec![dyn_embedded], embedded.clone());
    println!("  Router backends: {}", router.backend_count());
    let embedded: Arc<EmbeddedMemoryBackend> = router
        .find_embedded_backend()
        .expect("embedded backend must be wired");

    header("Scenario 1: sync_turn -> DB row -> FTS5 index");
    let turns: [(&str, &str); 5] = [
        ("We start ProjectX, a Rust rewrite.", "OK, noted ProjectX is a Rust rewrite."),
        ("ProjectX uses tokio for async runtime.", "Noted: ProjectX uses tokio."),
        ("We decide not to use any external memory tools.", "OK, no external memory dep."),
        ("ProjectX storage layer is SQLite single file.", "SQLite single file, understood."),
        ("Tomorrow we demo ProjectX progress to the boss.", "Noted: demo ProjectX tomorrow."),
    ];
    for (i, (u, a)) in turns.iter().enumerate() {
        router.sync_turn(u, a, "sess-proj-A");
        println!("    Turn {i}: synced");
    }
    let n_frames = count_visible_frames(&embedded).await;
    println!("  Visible frames: {n_frames}");
    if (4..=6).contains(&n_frames) {
        ok("sync_turn persisted frames");
    } else {
        fail(&format!("sync_turn produced {n_frames} frames"));
    }
    let q = MemoryQuery::recall("ProjectX tokio Rust").with_limit(10);
    let hits = router.recognize(&q).await.expect("recognize");
    println!("  Recognize('ProjectX tokio Rust') -> {} hits", hits.len());
    if hits.iter().any(|h| h.content.contains("ProjectX")) {
        ok("FTS5 multi-token recall works");
    } else {
        fail("FTS5 multi-token recall broken");
    }

    header("Scenario 2: Time-aware recall");
    use hermix_focus::extract_time_window;
    let cases = [
        ("Tomorrow demo what", "expect tomorrow window (UNSUPPORTED)"),
        ("What did we talk about yesterday", "expect yesterday window"),
        ("What was discussed last week", "expect last-week window"),
        ("ProjectX progress", "expect no time anchor"),
        ("the bug from 3 days ago", "expect 3-days-ago window"),
    ];
    for (q, expected) in cases {
        match extract_time_window(q) {
            Some(w) => println!("    '{}' -> [{} .. {}] ({})", q, w.start.format("%Y-%m-%d %H:%M"), w.end.map(|e| e.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(), expected),
            None => println!("    '{}' -> NO window ({})", q, expected),
        }
    }

    header("Scenario 3: FocusStack -> upsert_conclusion");
    let mut stack: FocusStack<String> = FocusStack::new();
    let _f1_id = stack.push(FocusFrame::new("task A".to_string()));
    let f2_id = stack.push(FocusFrame::new("task B (Cleared)".to_string()));
    let _f3_id = stack.push(FocusFrame::new("task C".to_string()));
    let _ = stack.pop();
    if let Some(frame) = stack.get_mut(f2_id) {
        frame.mark_clear();
    }
    let _ = stack.pop();
    let conclusions: Vec<FocusConclusion> = stack.drain_conclusions();
    let n_before = count_visible_frames(&embedded).await;
    for c in &conclusions {
        let _ = router.upsert_conclusion(c).await;
    }
    let n_after = count_visible_frames(&embedded).await;
    println!("  frames: before={n_before}, after={n_after}");
    if n_after >= n_before {
        ok("upsert_conclusion did not lose data");
    } else {
        fail("upsert_conclusion reduced frame count");
    }

    header("Scenario 4: on_pre_compress");
    let msgs = vec![
        json!({"role": "user", "content": "Where did we leave off with ProjectX"}),
        json!({"role": "assistant", "content": "ProjectX is Rust rewrite with tokio"}),
    ];
    let note = router.on_pre_compress(&msgs);
    println!("  on_pre_compress returned {} chars", note.len());
    if !note.is_empty() && note.contains("ProjectX") {
        ok("on_pre_compress surfaces recent turns");
    } else {
        fail("on_pre_compress empty");
    }

    header("Scenario 5: on_session_end");
    let session_msgs = vec![
        json!({"role": "user", "content": "Today we discussed ProjectX deployment"}),
        json!({"role": "assistant", "content": "ProjectX deployment: systemd"}),
    ];
    let n_before = count_visible_frames(&embedded).await;
    router.on_session_end(&session_msgs);
    let n_after = count_visible_frames(&embedded).await;
    println!("  frames: before={n_before}, after={n_after}");
    if n_after > n_before {
        ok("on_session_end added a frame");
    } else {
        fail("on_session_end did not add");
    }

    header("Scenario 6: Cross-session recall");
    let q = MemoryQuery::recall("ProjectX deployment").with_limit(5);
    let hits = router.recognize(&q).await.expect("recognize");
    println!("  Recognize('ProjectX deployment') -> {} hits", hits.len());
    if hits.iter().any(|h| h.content.contains("ProjectX")) {
        ok("cross-session recall works");
    } else {
        fail("cross-session recall broken");
    }

    header("Scenario 7: collect_system_prompt_blocks");
    let block = router.collect_system_prompt_blocks();
    println!("  Length: {} chars", block.len());
    if !block.is_empty() {
        ok("system prompt block non-empty");
    } else {
        fail("system prompt block EMPTY");
    }

    header("Scenario 8: Soft-hide via on_memory_write('remove')");
    let q = MemoryQuery::recall("ProjectX tokio").with_limit(10);
    let hits_before = router.recognize(&q).await.expect("recognize");
    println!("  Before soft-hide: {} hits", hits_before.len());
    router.on_memory_write("remove", "session:sess-proj-A", "ProjectX uses tokio for async runtime.");
    let hits_after = router.recognize(&q).await.expect("recognize after");
    println!("  After soft-hide: {} hits", hits_after.len());
    if hits_after.len() < hits_before.len() {
        ok("soft-hide filtered a matching turn frame");
    } else {
        fail(&format!("soft-hide did not filter ({} -> {})", hits_before.len(), hits_after.len()));
    }

    header("Summary");
    let final_frames = count_visible_frames(&embedded).await;
    println!("  Final visible frames: {final_frames}");
    println!("  DB: {}", path.display());
}
