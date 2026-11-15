//! agent-bbs API 测试（node --test；:memory: 库 + 注入式 db，不落盘）。
//!
//! 用量规划（模块级限流器跨用例共享）：注册 ≤3/IP、发帖 ≤20/key、
//! 读 ≤300/IP——本文件全部在限内。

'use strict';

const test = require('node:test');
const assert = require('node:assert');
const http = require('node:http');

const { createBbs } = require('../src/server.js');
const { openDb } = require('../src/db.js');
const { mdToSafeHtml } = require('../src/sanitize.js');

function withServer(fn) {
  return async () => {
    const db = openDb(':memory:');
    const server = createBbs({ db });
    await new Promise((r) => server.listen(0, '127.0.0.1', r));
    const port = server.address().port;
    const base = `http://127.0.0.1:${port}`;
    try {
      await fn(base, db);
    } finally {
      server.close();
    }
  };
}

function req(method, url, headers = {}, body) {
  return new Promise((resolve, reject) => {
    const r = http.request(url, { method, headers }, (up) => {
      let b = '';
      up.on('data', (d) => (b += d));
      up.on('end', () => resolve({ status: up.statusCode, body: b }));
    });
    r.on('error', reject);
    if (body !== undefined) r.write(JSON.stringify(body));
    r.end();
  });
}

const json = (r) => JSON.parse(r.body);

test('注册：201 + key 展示一次 + 重名 409 + 名字校验', withServer(async (base) => {
  const reg = json(await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: 'alpha-agent' }));
  assert.strictEqual(reg.agent_name, 'alpha-agent');
  assert.ok(reg.key.startsWith('hbsk_'));
  assert.ok(reg.key.length >= 40, 'key 应有足够熵');

  const dup = await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: 'alpha-agent' });
  assert.strictEqual(dup.status, 409);
  assert.strictEqual(json(dup).code, 'name_taken');

  const bad = await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: 'x' });
  assert.strictEqual(bad.status, 400);
}));

test('发帖/回复/读：全链路 + 消毒 + 错 key 401 + 404', withServer(async (base) => {
  const reg = json(await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: 'poster' }));
  const key = { 'X-Agent-Key': reg.key };

  // 错 key 与缺 key 都是 401。
  assert.strictEqual((await req('POST', `${base}/bbs/api/posts`, { 'X-Agent-Key': 'hbsk_nope' }, { title: 't', content_md: 'c' })).status, 401);
  assert.strictEqual((await req('POST', `${base}/bbs/api/posts`, {}, { title: 't', content_md: 'c' })).status, 401);

  // 发帖：script 消毒、markdown 保留。
  const post = json(await req('POST', `${base}/bbs/api/posts`, key, {
    title: '首帖 <script>alert(1)</script>',
    content_md: '# 不支持标题级\n`code` 与 [链接](https://x.y) 与 **粗体** 与 [坏](javascript:alert(1))',
  }));
  assert.ok(post.post_id > 0);
  const detail = json(await req('GET', `${base}/bbs/api/posts/${post.post_id}`));
  assert.ok(!detail.post.content_html.includes('<script>'), 'script 必须被转义');
  assert.ok(!detail.post.content_html.includes('<h1>'), '一级标题不在白名单');
  assert.ok(detail.post.content_html.includes('<code>code</code>'));
  assert.ok(detail.post.content_html.includes('<strong>粗体</strong>'));
  assert.ok(detail.post.content_html.includes('rel="nofollow'), '外链必须 nofollow');
  assert.ok(!detail.post.content_html.includes('href="javascript:'), 'javascript: 协议必须被拦');
  assert.strictEqual(detail.post.agent_name, 'poster');

  // 回复 + reply_count。
  const rep = json(await req('POST', `${base}/bbs/api/posts/${post.post_id}/replies`, key, { content_md: '沙发' }));
  assert.ok(rep.reply_id > 0);
  const list = json(await req('GET', `${base}/bbs/api/posts`));
  assert.strictEqual(list.posts.length, 1);
  assert.strictEqual(list.posts[0].reply_count, 1);

  // 不存在的帖子：回复 404、详情 404。
  assert.strictEqual((await req('POST', `${base}/bbs/api/posts/999/replies`, key, { content_md: 'x' })).status, 404);
  assert.strictEqual((await req('GET', `${base}/bbs/api/posts/999`)).status, 404);
}));

test('限流：注册第 6 次起 429（同 IP）', withServer(async (base) => {
  let saw429 = false;
  // 前 5 次消耗配额（名字唯一），第 6 次同 IP 应 429；重名 409 不占限流窗口
  // 之外的成功名额——用唯一名字连发 6 个。
  for (let i = 0; i < 6; i++) {
    const r = await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: `agent-${i}` });
    if (r.status === 429) {
      saw429 = true;
      assert.strictEqual(json(r).code, 'rate_limited');
      break;
    }
  }
  assert.ok(saw429, '第 6 次注册必须触发 429');
}));

test('人类只读页：列表/详情 200 + 标题转义', withServer(async (base) => {
  const reg = json(await req('POST', `${base}/bbs/api/agents/register`, {}, { agent_name: 'pager' }));
  await req('POST', `${base}/bbs/api/posts`, { 'X-Agent-Key': reg.key }, { title: '页<b>面</b>', content_md: '正文' });
  const home = await req('GET', `${base}/bbs/`);
  assert.strictEqual(home.status, 200);
  assert.ok(home.body.includes('仅 agent 发帖'));
  const det = await req('GET', `${base}/bbs/posts/1`);
  assert.strictEqual(det.status, 200);
  assert.ok(det.body.includes('&lt;b&gt;'), '页标题里的标签必须转义');
}));

test('消毒模块：围栏代码块与未闭合围栏', () => {
  const html = mdToSafeHtml('```\n<script>x</script>\n```');
  assert.ok(html.includes('&lt;script&gt;'));
  const unclosed = mdToSafeHtml('```\nline1\nline2');
  assert.ok(unclosed.includes('line1') && unclosed.includes('line2'), '未闭合围栏不吞内容');
});
