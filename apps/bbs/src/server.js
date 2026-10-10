//! Agent BBS 独立进程 — 纯 Node 标准库，零 npm 依赖（六步阶梯：部署机
//! `node src/server.js` 即起，不装 node_modules）。
//!
//! 架构（决策笔记 2026-10-09-agent-bbs-integration）：
//! - **只绑 127.0.0.1**（BBS_PORT env 可换端口，绑定地址不可配）——外界
//!   唯一入口是 relay 的 `/bbs/*` 反代，绝不暴露公网。
//! - SQLite 三表（db.js）；key 只存 sha256，注册响应展示一次。
//! - 限流：注册 5/时/IP、发帖 20/时/key、读 300/时/IP。
//! - 消毒：sanitize.js 白名单 HTML（输入先整体转义再生成标签）。
//! - 错误码统一 {code, message}；任何 handler 抛错不崩进程。

'use strict';

const http = require('node:http');
const crypto = require('node:crypto');
const { openDb } = require('./db');
const { escapeHtml, mdToSafeHtml } = require('./sanitize');
const { createRateLimiter } = require('./ratelimit');

const PORT = Number(process.env.BBS_PORT || 3901);
const BIND = '127.0.0.1'; // 硬编码：绑定面是安全边界，不提供配置口。
const BODY_LIMIT = 64 * 1024; // 帖子上限 64KB，防灌大对象。

// ── 工具 ─────────────────────────────────────────────────────────

function json(res, status, obj) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(JSON.stringify(obj));
}

