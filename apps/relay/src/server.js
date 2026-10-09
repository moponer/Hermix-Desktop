'use strict';

/**
 * relay 服务本体：Express + WebSocket。
 *
 * 两条入口路径：
 * - `GET /relay/node`   —— 电脑侧（PC 出站主动连）
 * - `GET /relay/phone`  —— 手机侧
 *
 * 两条都要求 `Authorization: Bearer <token>`，且 token 必须在本地登记过。
 * 握手期就比对自己的 `proto_version`——版本漂移必须在握手时报，而不是等
 * 消息「莫名消失」。
 *
 * 关于认证的两句话，别读混：
 * - 这里校验的是「这个 token 是不是本 relay 登记过、绑到哪个 node」，属**路由级**；
 * - hermix 侧在链路另一端独立校验同一凭据 + 请求签名，那才是**授权级**。
 * relay 不参与授权判定，也**不解析内层帧**。
 */

const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const express = require('express');
const { WebSocketServer } = require('ws');

const { PROTO_VERSION, TAG, buildEnvelope, parseEnvelope } = require('./proto');

function hashFileSync(p) {
  try {
    return crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
  } catch {
    return null;
  }
}
const { openRegistry, bearerFromHeaders } = require('./auth');
const { openAudit } = require('./audit');
const { Router } = require('./router');

