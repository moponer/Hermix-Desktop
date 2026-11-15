//! `token_usage` 保留期（`storage.metrics_retention_days`，默认 **0 = 关闭**）。
//!
//! 为什么只清这张表：一行 = 一次 API 调用，纯指标、无回看价值，且是
//! sessions.db 里增长最快的数据。聊天内容（messages / compression_archives）
//! **永远不在这里删** —— 那类保留期是产品决策，必须先有"删了什么"的用户
//! 沟通，不能藏在一个配置项后面。
//!
//! 节奏：启动后 60s 跑第一次，之后每 6 小时一次。每次删完顺手
//! `wal_checkpoint(PASSIVE)`（截断留给人工 `/api/storage/vacuum`）。

use std::path::{Path, PathBuf};

/// 启动后台循环。调用方保证 `retention_days > 0`。
pub async fn run_loop(home: PathBuf, retention_days: u32) {
    let interval = std::time::Duration::from_secs(6 * 60 * 60);
    // 启动后延迟 60s：避开启动高峰（agent 预热、首个回合），也避免与
    // 首次 schema 迁移抢锁。
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    loop {
        let home = home.clone();
        let deleted = tokio::task::spawn_blocking(move || prune_once(&home, retention_days))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "storage janitor task failed");
                0
            });
        if deleted > 0 {
            tracing::info!(deleted, retention_days, "storage: token_usage pruned");
        }
        tokio::time::sleep(interval).await;
    }
}

/// 删除早于保留期的 `token_usage` 行，返回删除行数。任何失败都只记日志：
/// 清理失败不该影响服务运行。
pub(crate) fn prune_once(home: &Path, retention_days: u32) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let db = hermix_config::paths::data_dir_in(home).join("sessions.db");
    if !db.exists() {
        return 0;
    }
    let conn = match hermix_store::open_connection(&db, hermix_store::OpenMode::Write) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "storage janitor: cannot open sessions.db");
            return 0;
        }
    };
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64))
        .to_rfc3339();
    let deleted = match conn.execute(
        "DELETE FROM token_usage WHERE created_at < ?1",
        rusqlite::params![cutoff],
    ) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "storage janitor: prune failed");
            return 0;
        }
    };
    if deleted > 0 {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空库 / 不存在的库 → 0 行删除，不报错。
    #[test]
    fn missing_db_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(prune_once(tmp.path(), 30), 0);
    }

    #[test]
    fn prunes_only_rows_older_than_cutoff() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = hermix_config::paths::data_dir_in(tmp.path());
        std::fs::create_dir_all(&data_dir).unwrap();
        let db = data_dir.join("sessions.db");
        {
            let conn = hermix_store::open_connection(&db, hermix_store::OpenMode::Write).unwrap();
            conn.execute_batch(
                "CREATE TABLE token_usage (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL
                );",
            )
            .unwrap();
            // 老行（retention 30 天前之外）与新生（刚刚）各一。
            conn.execute(
                "INSERT INTO token_usage (created_at) VALUES ('2020-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO token_usage (created_at) VALUES (?1)",
                rusqlite::params![chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        assert_eq!(prune_once(tmp.path(), 30), 1, "只删老行");
        let conn = hermix_store::open_connection(&db, hermix_store::OpenMode::Read).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "新行必须保留");
    }

    #[test]
    fn zero_retention_never_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = hermix_config::paths::data_dir_in(tmp.path());
        std::fs::create_dir_all(&data_dir).unwrap();
        let db = data_dir.join("sessions.db");
        {
            let conn = hermix_store::open_connection(&db, hermix_store::OpenMode::Write).unwrap();
            conn.execute_batch(
                "CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO token_usage (created_at) VALUES ('2020-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        }
        assert_eq!(prune_once(tmp.path(), 0), 0, "0 = 永不清理");
    }
}