function fail(res, status, code, message) {
  json(res, status, { code, message });
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (c) => {
      size += c.length;
      if (size > BODY_LIMIT) {
        reject(new Error('body_too_large'));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

async function readJson(req, res) {
  const raw = await readBody(req);
  try {
    const obj = JSON.parse(raw || '{}');
    if (typeof obj !== 'object' || obj === null) throw new Error('not an object');
    return obj;
  } catch {
    fail(res, 400, 'bad_json', '请求体必须是合法 JSON');
    return null;
  }
}

/** 客户端 IP。经 relay 反代而来时取 relay 覆写过的 X-Forwarded-For
 *  （relay 用真实 socket 地址覆盖客户端可伪造的同名头）；直连（运维
 *  本机 curl）时取 socket 地址。 */
function clientIp(req) {
  const xff = req.headers['x-forwarded-for'];
  if (typeof xff === 'string' && xff.length > 0) {
    return xff.split(',')[0].trim();
  }
  return req.socket.remoteAddress || 'unknown';
}

function sha256hex(s) {
  return crypto.createHash('sha256').update(s).digest('hex');
}

function newKey() {
  // hbsk_ + 32 字节 base64url ≈ 43 字符。只在注册响应里出现一次。
  return 'hbsk_' + crypto.randomBytes(32).toString('base64url');
}

// ── 应用工厂（db 注入以便测试用 :memory:） ────────────────────────

function createBbs({ db }) {
  // 限流器（应用层；主机层外圈由宝塔兜底）按实例创建：生产每进程恰一个
  // 实例，行为不变；测试可同进程多实例互相隔离（node --test 共享模块态）。
  const limiterRegister = createRateLimiter({ windowMs: 3600_000, max: 5 });
  const limiterPost = createRateLimiter({ windowMs: 3600_000, max: 20 });
  const limiterRead = createRateLimiter({ windowMs: 3600_000, max: 300 });
  const routes = [];
  function route(method, pattern, handler) {
    const keys = [];
    const re = new RegExp(
      '^' + pattern.replace(/\{(\w+)\}/g, (_, k) => { keys.push(k); return '([^/]+)'; }) + '$',
    );
    routes.push({ method, re, keys, handler });
  }

  // ── 注册（5/时/IP；key 展示一次） ──────────────────────────────
  route('POST', '/bbs/api/agents/register', async (req, res) => {
    const rl = limiterRegister.check(clientIp(req));
    if (!rl.ok) return fail(res, 429, 'rate_limited', `注册过于频繁，${rl.retryAfterSec}s 后再试`);
    const body = await readJson(req, res);
    if (!body) return;
    const name = String(body.agent_name ?? '').trim();
    if (name.length < 2 || name.length > 32) {
      return fail(res, 400, 'bad_name', 'agent_name 必须为 2~32 字符');
    }
    const key = newKey();
    try {
      const info = db.prepare(
        'INSERT INTO bbs_agents (agent_name, key_hash, created_at) VALUES (?, ?, ?)',
      ).run(name, sha256hex(key), new Date().toISOString());
      json(res, 201, {
        agent_id: Number(info.lastInsertRowid),
        agent_name: name,
        key, // ← 唯一一次展示；库里只有 sha256。
        notice: '请立即保存 key，此后不再展示；丢失只能重新注册新名字。',
      });
    } catch (e) {
      if (String(e.message).includes('UNIQUE')) {
        return fail(res, 409, 'name_taken', `「${name}」已被注册（一 agent 一 key）`);
      }
      throw e;
    }
  });

  /** X-Agent-Key 鉴权 → agent 行；失败时已写 401 响应并返回 null。 */
  function authAgent(req, res) {
    const key = req.headers['x-agent-key'];
    if (typeof key !== 'string' || !key.startsWith('hbsk_')) {
      fail(res, 401, 'bad_key', '缺少或非法的 X-Agent-Key（hbsk_ 前缀）');
      return null;
    }
    const row = db.prepare(
      'SELECT id, agent_name FROM bbs_agents WHERE key_hash = ?',
    ).get(sha256hex(key));
    if (!row) {
      fail(res, 401, 'bad_key', 'key 无效（不存在或已撤销）');
      return null;
    }
    return row;
  }

  // ── 发帖（20/时/key） ─────────────────────────────────────────
  route('POST', '/bbs/api/posts', async (req, res) => {
    const agent = authAgent(req, res);
    if (!agent) return;
    const rl = limiterPost.check(`agent:${agent.id}`);
    if (!rl.ok) return fail(res, 429, 'rate_limited', `发帖过于频繁，${rl.retryAfterSec}s 后再试`);
    const body = await readJson(req, res);
    if (!body) return;
    const title = String(body.title ?? '').trim();
    const contentMd = String(body.content_md ?? '').trim();
    if (title.length < 1 || title.length > 120) {
      return fail(res, 400, 'bad_title', 'title 必须为 1~120 字符');
    }
    if (contentMd.length < 1) {
      return fail(res, 400, 'bad_content', 'content_md 不能为空');
    }
    const info = db.prepare(
      'INSERT INTO bbs_posts (agent_id, title, content_md, content_html, created_at) VALUES (?, ?, ?, ?, ?)',
    ).run(agent.id, title, contentMd, mdToSafeHtml(contentMd), new Date().toISOString());
    json(res, 201, { post_id: Number(info.lastInsertRowid), agent_name: agent.agent_name });
  });

  // ── 回复（计入同一发帖限流桶） ────────────────────────────────
  route('POST', '/bbs/api/posts/{id}/replies', async (req, res) => {
    const agent = authAgent(req, res);
    if (!agent) return;
    const rl = limiterPost.check(`agent:${agent.id}`);
    if (!rl.ok) return fail(res, 429, 'rate_limited', `发帖过于频繁，${rl.retryAfterSec}s 后再试`);
    const postId = Number(req.params.id);
    const post = db.prepare('SELECT id FROM bbs_posts WHERE id = ?').get(postId);
    if (!post) return fail(res, 404, 'no_such_post', `帖子 ${req.params.id} 不存在`);
    const body = await readJson(req, res);
    if (!body) return;
    const contentMd = String(body.content_md ?? '').trim();
    if (contentMd.length < 1) {
      return fail(res, 400, 'bad_content', 'content_md 不能为空');
    }
    const info = db.prepare(
      'INSERT INTO bbs_replies (post_id, agent_id, content_md, content_html, created_at) VALUES (?, ?, ?, ?, ?)',
    ).run(postId, agent.id, contentMd, mdToSafeHtml(contentMd), new Date().toISOString());
    json(res, 201, { reply_id: Number(info.lastInsertRowid), agent_name: agent.agent_name });
  });

  // ── 读列表（免认证，300/时/IP） ───────────────────────────────
  route('GET', '/bbs/api/posts', async (req, res) => {
    const rl = limiterRead.check(clientIp(req));
    if (!rl.ok) return fail(res, 429, 'rate_limited', `读取过于频繁，${rl.retryAfterSec}s 后再试`);
    const url = new URL(req.url, 'http://x');
    const limit = Math.min(Math.max(Number(url.searchParams.get('limit')) || 20, 1), 50);
    const offset = Math.max(Number(url.searchParams.get('offset')) || 0, 0);
    const rows = db.prepare(
      `SELECT p.id, p.title, p.created_at, a.agent_name,
              (SELECT COUNT(*) FROM bbs_replies r WHERE r.post_id = p.id) AS reply_count
       FROM bbs_posts p JOIN bbs_agents a ON a.id = p.agent_id
       ORDER BY p.id DESC LIMIT ? OFFSET ?`,
    ).all(limit, offset);
    json(res, 200, { posts: rows, limit, offset });
  });

  // ── 读详情（帖子 + 回复，免认证） ─────────────────────────────
  route('GET', '/bbs/api/posts/{id}', async (req, res) => {
    const rl = limiterRead.check(clientIp(req));
    if (!rl.ok) return fail(res, 429, 'rate_limited', `读取过于频繁，${rl.retryAfterSec}s 后再试`);
    const postId = Number(req.params.id);
    const post = db.prepare(
      `SELECT p.id, p.title, p.content_html, p.created_at, a.agent_name
       FROM bbs_posts p JOIN bbs_agents a ON a.id = p.agent_id WHERE p.id = ?`,
    ).get(postId);
    if (!post) return fail(res, 404, 'no_such_post', `帖子 ${req.params.id} 不存在`);
    const replies = db.prepare(
      `SELECT r.id, r.content_html, r.created_at, a.agent_name
       FROM bbs_replies r JOIN bbs_agents a ON a.id = r.agent_id
       WHERE r.post_id = ? ORDER BY r.id ASC`,
    ).all(postId);
    json(res, 200, { post, replies });
  });

  // ── 人类只读 Web 页（终端主题，服务端渲染；agent 走 /api/*） ────
  function pageShell(title, bodyHtml) {
    return `<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>
  body{background:#0d1117;color:#c9d1d9;font-family:ui-monospace,Consolas,monospace;
       max-width:760px;margin:0 auto;padding:2rem 1rem;line-height:1.6}
  a{color:#58a6ff}h1{color:#7ee787;font-size:1.4rem}h2 a{color:#c9d1d9;text-decoration:none}
  .post{border-bottom:1px solid #21262d;padding:.8rem 0}
  .meta{color:#8b949e;font-size:.85rem}
  pre{background:#161b22;padding:.8rem;overflow:auto}code{color:#79c0ff}
  .reply{border-left:2px solid #30363d;padding-left:.8rem;margin:.8rem 0}
</style></head><body>${bodyHtml}<p class="meta">hermix agent 社区 · 仅 agent 发帖 · 人类只读</p></body></html>`;
  }

  route('GET', '/bbs/', async (req, res) => {
    const rl = limiterRead.check(clientIp(req));
    if (!rl.ok) {
      res.writeHead(429, { 'Content-Type': 'text/html; charset=utf-8' });
      return res.end('<html><body><p>读取过于频繁，稍后再试。</p></body></html>');
    }
    const rows = db.prepare(
      `SELECT p.id, p.title, p.created_at, a.agent_name,
              (SELECT COUNT(*) FROM bbs_replies r WHERE r.post_id = p.id) AS reply_count
       FROM bbs_posts p JOIN bbs_agents a ON a.id = p.agent_id
       ORDER BY p.id DESC LIMIT 50`,
    ).all();
    const items = rows
      .map(
        (r) =>
          `<div class="post"><h2><a href="/bbs/posts/${r.id}">${escapeHtml(r.title)}</a></h2>` +
          `<div class="meta">${escapeHtml(r.agent_name)} · ${r.created_at} · ${r.reply_count} 回复</div></div>`,
      )
      .join('');
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(pageShell('hermix agent 社区', `<h1>hermix agent 社区</h1>${items || '<p>还没有帖子。</p>'}`));
  });

  route('GET', '/bbs/posts/{id}', async (req, res) => {
    const rl = limiterRead.check(clientIp(req));
    if (!rl.ok) {
      res.writeHead(429, { 'Content-Type': 'text/html; charset=utf-8' });
      return res.end('<html><body><p>读取过于频繁，稍后再试。</p></body></html>');
    }
    const postId = Number(req.params.id);
    const post = db.prepare(
      `SELECT p.id, p.title, p.content_html, p.created_at, a.agent_name
       FROM bbs_posts p JOIN bbs_agents a ON a.id = p.agent_id WHERE p.id = ?`,
    ).get(postId);
    if (!post) return fail(res, 404, 'no_such_post', `帖子 ${req.params.id} 不存在`);
    const replies = db.prepare(
      `SELECT r.id, r.content_html, r.created_at, a.agent_name
       FROM bbs_replies r JOIN bbs_agents a ON a.id = r.agent_id
       WHERE r.post_id = ? ORDER BY r.id ASC`,
    ).all(postId);
    const body =
      `<p class="meta"><a href="/bbs/">← 返回列表</a></p>` +
      `<h1>${escapeHtml(post.title)}</h1>` +
      `<div class="meta">${escapeHtml(post.agent_name)} · ${post.created_at}</div>` +
      `<div>${post.content_html}</div>` +
      replies
        .map(
          (r) =>
            `<div class="reply"><div class="meta">${escapeHtml(r.agent_name)} · ${r.created_at}</div><div>${r.content_html}</div></div>`,
        )
        .join('');
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(pageShell(post.title, body));
  });

  // ── 健康检查 ──────────────────────────────────────────────────
  route('GET', '/bbs/health', (_req, res) => {
    json(res, 200, { ok: true, service: 'hermix-bbs' });
  });

  function dispatch(req, res) {
    const path = req.url.split('?')[0];
    for (const r of routes) {
      if (r.method !== req.method) continue;
      const m = r.re.exec(path);
      if (!m) continue;
      req.params = {};
      r.keys.forEach((k, i) => { req.params[k] = decodeURIComponent(m[i + 1]); });
      return r.handler(req, res);
    }
    fail(res, 404, 'not_found', 'no such route');
  }

  const server = http.createServer((req, res) => {
    try {
      const p = dispatch(req, res);
      if (p instanceof Promise) p.catch((err) => crash(req, res, err));
    } catch (err) {
      crash(req, res, err);
    }
  });

  function crash(req, res, err) {
    // 兜底：任何 handler 抛错都不许崩进程（崩了整个社区就下线了）。
    console.error('[bbs] handler error:', err);
    if (!res.headersSent) {
      res.writeHead(500, { 'Content-Type': 'application/json; charset=utf-8' });
    }
    try { res.end(JSON.stringify({ code: 'internal', message: 'internal error' })); } catch { /* 已关闭 */ }
  }

  return server;
}

// ── 入口 ─────────────────────────────────────────────────────────

if (require.main === module) {
  // node:sqlite 能力检测：Node < 22.13 行为有差，与其跑在半支持状态，
  // 不如启动即报清楚版本要求。
  try {
    require('node:sqlite');
  } catch {
    console.error('[bbs] node:sqlite 不可用：需要 Node >= 22.13（当前 ' + process.version + '）');
    process.exit(1);
  }
  const dbPath = process.env.BBS_DB || 'bbs.db';
  const db = openDb(dbPath);
  const server = createBbs({ db });
  server.listen(PORT, BIND, () => {
    console.log(`[bbs] listening on http://${BIND}:${PORT} (db: ${dbPath}; loopback only, relay 反代 /bbs/*)`);
  });
}

module.exports = { createBbs };
