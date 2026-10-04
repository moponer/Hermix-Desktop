'use strict';

/**
 * 路由表：**唯一含转发逻辑的文件**。
 *
 * 一条连接进来时，relay 要做的事只有：
 * 1. 看这条连接自称的 token 哈希对应哪个 node；
 * 2. 把帧**原样**交给那个 node 的连接（若在）或反向交给该 node 名下
 *    某条手机连接（若是手机侧）；
 * 3. 记一行审计。
 *
 * relay **不做**的事，逐条对应 README 的硬约束：
 * - 不解析 `payload`（里面是什么与它无关）；
 * - 不改写 method / path / `Authorization` / `X-Hermix-Signature`
 *   （签名覆盖 `METHOD\npath`，改了就全废）；
 * - 不校验 hermix 侧授权（那是链路另一端的事）。
 *
 * 手机 → 电脑是 1:1 转发；同一 node 名下多台手机时，每条手机连接只与
 * 「它自己与该 node 之间」的配对绑定——多台手机的消息**不会**互相可见。
 */

const { TAG, buildEnvelope } = require('./proto');

/** 一个 node 名下：一条电脑连接 + 多条手机连接。 */
class NodeSlot {
  constructor(nodeId) {
    this.nodeId = nodeId;
    /** @type {object|null} 电脑侧连接 */
    this.nodeConn = null;
    /** @type {Map<string, object>} 手机连接：key = 手机 token 哈希 */
    this.phoneConns = new Map();
  }

  get phoneCount() {
    return this.phoneConns.size;
  }
}

class Router {
  constructor({ audit }) {
    /** @type {Map<string, NodeSlot>} */
    this.slots = new Map();
    this.audit = audit;
  }

  slot(nodeId) {
    let s = this.slots.get(nodeId);
    if (!s) {
      s = new NodeSlot(nodeId);
      this.slots.set(nodeId, s);
    }
    return s;
  }

  /** 电脑侧接入。同一 node 重连时顶掉旧连接（出站重连的常态）。 */
  attachNode(nodeId, conn) {
    const s = this.slot(nodeId);
    const replaced = s.nodeConn !== null;
    s.nodeConn = conn;
    this.audit.write('route', nodeId, 'node', replaced ? 'node_replaced' : 'node_attached');
    return replaced;
  }

  /** 手机侧接入。同一 token 重连顶掉旧连接。 */
  attachPhone(nodeId, tokenHash, conn) {
    const s = this.slot(nodeId);
    const replaced = s.phoneConns.has(tokenHash);
    s.phoneConns.set(tokenHash, conn);
    this.audit.write('route', nodeId, 'phone', replaced ? 'phone_replaced' : 'phone_attached');
    return replaced;
  }

  detach(conn) {
    for (const [nodeId, s] of this.slots) {
      if (s.nodeConn === conn) {
        s.nodeConn = null;
        this.audit.write('disconnect', nodeId, 'node', 'node_detached');
      }
      for (const [hash, phoneConn] of [...s.phoneConns]) {
        if (phoneConn === conn) {
          s.phoneConns.delete(hash);
          this.audit.write('disconnect', nodeId, 'phone', 'phone_detached');
        }
      }
      // 整个 node 空了就回收槽位，否则长跑进程会攒出一堆空 Map
      if (s.nodeConn === null && s.phoneConns.size === 0) {
        this.slots.delete(nodeId);
      }
    }
  }

  /**
   * 从手机侧来的一帧 → 交给对应 node。
   *
   * `envelope` 已经是解析好的外层信封；**payload 原样透传**（不解析、不改写、
   * 不重新编码字段顺序）。
   *
   * @returns {'delivered'|'no_node'}
   */
  phoneToNode(nodeId, tokenHash, envelope) {
    const s = this.slots.get(nodeId);
    if (!s || !s.nodeConn) {
      this.audit.write('route', nodeId, 'phone', 'dropped_no_node');
      return 'no_node';
    }
    this.audit.write('route', nodeId, 'phone', 'delivered');
    s.nodeConn.sendFrame(envelope);
    return 'delivered';
  }

  /**
   * 从电脑侧来的一帧 → 交给**某一条**手机连接。
   *
   * 电脑侧不指定具体手机（它只有一个会话），所以：只有一条手机连接时直接给它；
   * 有多条时**全部**给（同一 node 名下多台手机本来就在配对同一台电脑，用户
   * 预期是两端都看到）。这里不做负载均衡——那会让"我的审批卡出现在我手机上"
   * 变成概率事件。
   *
   * @returns {number} 收到该帧的手机连接数
   */
  nodeToPhones(nodeId, envelope) {
    const s = this.slots.get(nodeId);
    if (!s || s.phoneConns.size === 0) {
      this.audit.write('route', nodeId, 'node', 'dropped_no_phone');
      return 0;
    }
    for (const conn of s.phoneConns.values()) {
      conn.sendFrame(envelope);
    }
    this.audit.write('route', nodeId, 'node', `delivered_x${s.phoneConns.size}`);
    return s.phoneConns.size;
  }

  /** 诊断快照（`/health` 与 `relayctl status`）。只回计数，不回 token。 */
  snapshot() {
    const nodes = [];
    for (const [nodeId, s] of this.slots) {
      nodes.push({
        node_id: nodeId,
        node_connected: s.nodeConn !== null,
        phones: s.phoneConns.size,
      });
    }
    return { nodes };
  }

  /** 广播 close 给某 node 名下的所有连接。 */
  closeNode(nodeId) {
    const s = this.slots.get(nodeId);
    if (!s) return 0;
    let n = 0;
    const frame = buildEnvelope(TAG.CLOSE, { node_id: nodeId });
    if (s.nodeConn) {
      s.nodeConn.sendRaw(frame);
      n += 1;
    }
    for (const conn of s.phoneConns.values()) {
      conn.sendRaw(frame);
      n += 1;
    }
    return n;
  }
}

module.exports = { Router, NodeSlot };
