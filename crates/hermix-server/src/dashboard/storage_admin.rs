//! 存储维护端点（本机专用）：库体积事实 + 人工触发的空间回收。
//!
//! 为什么不自动清理：`messages` / `token_usage` 只增不减是**产品语义**（历史
//! 要能回看）。自动删数据必须先有"保留多久"的决策；在那之前，能给用户的是
//! **事实**（库多大、可回收多少页）与**一个他自己点的按钮**。
//!
//! 背景数据（2026-10-30 实测）：`sessions.db` 55.2 MB + `-wal` 12.0 MB，全仓
//! 零 `VACUUM`、零保留期 —— 删会话后文件不会缩水。

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::{HttpError, HttpServerState};

fn sessions_db(state: &HttpServerState) -> std::path::PathBuf {
    hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db")
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// GET /api/storage/stats — 体积与页统计（只读）。
pub async fn stats(State(state): State<HttpServerState>) -> Json<Value> {
    let db = sessions_db(&state);
    let wal = db.with_extension("db-wal");
    let probe_path = db.clone();
    // PRAGMA 是同步 SQLite：放阻塞池，别钉住 worker。
    let pages = tokio::task::spawn_blocking(move || {
        let conn =
            hermix_store::open_connection(&probe_path, hermix_store::OpenMode::Read).ok()?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).ok()?;
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).ok()?;
        let freelist: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0)).ok()?;
        Some((page_size, page_count, freelist))
    })
    .await
    .ok()
    .flatten();

    let (page_size, page_count, freelist) = pages.unwrap_or((0, 0, 0));
    Json(json!({
        "db_path": db.display().to_string(),
        "db_bytes": file_len(&db),
        "wal_bytes": file_len(&wal),
        "page_size": page_size,
        "page_count": page_count,
        "freelist_pages": freelist,
        // `VACUUM` 能还给文件系统的近似值（页大小 × 空闲页）。
        "reclaimable_bytes": page_size.saturating_mul(freelist),
    }))
}

/// POST /api/storage/vacuum — 人工触发 `VACUUM` + WAL 截断。
///
/// 这是**唯一**会缩文件的操作，因此：本机专用（`ConfigMutation` 能力位）、
/// 单飞（并发请求直接 409）、跑在阻塞池。VACUUM 需要独占锁，库正被写入时会
/// 等 `busy_timeout`（5s）后失败，前端可重试。
pub async fn vacuum(
    State(state): State<HttpServerState>,
) -> Result<Json<Value>, HttpError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(HttpError {
            status: StatusCode::CONFLICT,
            message: "已有一个空间回收任务在进行".to_string(),
        });
    }

    let db = sessions_db(&state);
    let before = file_len(&db);
    let work_path = db.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let conn = hermix_store::open_connection(&work_path, hermix_store::OpenMode::Write)
            .map_err(|e| e.to_string())?;
        // VACUUM 不能在事务里跑；execute_batch 是 autocommit。
        conn.execute_batch("VACUUM;").map_err(|e| e.to_string())?;
        // VACUUM 之后 WAL 里只剩收尾语句，截断到 0 字节。
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    })
    .await;

    RUNNING.store(false, Ordering::SeqCst);

    match result {
        Ok(Ok(())) => {
            let after = file_len(&db);
            tracing::info!(before, after, "storage: vacuum done");
            Ok(Json(json!({
                "ok": true,
                "before_bytes": before,
                "after_bytes": after,
                "reclaimed_bytes": before.saturating_sub(after),
            })))
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "storage: vacuum failed");
            Err(HttpError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: "空间回收失败（库可能正被写入，请稍后重试）".to_string(),
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "storage: vacuum task failed");
            Err(HttpError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: "空间回收任务异常（详见 server.log）".to_string(),
            })
        }
    }
}
