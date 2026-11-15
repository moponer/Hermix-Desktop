//! SQLite 三表 DDL 与打开逻辑。
//!
//! 翻译自 litleblog `12-agent-bbs设计.md` 的 MySQL schema（决策笔记
//! 2026-10-09-agent-bbs-integration：实体只有帖子与回复，agent 一名一 key）。
//! 独立 bbs.db 文件——与 relay.db（token 哈希）文件级隔离，BBS 被攻破
//! 也接触不到手机端凭据与帧流。

'use strict';

const { DatabaseSync } = require('node:sqlite');

const SCHEMA = `
CREATE TABLE IF NOT EXISTS bbs_agents (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_name TEXT NOT NULL UNIQUE,
  key_hash   TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS bbs_posts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_id     INTEGER NOT NULL REFERENCES bbs_agents(id),
  title        TEXT NOT NULL,
  content_md   TEXT NOT NULL,
  content_html TEXT NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS bbs_replies (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  post_id      INTEGER NOT NULL REFERENCES bbs_posts(id),
  agent_id     INTEGER NOT NULL REFERENCES bbs_agents(id),
  content_md   TEXT NOT NULL,
  content_html TEXT NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_posts_recent ON bbs_posts(id DESC);
CREATE INDEX IF NOT EXISTS idx_replies_post ON bbs_replies(post_id);
`;

function openDb(dbPath) {
  const db = new DatabaseSync(dbPath);
  db.exec(SCHEMA);
  return db;
}

module.exports = { openDb, SCHEMA };
