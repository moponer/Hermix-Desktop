'use strict';

/**
 * relay 服务本体：Express + WebSocket —— 纯装配层。
 *
 * 这里只做四件事：中间件、凭据/审计端点、按注册表挂产品路由、listen。
 * **不认识任何具体产品的路由细节**——产品见 src/registry.js 与 src/products/；
 * WS 协议面见 src/connections.js。
 *
 * 两条 WS 入口路径（在 connections.js 内注册）：
 * - `GET /relay/node`   —— 电脑侧（PC 出站主动连）
 * - `GET /relay/phone`  —— 手机侧
 *
 * 两条都要求 `Authorization: Bearer <token>`，且 token 必须在本地登记过。
 * 握手期就比对自己的 `proto_version`——版本漂移必须在握手时报，而不是等
 * 消息「莫名消失」。
 */

const http = require('node:http');
const path = require('node:path');
const express = require('express');

const { PROTO_VERSION } = require('./proto');
const { openRegistry, bearerFromHeaders, deriveNodeIdFromPublicKey } = require('./auth');
const { openAudit } = require('./audit');
const { Router } = require('./router');
const { PRODUCTS, payloadProducts, proxyProducts } = require('./registry');
const { setupConnections } = require('./connections');

function createRelay({ dbPath, relayRoot } = {}) {
  // relayRoot：发布面（deploy/）与静态站（public/）的根。默认 = 包根，
  // 远端部署时 relay 进程的工作目录就是它（/www/wwwroot/hermix-relay）。
  const root = relayRoot || path.join(__dirname, '..');
  const registry = openRegistry(dbPath);
  const audit = openAudit(dbPath);
  const router = new Router({ audit });

  const app = express();
  app.disable('x-powered-by');

  // ── 反代型产品（registry: type=proxy）────────────────────────────
  // BBS 是 relay 同机的独立 Node 进程（决策笔记 2026-10-09-agent-bbs-integration）。
  // **必须挂在 express.json 之前**：body 解析器会消费请求流，反代必须在
  // 它之前拿到原始字节流（BBS 自己解析 JSON）。
  for (const p of proxyProducts()) {
    const upstreamPort = Number(new URL(p.target).port) || 80;
    app.use(p.mount, (req, res) => {
      const upstream = http.request(
        {
          host: new URL(p.target).hostname,
          port: upstreamPort,
          method: req.method,
          path: req.originalUrl, // 保留挂载前缀：上游路由按全路径注册
          headers: { ...req.headers, host: new URL(p.target).host },
        },
        (up) => {
          res.writeHead(up.statusCode || 502, up.headers);
          up.pipe(res);
        },
      );
      upstream.on('error', () => {
        // 上游进程没起/挂了：明确 502，不带出 relay 自身的堆栈。
        if (!res.headersSent) {
          res.status(502).json({ code: `${p.name}_down`, message: `${p.name} 服务未启动` });
        } else {
          res.end();
        }
      });
      req.pipe(upstream);
    });
  }

  // 只为凭据端点的 `{ label }` / `{ public_key }`。之前全站没有 JSON
  // body parser，这两个端点是第一批需要读请求体的入口。
  app.use(express.json({ limit: '4kb' }));

  app.get('/health', (_req, res) => {
    // 屏 1 展示的证书指纹。**必须是真证书的 sha256**，自己编一个常量就是
    // 自证——relay 被换掉指纹照旧，客户端无从察觉。
    //
    // 这里的值由部署时从 TLS 层导出、经 HERMIX_RELAY_CERT_SHA256 注入；**没有
    // 配就不给这个头**，让 App 明确显示「无法核对指纹」，而不是显示一个假的
    // 「已核对」。
    //
    // ⚠️ 必须先 setHeader 再 json —— json() 之后就发出去了，补都补不上。
    const fp = process.env.HERMIX_RELAY_CERT_SHA256
    if (fp && /^[0-9a-f]{64}$/i.test(fp)) {
      res.setHeader('x-hermix-cert-sha256', fp.toLowerCase())
    }
    res.json({
      ok: true,
      proto_version: PROTO_VERSION,
      ...router.snapshot(),
      audit_failures: audit.failureCount(),
    })
  });

  // ── 电脑侧凭据自动签发（enroll） ────────────────────────────────
  // 桌面端第一次启动时还没有 node 凭据。过去的流程是让用户登服务器敲
  // `relayctl add-node` 再把明文抄回桌面端——这是一台「零安装」应用不该有的
  // 人工环节。改为：桌面端把**自己的 E2EE 公钥**报上来，relay 从公钥推导
  // node_id（与 Rust 侧 derive_node_id 同算法，见 auth.js），签发并一次性
  // 下发凭据。用户全程无感。
  //
  // 安全边界（都是有意的取舍，见决策笔记 2026-10-11-relay-auto-enroll）：
  // · node_id 一律由公钥推导，**不接受客户端自报**——路由归属不能由请求方
  //   指定，否则凭空认领任意身份位。
  // · 先到先得 + 冲突拒绝（409）：同一 node_id 只签发一次。持公钥（拍过
  //   二维码）的攻击者无法在真电脑离线时补签一条占槽凭据；反过来，重装
  //   电脑要重取凭据就得登服务器手动签——这是把「凭据可匿名重取」这个更大
  //   的口子关掉换来的，值。
  // · 总量闸门：未撤销 node 凭据条数有上限。匿名端点必须有界，否则
  //   随机公钥刷库就是免费的存储 DoS。
  const ENROLL_NODE_CAP = 512;
  app.post('/api/relay/enroll-node', (req, res) => {
    const publicKey = typeof req.body?.public_key === 'string' ? req.body.public_key.trim() : '';
    const nodeId = deriveNodeIdFromPublicKey(publicKey);
    if (!nodeId) {
      return res.status(400).json({ error: 'public_key 必须是 65 字节未压缩 P-256 公钥的 base64' });
    }
    if (registry.hasActiveNode(nodeId)) {
      audit.write('enroll-node', nodeId, 'anon', 'node_already_enrolled');
      return res.status(409).json({
        error: '这个电脑身份已在 relay 登记过。凭据丢失请在服务器上用 relayctl 重新签发。',
        code: 'node_already_enrolled',
      });
    }
    if (registry.countActiveNodes() >= ENROLL_NODE_CAP) {
      audit.write('enroll-node', nodeId, 'anon', 'node_cap_reached');
      return res.status(503).json({ error: 'node 凭据签发额度已满，请在服务器上清理后重试' });
    }
    try {
      const token = registry.register({ nodeId, label: 'auto-enroll', role: 'node' });
      audit.write('enroll-node', nodeId, 'anon', 'node_enrolled');
      // 明文 token 仅此一次下发，与 relayctl / issue-phone 同一约定。
      res.json({ token, node_id: nodeId });
    } catch (e) {
      res.status(500).json({ error: `签发失败: ${e.message}` });
    }
  });

  // ── 签发手机凭据（凭据面，不碰 relay 协议面） ────────────────────
  // 桌面端用**自己的** node 凭据来换一条手机凭据，省掉「每加一台手机就登
  // 服务器敲一次 relayctl add-phone」。
  //
  // 为什么这样是安全的：node 凭据已经能代表这台电脑接入路由，再给它签发同
  // node 名下的手机凭据不扩大攻击面——拿到它的人本来就能读这台电脑的流量
  // （帧是密封的，读不到内容，但能投递）。反过来，**没有 node 凭据就拿不到
  // 手机凭据**，所以这不是一个可以匿名滥用的签发口。
  //
  // 明文 token 仅此一次下发，与 `relayctl` 同一约定。
  app.post('/api/relay/issue-phone', (req, res) => {
    const token = bearerFromHeaders(req.headers);
    if (!token) {
      return res.status(401).json({ error: '缺少 Authorization: Bearer <node 凭据>' });
    }
    const who = registry.authenticate(token);
    if (!who) {
      return res.status(401).json({ error: '凭据无效或已撤销' });
    }
    // 只认 node 角色：手机拿手机凭据来换手机凭据没有意义，只会让凭据扩散。
    if (who.role !== 'node') {
      return res.status(403).json({ error: '只有 node 凭据能签发手机凭据' });
    }
    const label = typeof req.body?.label === 'string' ? req.body.label.slice(0, 64) : '';
    // N:1：签发口也必须拦。只在握手处拦不够——凭据已经发出去、对方拿着它
    // 反复重连，每次都被拒，桌面端却还在正常出码，排查起来极难定位。真正的
    // 拦截点是「这台电脑现在是否已经有一台手机连着」。
    const bound = router.phoneHolder(router.slot(who.nodeId));
    if (bound !== null) {
      audit.write('issue-phone', who.nodeId, 'node', 'phone_issue_rejected');
      return res.status(409).json({
        error: '这台电脑已经连着一台手机了。换手机请先在桌面端「设备管理」里撤销原设备，再重新扫码。',
        code: 'desktop_already_paired',
      });
    }
    try {
      const issued = registry.register({ nodeId: who.nodeId, label, role: 'phone' });
      audit.write('issue-phone', who.nodeId, 'node', 'phone_issued');
      res.json({ token: issued, node_id: who.nodeId });
    } catch (e) {
      res.status(500).json({ error: `签发失败: ${e.message}` });
    }
  });

  // ── 撤销一台手机的 relay 访问 ──────────────────────────────────
  // 电脑端在 hermix 里撤销某台设备时调用这里。**只认凭据哈希**，不认明文——
  // 电脑端从没见过那台手机的明文 `hrel_*`，它只有手机在配对时报上来的
  // `sha256(token)`。
  //
  // 与 `issue-phone` 同一个信任前提：调用方必须是该 node 的 node 凭据。
  // 手机只能撤自己（`issue-phone` 那侧已经用 role 挡住了）。
  app.post('/api/relay/revoke-phone', (req, res) => {
    const token = bearerFromHeaders(req.headers);
    if (!token) {
      return res.status(401).json({ error: '缺少 Authorization: Bearer <node 凭据>' });
    }
    const who = registry.authenticate(token);
    if (!who) return res.status(401).json({ error: '凭据无效或已撤销' });
    if (who.role !== 'node') {
      return res.status(403).json({ error: '只有 node 凭据能撤销手机访问' });
    }

    // `express.json()` 只解析 `application/json`。别人发来 `text/plain`（fetch
    // 的字符串 body 默认就是它）时 req.body 是空的——这时说「哈希格式不对」是
    // 误导，会把人引到完全错误的方向上。
    if (!req.body || typeof req.body !== 'object' || Object.keys(req.body).length === 0) {
      return res.status(400).json({
        error: '请求体为空或不是 JSON（content-type 须为 application/json）',
      });
    }
    const hash = typeof req.body.token_hash === 'string' ? req.body.token_hash.trim() : '';
    if (!/^[0-9a-f]{64}$/.test(hash)) {
      return res.status(400).json({ error: 'token_hash 必须是 64 位小写 hex' });
    }

    // 顺序要紧：**先撤销凭据，再断连接**。反过来的话，连接在被踢之前还有
    // 一瞬间能用已撤销的凭据发东西。
    const revoked = registry.revokeByHash(hash);
    const dropped = router.dropPhone(hash);
    audit.write('revoke-phone', who.nodeId, 'node', revoked ? 'phone_revoked' : 'already_revoked');
    // `revoked=false` 且 `dropped=0` = 这台手机从来没在这个 relay 上登记过，
    // 幂等返回 200：hermix 侧的撤销仍然成立，不该因为"relay 那边没有记录"
    // 就让整个操作失败。
    res.json({ revoked, dropped, node_id: who.nodeId });
  });

  // ── 产品路由（registry: type=payload）────────────────────────────
  // 每个产品一个同构模块（products/<name>.js），deployDir 指向版本化的
  // 发布面（symlink current）。**顺序即语义**：payload 路由必须挂在
  // express.static 之前，否则静态层会抢先命中 manifest.json/zip，
  // 退化成没有校验头的裸文件，客户端更新链路静默降级。
  const publicDir = path.join(root, 'public');
  for (const p of payloadProducts()) {
    const mod = require(`./products/${p.name}`);
    mod.register(app, {
      deployDir: path.join(root, p.deployDir),
      mount: p.mount,
      publicDir,
    });
  }
  require('./products/pages').register(app, { publicDir });

  // ── 官网静态站（多页分站） ──────────────────────────────────
  // 官网是多个 HTML 页面 + assets/，不是单文件，所以要把 public/ 整个托管出去。
  app.use(express.static(publicDir, {
    setHeaders(res, filePath) {
      // 图片几乎不变，长缓存；CSS/JS 给 5 分钟——改完不用等浏览器过期，
      // 但同一会话内翻页不再回源。HTML 不设，留给 ETag 协商：官网页面
      // 改完必须立刻可见，缓存它等于给自己埋雷。
      if (/\.(png|jpe?g|webp|ico|svg|woff2?)$/i.test(filePath)) {
        res.setHeader('Cache-Control', 'public, max-age=604800');
      } else if (/\.(css|js)$/i.test(filePath)) {
        res.setHeader('Cache-Control', 'public, max-age=300');
      }
    },
  }));

  const server = http.createServer(app);
  const connections = setupConnections({ server, registry, audit, router });

  return {
    app,
    server,
    router,
    registry,
    audit,
    attachConn: connections.attachConn,
    listen(port, cb) {
      server.listen(port, cb);
    },
    close() {
      connections.close();
      server.close();
      registry.close();
      audit.close();
    },
  };
}

module.exports = { createRelay };
