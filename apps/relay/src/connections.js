//! 连接层装配：WS upgrade 鉴权 + 收发逻辑（attachConn）。
//! 从 server.js 原样搬移（行为不变）——server.js 退回纯装配层后，
//! 这里的代码是 relay 的协议面，与产品路由（products/）互不相干。

'use strict';

const { WebSocketServer } = require('ws');
const { PROTO_VERSION, TAG, buildEnvelope, parseEnvelope } = require('./proto');
const { bearerFromHeaders, hashToken } = require('./auth');

function setupConnections({ server, registry, audit, router }) {
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
        tokenHash: hashToken(token),
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
        //
        // ⚠️ 白名单必须覆盖 `session_id` 与 `auth`。这两个字段不是可选的冗余：
        // PC 侧按 session_id 拆包（缺了就整帧丢弃），并用 auth 里的签名在
        // 重建的 `/v1/ws-stream/{sid}` 上复验；更底一层，密封帧的 AAD
        // (`v=…;tag=…;sid=…;nid=…`) 把 session_id 算进去了，丢掉它
        // 连解密都会失败。漏任何一个的表现都是「连上了但消息莫名消失」。
        ws.send(
          JSON.stringify({
            proto_version: envelope.proto_version ?? PROTO_VERSION,
            tag: envelope.tag,
            ...(envelope.token_hash ? { token_hash: envelope.token_hash } : {}),
            ...(envelope.node_id ? { node_id: envelope.node_id } : {}),
            ...(envelope.session_id ? { session_id: envelope.session_id } : {}),
            ...(envelope.auth ? { auth: envelope.auth } : {}),
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
      const outcome = router.attachNode(nodeId, tokenHash, ws);
      if (outcome === 'conflict') {
        // 别人的凭据来抢这个 node_id：明确拒绝，并说清是"已有电脑在线"。
        // 静默踢掉真电脑是 relay 最不该做的事——那等于让任何人把受害者挤下线。
        ws.sendRaw(
          buildEnvelope(TAG.CLOSE, {
            node_id: nodeId,
            detail: 'node_id_taken_by_another_credential',
          }),
        );
        ws.close(4003, 'node_id already connected with a different credential');
        return;
      }
      ws.sendRaw(buildEnvelope(TAG.ACK, { node_id: nodeId, detail: 'node_registered' }));
    } else {
      const outcome = router.attachPhone(nodeId, tokenHash, ws);
      if (outcome === 'conflict') {
        // 这台电脑已经连着**另一台**手机了：明确拒绝，并说清要先去桌面端撤销。
        // 放行等于一台电脑的会话内容同时流向任意多台手机——N:1 的全部意义就在
        // 这道门上（详见 router.attachPhone）。
        ws.sendRaw(
          buildEnvelope(TAG.CLOSE, {
            node_id: nodeId,
            detail: 'desktop_already_paired_with_another_phone',
          }),
        );
        ws.close(4009, 'this desktop is already connected to another phone');
        return;
      }
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
    wss,
    attachConn,
    close() {
      wss.clients.forEach((c) => c.terminate());
      wss.close();
    },
  };
}

module.exports = { setupConnections };
