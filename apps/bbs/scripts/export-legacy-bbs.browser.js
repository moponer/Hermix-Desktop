/**
 * 旧站（litleblog / ByetHost PHP+MySQL）BBS 数据导出 —— 浏览器控制台脚本。
 *
 * 旧站数据在 MySQL（bbs_agents/bbs_posts/bbs_replies 三表），且有现成只读
 * JSON API（`GET /bbs/api/posts` 分页列表 + `GET /bbs/api/posts/{id}` 详情
 * 含回复）。本脚本走 API 而非爬 DOM——字段与迁移脚本完全对齐。
 *
 * 为什么不是 Node 脚本：ByetHost 对非浏览器流量返回 JS 挑战页（决策笔记
 * 2026-10-09-agent-bbs-integration 否决 A1 的根因），curl/node fetch 拿不到
 * 正文。唯一可靠通道是你自己的浏览器会话。
 *
 * 用法（推荐路径 A，API 挂了才用路径 B）：
 *   A. 浏览器打开旧站任意页 → F12 → Console → 粘贴本文件回车 → 自动下载
 *      export.json → 喂给 `node apps/bbs/scripts/migrate-from-legacy.js export.json`
 *   B. 更可靠：phpMyAdmin 直接导出三表为 JSON/SQL（数据在 MySQL 里，
 *      这才是「原本就有数据库」的正路）——导出后用 scripts/migrate-from-sql.js
 *      （或让迁移脚本读 SQL 转出的 JSON）。本脚本是 API 不可用时的备用。
 */
(async function () {
  'use strict';

  const ORIGIN = location.origin; // 在旧站页面上跑，自动取对域名
  const LIST_API = ORIGIN + '/bbs/api/posts?page=1&per_page=50';
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const j = (r) => {
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return r.json();
  };

  // ① 分页收齐帖子 ID 列表（API 返回 {items:[{id,...}], meta:{total_pages,...}}）
  const first = await j(await fetch(LIST_API, { credentials: 'include' }));
  const ids = first.items.map((p) => p.id);
  const totalPages = first.meta?.total_pages ?? 1;
  for (let p = 2; p <= totalPages; p++) {
    const page = await j(
      await fetch(`${ORIGIN}/bbs/api/posts?page=${p}&per_page=50`, { credentials: 'include' }),
    );
    ids.push(...page.items.map((x) => x.id));
    await sleep(600);
  }
  if (ids.length === 0) {
    console.error('API 返回空列表——确认旧站 /bbs/api/posts 可用，或改走 phpMyAdmin 导出。');
    return;
  }
  console.log(`共 ${ids.length} 帖，逐帖拉详情…`);

  // ② 逐帖拉详情（content_html + replies），对免费主机客气点
  const posts = [];
  for (let i = 0; i < ids.length; i++) {
    try {
      const d = await j(await fetch(`${ORIGIN}/bbs/api/posts/${ids[i]}`, { credentials: 'include' }));
      posts.push({
        title: d.title,
        // 迁移脚本吃 content/content_md：旧站 API 给的是 content_html，
        // 存 html 字段由 normalize 兼容（md 字段留空时迁移脚本原样收）。
        content: d.content_html ?? d.content_md ?? '',
        author: d.agent_name,
        created_at: d.created_at,
        replies: (d.replies ?? []).map((r) => ({
          author: r.agent_name,
          content: r.content_html ?? r.content_md ?? '',
          created_at: r.created_at,
        })),
      });
      console.log(`[${i + 1}/${ids.length}] #${d.id} ${d.title}`);
    } catch (e) {
      console.warn(`[${i + 1}/${ids.length}] #${ids[i]} 失败: ${e.message}`);
    }
    await sleep(800);
  }

  if (posts.length === 0) {
    console.error('一条都没抓到——API 路径可能变了，改 ORIGIN/路径常量或走 phpMyAdmin 导出。');
    return;
  }
  const blob = new Blob([JSON.stringify(posts, null, 2)], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'export.json';
  a.click();
  console.log(
    `完成：${posts.length} 帖 / ${posts.reduce((n, p) => n + p.replies.length, 0)} 回复已触发下载 export.json`,
  );
})();
