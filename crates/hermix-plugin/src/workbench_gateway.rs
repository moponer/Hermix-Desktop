//! 工作台宿主网关 —— 代码层访问数据与本机的唯一通道。
//!
//! 代码层（iframe 内的 JS）在架构上够不到 `data/` 目录，也够不到本机磁盘：
//! 一切经 `window.wb.*` 打到这里。
//!
//! **没有授权这一层**：工作台是用户的个人环境，本机能力直接可用，
//! 不做目录授权、不弹窗、不需要用户预批。这里只保留两样东西：
//!
//! 1. **数据**：每工作台一个 SQLite 库，落在 `data/workbench.db`。
//!    代码可以随便改、回滚、重做，数据库不动 —— 「代码与数据生命周期分离」的地基。
//! 2. **边界**：密钥/凭据类文件不给碰。这不是「需要用户批准」，
//!    是**越权**（凭证属于用户与外部服务的信任关系，不属于工作台的数据面）；
//!    工作台之间的 `data/` 互相隔离，同理 —— 隔离是架构属性，不是权限申请。

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params_from_iter, Connection, OpenFlags};
use serde_json::Value as Json;

use crate::workbench::{workbench_dir, workbenches_dir};

/// 越权路径：密钥、凭据，以及**决定 agent 能做什么**的配置文件。
/// 判据不是「危不危险」，而是「这些根本不是工作台的数据」——
/// 摸到别人的凭据是越权，与用户授权与否无关。
///
/// 权限治理文件（审批白名单 / 会话放行 / 已启用工具集 / app home 下的
/// config.yaml）同样在这里拒：工作台代码是可被 `write_code_file` 写入的，
/// 若它能改写白名单，下一步 `rm -rf` 就不再经过人 —— 这条越权链与
/// `hermix_tools::permission_guard` 判的是同一组文件，名单在
/// `hermix_core::path_security` 共享，避免两侧漂移。
fn is_out_of_scope(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase().replace('\\', "/");
    const SENSITIVE: [&str; 6] = [
        ".env",
        ".ssh/",
        "id_rsa",
        "id_ed25519",
        "credentials",
        ".npmrc",
    ];
    if SENSITIVE.iter().any(|s| lower.contains(s)) {
        return true;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if hermix_core::path_security::is_permission_config_name(name) {
            return true;
        }
        // config.yaml 是成千上万个项目的普通文件，只在本 app home 下才是
        // 治理文件（provider 端点、审批开关、工具集）。
        if name.eq_ignore_ascii_case("config.yaml") {
            let home = hermix_config::paths::hermix_home();
            let home = home.to_string_lossy().to_lowercase().replace('\\', "/");
            if lower.starts_with(&format!("{}/", home.trim_end_matches('/'))) {
                return true;
            }
        }
    }
    // 工作台的 data/ 归宿主网关独占：代码层拿到路径直读会把
    // 「代码回滚不丢数据」的隔离打穿。
    let prefix = workbenches_dir()
        .to_string_lossy()
        .to_lowercase()
        .replace('\\', "/");
    lower.starts_with(&prefix) && lower.contains("/data/")
}

fn db_path(id: &str) -> PathBuf {
    workbench_dir(id).join("data").join("workbench.db")
}

// ── 数据层 ──────────────────────────────────────────────

fn connect(id: &str) -> Result<Connection, String> {
    let path = db_path(id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create data dir: {e}"))?;
    }
    Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| format!("open db: {e}"))
}

/// 多语句注入靠 rusqlite 的 prepare 天然拦掉（一次 prepare 只吃一条语句），
/// 这里再挡一层能跨库/改引擎行为的指令。
fn reject_dangerous(sql: &str) -> Result<(), String> {
    let upper = sql.to_uppercase();
    for kw in ["ATTACH", "DETACH", "PRAGMA", "VACUUM", "LOAD_EXTENSION"] {
        if upper.split_whitespace().any(|w| w == kw) {
            return Err(format!("禁止的 SQL：{kw}"));
        }
    }
    Ok(())
}

fn is_read_only(sql: &str) -> bool {
    let first = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    matches!(first.as_str(), "SELECT" | "WITH" | "VALUES" | "EXPLAIN")
}

