'use strict';

/**
 * 参与者凭据登记与校验。
 *
 * **relay 的校验是"路由级"的，不是"授权级"的**：它只回答「这个 token 是不是
 * 本 relay 登记过的、并且绑到哪个 node」。它**不**判断这个 token 在 hermix
 * 侧是否有效、是否已撤销、是否具备某项能力——那件事由 hermix 在链路另一端
 * 独立完成（`DeviceRegistry::authenticate` + 请求签名）。
 *
 * 两端各自校验同一个凭据，是刻意的：relay 被攻破，攻击者能改路由，但拿不到
 * 会话内容也拿不到 hermix 侧身份；hermix 被攻破，relay 也无从得知。
 *
 * 明文 token **只在签发那一刻**存在一次；库里只存 sha256 哈希。
 */

const crypto = require('node:crypto');
const Database = require('better-sqlite3');

/** token 哈希：sha256 hex。自建 relay 规模下不需要加盐（无口令猜测面）。 */
function hashToken(token) {
  return crypto.createHash('sha256').update(token, 'utf8').digest('hex');
}

/** 签发一个新 token。只在这里出现明文。 */
function mintToken() {
  return `hrel_${crypto.randomBytes(24).toString('hex')}`;
}

/**
 * @param {string} dbPath SQLite 文件路径
 */
function openRegistry(dbPath) {
  const db = new Database(dbPath);
  db.pragma('journal_mode = WAL');
  db.exec(`
    CREATE TABLE IF NOT EXISTS participants (
      token_hash   TEXT PRIMARY KEY,
      node_id      TEXT NOT NULL,
      label        TEXT NOT NULL DEFAULT '',
      role         TEXT NOT NULL CHECK (role IN ('phone', 'node')),
      created_at   INTEGER NOT NULL,
      revoked_at    INTEGER
    );
    CREATE INDEX IF NOT EXISTS participants_node ON participants(node_id);
  `);

  const insert = db.prepare(
    `INSERT INTO participants (token_hash, node_id, label, role, created_at)
     VALUES (@token_hash, @node_id, @label, @role, @created_at)`,
  );
  const getByHash = db.prepare(
    `SELECT token_hash, node_id, label, role, created_at, revoked_at
       FROM participants WHERE token_hash = ?`,
  );
  const listByNode = db.prepare(
    `SELECT token_hash, node_id, label, role, created_at
       FROM participants WHERE node_id = ? AND revoked_at IS NULL`,
  );
  const revoke = db.prepare(
    `UPDATE participants SET revoked_at = ? WHERE token_hash = ? AND revoked_at IS NULL`,
  );

  return {
    db,
    close() {
      db.close();
    },

    /**
     * 登记一个参与者。返回明文 token（**仅此一次**）。
     * 同一个 node 可以有多条凭据（手机与电脑各一条）。
     */
    register({ nodeId, label = '', role }) {
      if (typeof nodeId !== 'string' || nodeId.trim() === '') {
        throw new Error('nodeId 不能为空');
      }
      if (role !== 'phone' && role !== 'node') {
        throw new Error(`role 必须是 phone 或 node，收到 ${role}`);
      }
      const token = mintToken();
      insert.run({
        token_hash: hashToken(token),
        node_id: nodeId.trim(),
        label: String(label),
        role,
        created_at: Date.now(),
      });
      return token;
    },

    /**
     * 校验并取出参与者身份。
     *
     * 撤销过的凭据**直接拒绝**（返回 null），不做"标记但放行"——被撤销的
     * 手机必须立刻断掉，否则撤销按钮就是个摆设。
     */
    authenticate(token) {
      if (typeof token !== 'string' || token === '') return null;
      const row = getByHash.get(hashToken(token));
      if (!row || row.revoked_at !== null) return null;
      return { nodeId: row.node_id, role: row.role, label: row.label };
    },

    /** 列出某 node 下所有未撤销凭据（`relayctl list` 用；只回哈希，不回明文）。 */
    list(nodeId) {
      return listByNode.all(nodeId).map((r) => ({
        token_hash: r.token_hash,
        label: r.label,
        role: r.role,
        created_at: r.created_at,
      }));
    },

    /** 撤销。返回是否真的撤掉了（重复撤销返回 false）。 */
    revokeToken(token) {
      if (typeof token !== 'string' || token === '') return false;
      return revoke.run(Date.now(), hashToken(token)).changes > 0;
    },
  };
}

/**
 * 从握手请求里取 Bearer token。
 *
 * **只认 `Authorization` 头**：历史上还有 `?token=` query 通道，它让凭据进
 * URL（服务器日志、Referer、历史记录）。hermix 侧已经因为同样理由把它删了
 * （`security.rs:537-547`），这里保持一致。
 */
function bearerFromHeaders(headers) {
  const raw = headers['authorization'];
  if (typeof raw !== 'string') return null;
  const match = /^Bearer\s+(.+)$/i.exec(raw.trim());
  if (!match) return null;
  const token = match[1].trim();
  return token === '' ? null : token;
}

module.exports = { openRegistry, hashToken, mintToken, bearerFromHeaders };