function createRelay({ dbPath }) {
  const registry = openRegistry(dbPath);
  const audit = openAudit(dbPath);
  const router = new Router({ audit });

  const app = express();
  app.disable('x-powered-by');

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

  // ── 安装页 / APK 下载（静态，不碰 relay 协议面） ──────────────
  // APK 由运维放到 <relay 根>/public/hermix.apk；没放就如实说"未上传"，
  // 不给假按钮。文件指纹按 mtime 缓存——每次请求重算 SHA-256 会把 CPU
  // 打满，而安装包一天也传不了几次。
  const publicDir = path.join(__dirname, '..', 'public');
  const apkPath = path.join(publicDir, 'hermix.apk');
  let apkCache = { mtimeMs: 0, sha256: null };

  function apkInfoSync() {
    let st;
    try {
      st = fs.statSync(apkPath);
    } catch {
      return { available: false, version: null, size: 0, sha256: null, updatedAt: null };
    }
    if (apkCache.mtimeMs !== st.mtimeMs) {
      apkCache = { mtimeMs: st.mtimeMs, sha256: hashFileSync(apkPath) };
    }
    const version = (() => {
      try {
        const html = fs.readFileSync(path.join(publicDir, 'index.html'), 'utf8');
        const m = html.match(/HERMIX_M_VERSION\s*=\s*["']([^"']+)["']/) || html.match(/版本[：:]?\s*v?([0-9][^<\s]*)/);
        return m ? m[1] : null;
      } catch {
        return null;
      }
    })();
    return {
      available: true,
      version,
      size: st.size,
      sha256: apkCache.sha256,
      updatedAt: new Date(st.mtimeMs).toISOString(),
    };
  }

  app.get('/', (_req, res) => {
    res.sendFile(path.join(publicDir, 'index.html'));
  });

  app.get('/static-mascot.png', (_req, res) => {
    res.sendFile(path.join(publicDir, 'static-mascot.png'));
  });

  app.get('/api/app-info', (_req, res) => {
    res.json(apkInfoSync());
  });

  app.get('/download/hermix.apk', (_req, res) => {
    let st;
    try {
      st = fs.statSync(apkPath);
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('安装包尚未上传（运维需把 APK 放到 relay 目录的 public/hermix.apk 后重启项目）');
    }
    res.setHeader('Content-Type', 'application/vnd.android.package-archive');
    res.setHeader('Content-Length', st.size);
    res.setHeader('Content-Disposition', 'attachment; filename="hermix.apk"');
    res.setHeader('X-Checksum-Sha256', apkInfoSync().sha256 || '');
    fs.createReadStream(apkPath).pipe(res);
  });

  // ── 桌面端在线更新面（静态，不碰 relay 协议面） ──────────────
  // 运维把发布产物放到 <relay 根>/public/desktop/：
  //   manifest.json   —— 版本清单（见 docs/notes/implemented/feature/2026-10-09-online-update.md）
  //   latest.zip      —— 当前版桌面整包（文件名恒定，manifest 的 url 字段永不变更）
  // manifest 缺失/损坏时如实报 404，客户端静默降级为「无更新」，不给假提示。
  const desktopDir = path.join(publicDir, 'desktop');
  const manifestPath = path.join(desktopDir, 'manifest.json');

  app.get('/desktop/manifest.json', (_req, res) => {
    let raw;
    try {
      raw = fs.readFileSync(manifestPath, 'utf8');
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('更新清单尚未上传（运维需把 manifest.json 放到 relay 目录的 public/desktop/）');
    }
    const manifest = JSON.parse(raw);
    // sha256 直读文件清单旁的 latest.zip，响应头带给客户端做下载前预检；
    // 清单里也存了一份（发布脚本写入），两处不一致说明上传只完成了一半。
    const zipSha = hashFileSync(path.join(desktopDir, 'latest.zip'));
    if (zipSha) res.setHeader('X-Checksum-Sha256', zipSha);
    res.setHeader('Cache-Control', 'no-cache');
    res.json(manifest);
  });

  app.get('/desktop/latest.zip', (_req, res) => {
    let st;
    try {
      st = fs.statSync(path.join(desktopDir, 'latest.zip'));
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('更新包尚未上传（运维需把 latest.zip 放到 relay 目录的 public/desktop/）');
    }
    res.setHeader('Content-Type', 'application/zip');
    res.setHeader('Content-Length', st.size);
    res.setHeader('Content-Disposition', 'attachment; filename="hermix-desktop-update.zip"');
    res.setHeader('X-Checksum-Sha256', hashFileSync(path.join(desktopDir, 'latest.zip')) || '');
    fs.createReadStream(path.join(desktopDir, 'latest.zip')).pipe(res);
  });

  // ── spiker-tauri 桌面端在线更新面（静态，复用 relay 443，零新端口） ──
  // 运维把发布产物放到 <relay 根>/public/spiker/：
  //   manifest.json   —— 版本清单
  //   latest.zip      —— 当前版整包（文件名恒定，manifest 的 url 字段永不变更）
  // 与 /desktop/ 同构，spiker 无 sidecar，zip 只含 exe + resources。
  const spikerDir = path.join(publicDir, 'spiker');
  const spikerManifest = path.join(spikerDir, 'manifest.json');

  app.get('/spiker/manifest.json', (_req, res) => {
    let raw;
    try {
      raw = fs.readFileSync(spikerManifest, 'utf8');
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('更新清单尚未上传（运维需把 manifest.json 放到 relay 目录的 public/spiker/）');
    }
    const manifest = JSON.parse(raw);
    const zipSha = hashFileSync(path.join(spikerDir, 'latest.zip'));
    if (zipSha) res.setHeader('X-Checksum-Sha256', zipSha);
    res.setHeader('Cache-Control', 'no-cache');
    res.json(manifest);
  });

  app.get('/spiker/latest.zip', (_req, res) => {
    let st;
    try {
      st = fs.statSync(path.join(spikerDir, 'latest.zip'));
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('更新包尚未上传（运维需把 latest.zip 放到 relay 目录的 public/spiker/）');
    }
    res.setHeader('Content-Type', 'application/zip');
    res.setHeader('Content-Length', st.size);
    res.setHeader('Content-Disposition', 'attachment; filename="spiker-tauri-update.zip"');
    res.setHeader('X-Checksum-Sha256', hashFileSync(path.join(spikerDir, 'latest.zip')) || '');
    fs.createReadStream(path.join(spikerDir, 'latest.zip')).pipe(res);
  });

  const server = http.createServer(app);
  const wss = new WebSocketServer({ noServer: true });

  server.on('upgrade', (req, socket, head) => {
    const url = new URL(req.url, 'http://placeholder');
    const kind = url.pathname === '/relay/node' ? 'node' : url.pathname === '/relay/phone' ? 'phone' : null;
    if (kind === null) {
      // 未知路径：明确拒绝，不静默接受（避免路径被改写后签名失效这类问题
      // 在这里变成难以察觉的"连上了但什么都收不到"）。
      socket.write('HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }

    // --- 鉴权：登记过 + 未撤销 ---
    const token = bearerFromHeaders(req.headers);
    const identity = token ? registry.authenticate(token) : null;
    if (identity === null) {
      // 统一 401，不区分"没 token"与"token 无效"——细分等于给攻击者一个
      // 免费 oracle。与 hermix 侧 security::DeviceAuthError 同一条理由。
      audit.write('auth', null, kind, 'denied');
      socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    // 角色必须与入口路径一致：手机不能占电脑的槽（否则一条手机连接能顶掉
    // 真正的电脑连接，症状是"电脑突然连不上"）。
    if (identity.role !== kind) {
      audit.write('auth', identity.nodeId, kind, 'role_mismatch');
      socket.write('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }

    wss.handleUpgrade(req, socket, head, (ws) => {
      // token 哈希在这里算一次：手机侧要用它做「连接 ↔ 手机」绑定键，
      // 而明文 token 此后不再需要。
      wss.emit('connection', ws, req, {
        kind,
        identity,
        tokenHash: require('./auth').hashToken(token),
      });
    });
  });

  wss.on('connection', (ws, _req, meta) => {
    attachConn(ws, meta.kind, meta.identity, meta.tokenHash);
  });

  /**
   * 把一条连接接进路由表，并装上收发逻辑。
   *
   * 单独抽出来是为了测试：不必真开一个 HTTP 服务也能验证路由行为。
   */
  function attachConn(ws, kind, identity, tokenHash) {
    const nodeId = identity.nodeId;

    ws.sendFrame = (envelope) => {
      if (ws.readyState !== ws.OPEN) return;
      if (envelope && typeof envelope === 'object') {
        // 内层 payload **原样**转发：这里不重新构造 payload 字段，
        // 只补/保外层信封。已删的字段不会复活。
        ws.send(
          JSON.stringify({
            proto_version: envelope.proto_version ?? PROTO_VERSION,
            tag: envelope.tag,
            ...(envelope.token_hash ? { token_hash: envelope.token_hash } : {}),
            ...(envelope.node_id ? { node_id: envelope.node_id } : {}),
            ...(envelope.payload !== undefined ? { payload: envelope.payload } : {}),
          }),
        );
      } else {
        ws.send(envelope);
      }
    };
    ws.sendRaw = (text) => {
      if (ws.readyState === ws.OPEN) ws.send(text);
    };

    if (kind === 'node') {
      router.attachNode(nodeId, ws);
      ws.sendRaw(buildEnvelope(TAG.ACK, { node_id: nodeId, detail: 'node_registered' }));
    } else {
      router.attachPhone(nodeId, tokenHash, ws);
      ws.sendRaw(buildEnvelope(TAG.ACK, { node_id: nodeId, detail: 'phone_registered' }));
    }

    ws.on('message', (data, isBinary) => {
      // 二进制帧按文本解：hermix 侧的 WsTransport 接受客户端用二进制发 JSON
      // （见 ws_transport.rs），relay 不必比它更严。
      const raw = isBinary ? data.toString('utf8') : String(data);
      const parsed = parseEnvelope(raw);
      if (!parsed.ok) {
        if (parsed.versionMismatch) {
          // 版本漂移：明确拒绝并说明，别让它退化成"能连但消息消失"。
          ws.sendRaw(
            buildEnvelope(TAG.CLOSE, {
              node_id: nodeId,
              detail: 'proto_version_mismatch',
            }),
          );
          ws.close(1002, 'proto_version mismatch');
          return;
        }
        // 脏数据跳过，不断连接：relay 不该被一帧垃圾打掉。
        audit.write('route', nodeId, kind, `bad_frame:${parsed.reason}`);
        return;
      }
      const { envelope } = parsed;
      if (envelope.tag === TAG.CLOSE) {
        ws.close(1000, 'peer closed');
        return;
      }
      if (envelope.tag !== TAG.ROUTE) {
        // hello / ack 是链路自身的握手帧，不当作业务数据转发
        return;
      }
      if (kind === 'phone') {
        router.phoneToNode(nodeId, tokenHash, envelope);
      } else {
        router.nodeToPhones(nodeId, envelope);
      }
    });

    ws.on('close', () => router.detach(ws));
    ws.on('error', () => router.detach(ws));
  }

  return {
    app,
    server,
    router,
    registry,
    audit,
    attachConn,
    listen(port, cb) {
      server.listen(port, cb);
    },
    close() {
      wss.clients.forEach((c) => c.terminate());
      wss.close();
      server.close();
      registry.close();
      audit.close();
    },
  };
}

module.exports = { createRelay };
