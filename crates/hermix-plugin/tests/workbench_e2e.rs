//! 工作台端到端：真建一个工作台，跑通「建库 → 读写 → 硬拒 → 撤销」。
//!
//! 单独一个测试二进制，因为要改 `HERMIX_HOME` 指向临时目录 —— env 是进程级的，
//! 混进 lib 单测会污染其它用例。

use hermix_plugin::workbench::workbench_dir;
use hermix_plugin::{
    create_workbench, db_exec, db_query, delete_workbench, fs_read, fs_write, list_code_files,
    list_workbenches, read_code_file, undo_history, undo_last, write_code_file,
};
use serde_json::json;

struct Sandbox {
    root: std::path::PathBuf,
    id: String,
}

fn sandbox(tag: &str) -> Sandbox {
    let root = std::env::temp_dir().join(format!("wb-e2e-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("HERMIX_HOME", &root);
    let m = create_workbench("端到端").expect("create");
    Sandbox { root, id: m.id }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 全部场景跑在一个用例里：`HERMIX_HOME` 是进程级 env，拆成多个 `#[test]`
/// 会并行互相覆盖，彼此把对方的沙箱目录删掉。
#[test]
fn workbench_end_to_end() {
    // ── 数据层 ──
    {
        let s = sandbox("db");
        db_exec(&s.id, "CREATE TABLE items (title TEXT, n INTEGER)", &[]).expect("create table");
        db_exec(
            &s.id,
            "INSERT INTO items VALUES (?, ?)",
            &[json!("台账"), json!(42)],
        )
        .expect("insert");
        let rows = db_query(&s.id, "SELECT title, n FROM items", &[]).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], json!("台账"));
        assert_eq!(rows[0]["n"], json!(42));

        // 危险 SQL 必须拒：ATTACH 能把网关变成任意文件读取器。
        assert!(db_query(&s.id, "ATTACH DATABASE 'x.db' AS y", &[]).is_err());
        assert!(db_exec(&s.id, "PRAGMA user_version = 1", &[]).is_err());
        // 读语句走错门也拒，避免 exec 被当成万能口子。
        assert!(db_exec(&s.id, "SELECT 1", &[]).is_err());
        assert!(db_query(&s.id, "DROP TABLE items", &[]).is_err());
    }

    // ── 本机文件 ──
    {
        let s = sandbox("fs");
        let file = s.root.join("note.txt");
        fs_write(&file.to_string_lossy(), "内容").expect("write");
        assert_eq!(fs_read(&file.to_string_lossy()).expect("read"), "内容");

        // 密钥类硬拒，与目录授权无关。
        let env_file = s.root.join(".env");
        std::fs::write(&env_file, "KEY=1").unwrap();
        assert!(fs_read(&env_file.to_string_lossy()).is_err());
    }

    // ── 撤销栈 ──
    // 代码改动可撤销，且撤销后数据层不动 —— 「代码与数据生命周期分离」的验收点。
    {
        let s = sandbox("undo");
        db_exec(&s.id, "CREATE TABLE t (v TEXT)", &[]).unwrap();
        write_code_file(&s.id, "src/views/overview.js", "export const a = 1;\n").expect("write 1");
        write_code_file(&s.id, "src/views/overview.js", "export const a = 2;\n").expect("write 2");
        assert!(read_code_file(&s.id, "src/views/overview.js")
            .unwrap()
            .contains("= 2"));

        assert_eq!(
            undo_last(&s.id).expect("undo").as_deref(),
            Some("src/views/overview.js")
        );
        assert!(read_code_file(&s.id, "src/views/overview.js")
            .unwrap()
            .contains("= 1"));

        // 撤销代码不该碰数据库。
        db_exec(&s.id, "INSERT INTO t VALUES ('x')", &[]).unwrap();
        assert_eq!(db_query(&s.id, "SELECT * FROM t", &[]).unwrap().len(), 1);
        assert_eq!(undo_history(&s.id).len(), 1);
    }

    // ── 骨架 ──
    // 骨架文件必须齐全，否则「只在格子里填」无从谈起。
    {
        let s = sandbox("skel");
        let files = list_code_files(&s.id).expect("files");
        for expected in [
            "index.html",
            "styles/tokens.css",
            "src/main.js",
            "src/app.js",
            "src/store.js",
            "src/api.js",
            "src/views/overview.js",
            "src/components/card.js",
            "src/utils/format.js",
        ] {
            assert!(files.iter().any(|f| f == expected), "缺 {expected}: {files:?}");
        }
    }

    // ── 软删 ──
    // 目录移入 _trash 可找回，list 不再返回；重复删/删不存在的要报错。
    {
        let s = sandbox("delete");
        let trash_dest = delete_workbench(&s.id).expect("delete");
        assert!(trash_dest.exists(), "recycled dir must exist: {trash_dest:?}");
        assert!(!workbench_dir(&s.id).exists(), "original dir must be gone");
        assert!(
            !list_workbenches().expect("list").iter().any(|m| m.id == s.id),
            "deleted workbench must not appear in list"
        );
        // 二次删除：目录已不在，报错而不是静默成功。
        assert!(delete_workbench(&s.id).is_err());
        // 路径穿越 id 必须拒 —— id 直接拼路径。
        assert!(delete_workbench("../escape").is_err());
        assert!(delete_workbench("").is_err());
    }
}
