//! 一次性迁移：litleblog 旧站（ByetHost PHP）帖子 → hermix-bbs SQLite。
//!
//! 决策笔记 2026-10-09-agent-bbs-integration「数据迁移」条目：量小、跑一次。
//!
//! 用法（在 apps/bbs 目录下）：
//!   node scripts/migrate-from-legacy.js <export.json> [--db bbs.db]
//!
//! export.json 格式（两种来源任选其一，字段映射在 normalize 兼容）：
//!   1. 旧站管理后台导出的 JSON：[{ title, content, author, created_at, replies: [...] }]
//!   2. 手工整理的同构文件。
//!
//! 注意：ByetHost 对非浏览器流量有 JS 挑战（否决 A1 的根因），所以本脚本
//! **不做在线拉取**——先在浏览器里把旧站数据导出/复制成 JSON，再喂给本脚本。
//! agent 名映射：旧作者名直接注册为 BBS agent（key 随机生成、弃用不展示，
//! 旧作者本就不会再发帖；key_hash 入库仅为满足外键）。

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const { openDb } = require('../src/db.js');
const { mdToSafeHtml } = require('../src/sanitize.js');

function arg(names, def) {
  const i = process.argv.indexOf(names);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : def;
}

const srcFile = process.argv[2];
const dbPath = arg('--db', 'bbs.db');
if (!srcFile) {
  console.error('用法: node scripts/migrate-from-legacy.js <export.json> [--db bbs.db]');
  process.exit(1);
}

const items = JSON.parse(fs.readFileSync(srcFile, 'utf8'));
if (!Array.isArray(items) || items.length === 0) {
  console.error('export.json 必须是非空数组');
  process.exit(1);
}

const db = openDb(dbPath);

/** 旧作者 → bbs_agents。key 只生成不入日志（没有人会再用它）。 */
const agentIds = new Map();
function agentIdFor(name) {
  if (agentIds.has(name)) return agentIds.get(name);
  const keyHash = crypto.createHash('sha256').update(crypto.randomBytes(32)).digest('hex');
  const info = db
    .prepare('INSERT INTO bbs_agents (agent_name, key_hash, created_at) VALUES (?, ?, ?)')
    .run(name, keyHash, new Date().toISOString());
  const id = Number(info.lastInsertRowid);
  agentIds.set(name, id);
  return id;
}

let posts = 0;
let replies = 0;
// 幂等保护：同文件重跑不重复灌（以旧站标题+时间为指纹）。
const existed = new Set(
  db.prepare('SELECT title, created_at FROM bbs_posts').all().map((r) => `${r.title}|${r.created_at}`),
);

for (const it of items) {
  const title = String(it.title ?? '').trim();
  const contentMd = String(it.content ?? it.content_md ?? '').trim();
  const author = String(it.author ?? it.agent_name ?? 'legacy-import').trim() || 'legacy-import';
  const createdAt = String(it.created_at ?? new Date().toISOString());
  if (!title || !contentMd) {
    console.warn(`跳过（缺标题或正文）: ${JSON.stringify(it).slice(0, 80)}…`);
    continue;
  }
  const fp = `${title}|${createdAt}`;
  if (existed.has(fp)) {
    console.warn(`跳过（已存在）: ${title}`);
    continue;
  }
  const info = db
    .prepare(
      'INSERT INTO bbs_posts (agent_id, title, content_md, content_html, created_at) VALUES (?, ?, ?, ?, ?)',
    )
    .run(agentIdFor(author), title, contentMd, mdToSafeHtml(contentMd), createdAt);
  const postId = Number(info.lastInsertRowid);
  posts++;
  for (const rep of it.replies ?? []) {
    const repMd = String(rep.content ?? rep.content_md ?? '').trim();
    if (!repMd) continue;
    const repAuthor = String(rep.author ?? rep.agent_name ?? author).trim() || author;
    const repAt = String(rep.created_at ?? createdAt);
    db.prepare(
      'INSERT INTO bbs_replies (post_id, agent_id, content_md, content_html, created_at) VALUES (?, ?, ?, ?, ?)',
    ).run(postId, agentIdFor(repAuthor), repMd, mdToSafeHtml(repMd), repAt);
    replies++;
  }
}

console.log(`迁移完成: ${posts} 帖 / ${replies} 回复 → ${path.resolve(dbPath)}`);
console.log(`agents 注册（key 弃用）: ${agentIds.size}`);
