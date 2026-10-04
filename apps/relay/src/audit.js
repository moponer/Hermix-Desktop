'use strict';

/**
 * 审计：每条连接与每次路由都落一行。
 *
 * 这是**旁路观测**，不是安全边界——审计写失败绝不阻断转发（与 hermix 侧
 * `TaskRecordSink` 的 best-effort 契约一致）。审计里**不记 payload，也不记
 * token 明文**，只记哈希前缀：出事后要能回答"谁在什么时候连过"，但审计表
 * 本身不该变成第二个凭据库。
 */

const Database = require('better-sqlite3');

function openAudit(dbPath, { retainDays = 30 } = {}) {
  const db = new Database(dbPath);
  db.pragma('journal_mode = WAL');
  db.exec(`
    CREATE TABLE IF NOT EXISTS audit (
      id         INTEGER PRIMARY KEY AUTOINCREMENT,
      ts         INTEGER NOT NULL,
      event      TEXT NOT NULL,
      node_id    TEXT,
      role       TEXT,
      detail     TEXT
    );
    CREATE INDEX IF NOT EXISTS audit_ts ON audit(ts);
  `);

  const insert = db.prepare(
    `INSERT INTO audit (ts, event, node_id, role, detail) VALUES (?, ?, ?, ?, ?)`,
  );
  const recent = db.prepare(
    `SELECT ts, event, node_id, role, detail FROM audit ORDER BY id DESC LIMIT ?`,
  );
  const prune = db.prepare(`DELETE FROM audit WHERE ts < ?`);

  let failures = 0;

  return {
    db,
    close() {
      db.close();
    },
    /** 记一行。失败只计数不抛——审计挂了不能影响转发。 */
    write(event, nodeId = null, role = null, detail = null) {
      try {
        insert.run(Date.now(), event, nodeId, role, detail);
      } catch {
        failures += 1;
      }
    },
    recent(limit = 100) {
      return recent.all(limit);
    },
    /** 清掉过期行。retention 是隐私承诺的一部分：自建 relay 长期跑会攒不少。 */
    prune() {
      const cutoff = Date.now() - retainDays * 24 * 60 * 60 * 1000;
      return prune.run(cutoff).changes;
    },
    /** 审计写失败次数（供 /health 暴露；持续增长说明磁盘或表有问题）。 */
    failureCount() {
      return failures;
    },
  };
}

module.exports = { openAudit };
