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
 * 手机 → 电脑是 1:1 转发。**一台桌面同时只允许一台手机接入**（见
 * `attachPhone`）：手机端可以管多台桌面，但每台桌面的出站帧只流向当前那**一台**
 * 手机连接。换手机必须先在桌面端撤销，否则那条旧连接会一直收着这台电脑的广播。
 */

const { TAG, buildEnvelope } = require('./proto');

/** 一个 node 名下：一条电脑连接 + **至多一条**手机连接（N:1，见 attachPhone）。 */
class NodeSlot {
  constructor(nodeId) {
    this.nodeId = nodeId;
    /** @type {object|null} 电脑侧连接 */
    this.nodeConn = null;
    /** 电脑侧连接的凭据哈希。**没有它就没法判断"来顶号的是不是同一个人"**。 */
    this.nodeHash = null;
    /**
     * 手机连接：key = 手机 token 哈希。
     * N:1 之后**至多一条**——用 Map 而不是裸字段，是为了让 `dropPhone`、
     * `detach` 这两处按凭据定位的既有逻辑不用改。
     */
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

  /**
   * 电脑侧接入。
   *
   * ## 顶号必须凭据相等
   *
   * 原实现无条件替换（`node_replaced`），于是**任何持有该 node_id 的另一条凭据
   * 都能把真电脑踢下线**。这不是理论风险：`node_id` 只有 32 位熵，而且它就明晃晃
   * 印在二维码的 `server_id` 与桌面端设置页里，猜到或抄到都不难。
   *
   * 改成「只有同一凭据才能顶掉旧连接」之后：真电脑重连（同一 `hrel_*`）行为不变，
   * 而别人拿另一条凭据来抢同一个 node_id 会被拒绝。这条同时也是开放自助注册的前置
   * 条件——没有它，任何人都能凭空注册一台电脑把受害者挤下线。
   *
   * @returns {'attached'|'replaced'|'conflict'}
   */
  attachNode(nodeId, tokenHash, conn) {
    const s = this.slot(nodeId);
    if (s.nodeConn !== null && s.nodeHash !== tokenHash) {
      this.audit.write('route', nodeId, 'node', 'node_evict_rejected');
      return 'conflict';
    }
    const replaced = s.nodeConn !== null;
    s.nodeConn = conn;
    s.nodeHash = tokenHash;
    this.audit.write('route', nodeId, 'node', replaced ? 'node_replaced' : 'node_attached');
    return replaced ? 'replaced' : 'attached';
  }

  /**
   * 断开并移除持有该凭据哈希的手机连接。
   *
   * 撤销必须**立刻断连**，否则「撤销」只是个界面动作：那台手机虽然发不出帧
   * （hermix 侧 `verify_frame_auth` 会拒），却还在持续接收广播给该 node 的所有
   * 出站帧。`auth.js` 的判据原话是「否则撤销按钮就是个摆设」。
   *
   * @returns {number} 实际断开的连接数
   */
  dropPhone(tokenHash) {
    let n = 0;
    for (const [nodeId, s] of this.slots) {
      const conn = s.phoneConns.get(tokenHash);
      if (!conn) continue;
      s.phoneConns.delete(tokenHash);
      try {
        conn.close(1000, 'revoked');
      } catch {
        // 连接可能已经死了；清理路由表才是要紧的，close 失败不影响。
      }
      this.audit.write('disconnect', nodeId, 'phone', 'phone_revoked');
      n += 1;
    }
    // 整个 node 空了就回收
    for (const [nodeId, s] of [...this.slots]) {
      if (s.nodeConn === null && s.phoneConns.size === 0) this.slots.delete(nodeId);
    }
    return n;
  }

  /**
   * 手机侧接入。
   *
   * ## 一台电脑同时只能被一台手机连着（N:1）
   *
   * 手机端可以管多台桌面（家里/公司），但反过来：**每台桌面同时只允许一台手机
   * 接入**。别的手机要连，得先在桌面端把原来那台撤销掉。
   *
   * 为什么必须约束：电脑的出站帧是按 `node_id` 广播给该 node 名下**所有**手机
   * 连接的（`nodeToPhones`）。不约束就等于一台电脑的会话内容同时流向任意多台
   * 手机——只要其中一条凭据泄露，那台电脑就同时暴露给所有持有者。
   *
   * 拒绝而不是顶替，理由与 `attachNode` 的 conflict 完全一致：静默踢掉受害者
   * 等于让凭据泄露者直接接管那台电脑。顶替会把「先撤销」这个显式动作抹掉。
   *
   * 同一 token 的重连照旧放行——那是手机自己的断线重连，不是换人。
   *
   * @returns {'attached'|'replaced'|'conflict'}
   */
  attachPhone(nodeId, tokenHash, conn) {
    const s = this.slot(nodeId);
    // 占着这个槽的若是**别的**凭据，就是第二台手机来抢：拒绝。
    const holder = this.phoneHolder(s);
    if (holder !== null && holder !== tokenHash) {
      this.audit.write('route', nodeId, 'phone', 'phone_second_bind_rejected');
      return 'conflict';
    }
    const replaced = holder === tokenHash;
    s.phoneConns.set(tokenHash, conn);
    this.audit.write('route', nodeId, 'phone', replaced ? 'phone_replaced' : 'phone_attached');
    return replaced ? 'replaced' : 'attached';
  }

  /**
   * 该 node 当前占槽的手机凭据哈希；没有手机接入时为 null。
   *
   * N:1 之后 `phoneConns` 最多一条，但**遍历取值而不是直接读某个键**——这样
   * 「单槽」这条不变量一旦被写坏，这里立刻暴露，而不是悄悄按 Map 语义跑下去。
   */
  phoneHolder(s) {
    for (const hash of s.phoneConns.keys()) return hash;
    return null;
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
    // 无条件盖章：PC 侧的配对错误预算按「这条帧来自哪条手机凭据」分桶，
    // 并校验「消费配对码的连接 = 扫码那条凭据」。章必须由 relay 盖——手机
    // 自报的 token_hash 可以随口伪造，信它等于预算形同虚设。
    envelope.token_hash = tokenHash;
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