/// 查询：只允许读语句，返回行数组。
pub fn db_query(id: &str, sql: &str, params: &[Json]) -> Result<Vec<Json>, String> {
    reject_dangerous(sql)?;
    if !is_read_only(sql) {
        return Err(format!("db.query 只接受读语句，收到：{sql}"));
    }
    let conn = connect(id)?;
    let mut stmt = conn.prepare(sql).map_err(|e| format!("SQL 错误：{e}"))?;
    let cols = stmt.column_count();
    let rows = stmt
        .query_map(params_from_iter(params.iter().map(json_to_sqlite)), |row| {
            let mut obj = serde_json::Map::new();
            for i in 0..cols {
                let name = row.as_ref().column_name(i).unwrap_or("").to_string();
                obj.insert(name, sqlite_to_json(row.get::<_, rusqlite::types::Value>(i)?));
            }
            Ok(Json::Object(obj))
        })
        .map_err(|e| format!("SQL 错误：{e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("读取行失败：{e}"))?);
    }
    Ok(out)
}

/// 写入：DDL / DML。返回受影响行数。
pub fn db_exec(id: &str, sql: &str, params: &[Json]) -> Result<usize, String> {
    reject_dangerous(sql)?;
    if is_read_only(sql) {
        return Err("db.exec 不接受读语句，请用 db.query".to_string());
    }
    let conn = connect(id)?;
    conn.execute(sql, params_from_iter(params.iter().map(json_to_sqlite)))
        .map_err(|e| format!("SQL 错误：{e}"))
}

fn json_to_sqlite(v: &Json) -> rusqlite::types::ToSqlOutput<'_> {
    use rusqlite::types::{ToSqlOutput, Value};
    match v {
        Json::Null => ToSqlOutput::Owned(Value::Null),
        Json::Bool(b) => ToSqlOutput::Owned(Value::Integer(*b as i64)),
        Json::Number(n) => match n.as_i64() {
            Some(i) => ToSqlOutput::Owned(Value::Integer(i)),
            None => ToSqlOutput::Owned(Value::Real(n.as_f64().unwrap_or(0.0))),
        },
        Json::String(s) => ToSqlOutput::Owned(Value::Text(s.clone())),
        other => ToSqlOutput::Owned(Value::Text(other.to_string())),
    }
}

fn sqlite_to_json(v: rusqlite::types::Value) -> Json {
    use rusqlite::types::Value;
    match v {
        Value::Null => Json::Null,
        Value::Integer(i) => Json::Number(i.into()),
        Value::Real(f) => serde_json::Number::from_f64(f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Text(s) => Json::String(s),
        Value::Blob(b) => Json::String(format!("<blob {} bytes>", b.len())),
    }
}

// ── 文件系统 ────────────────────────────────────────────

fn guard(path: &Path) -> Result<(), String> {
    if is_out_of_scope(path) {
        return Err(format!(
            "越权路径：{} —— 密钥/凭据与工作台数据层不在工作台的能力范围内",
            path.display()
        ));
    }
    Ok(())
}

pub fn fs_read(path: &str) -> Result<String, String> {
    let p = PathBuf::from(path);
    guard(&p)?;
    fs::read_to_string(&p).map_err(|e| format!("读取失败：{e}"))
}

pub fn fs_write(path: &str, content: &str) -> Result<(), String> {
    let p = PathBuf::from(path);
    guard(&p)?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    // 原子写：写到 .tmp 再 rename，避免读到半截内容。
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, content).map_err(|e| format!("写入失败：{e}"))?;
    fs::rename(&tmp, &p).map_err(|e| format!("落盘失败：{e}"))
}

pub fn fs_list(dir: &str) -> Result<Vec<String>, String> {
    let p = PathBuf::from(dir);
    guard(&p)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&p).map_err(|e| format!("列目录失败：{e}"))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        out.push(entry.path().to_string_lossy().to_string());
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_out_of_scope() {
        for p in [
            "C:/Users/x/.hermix/.env",
            "/home/x/.ssh/id_rsa",
            "D:/work/.env.local",
            "/home/x/.aws/credentials",
        ] {
            assert!(is_out_of_scope(Path::new(p)), "应拒绝 {p}");
        }
    }

    #[test]
    fn ordinary_paths_are_in_scope() {
        for p in [
            "D:/台账/2026.xlsx",
            "/home/x/documents/note.md",
            "C:/Users/x/Desktop/a.txt",
        ] {
            assert!(!is_out_of_scope(Path::new(p)), "不应拒绝 {p}");
        }
    }

    /// 权限治理文件：工作台代码改写了它们就等于给自己扩权
    /// （下一轮 `rm -rf` 不再经过人）。
    #[test]
    fn permission_governing_files_are_out_of_scope() {
        for p in [
            "C:/Users/x/.hermix/approval_allowlist.json",
            "C:/Users/x/.hermix/session_approvals.json",
            "C:/Users/x/proj/APPROVAL_ALLOWLIST.JSON",
            "D:/work/enabled_toolsets.json",
        ] {
            assert!(is_out_of_scope(Path::new(p)), "应拒绝 {p}");
        }
        // config.yaml 只在本 app home 下才是治理文件。
        let home = hermix_config::paths::hermix_home().join("config.yaml");
        assert!(is_out_of_scope(&home), "app home 下的 config.yaml 必须拒写");
        assert!(
            !is_out_of_scope(Path::new("D:/work/proj/config.yaml")),
            "项目自己的 config.yaml 是普通文件"
        );
    }

    #[test]
    fn sql_classification() {
        assert!(is_read_only("SELECT * FROM t"));
        assert!(is_read_only("with x as (select 1) select * from x"));
        assert!(!is_read_only("INSERT INTO t VALUES (1)"));
        assert!(reject_dangerous("ATTACH DATABASE 'x' AS y").is_err());
        assert!(reject_dangerous("PRAGMA table_info(t)").is_err());
        assert!(reject_dangerous("SELECT 1").is_ok());
    }
}
