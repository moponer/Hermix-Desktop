# Decision Note: 多端连接 / 配对链路加固（对标 DSH 远控生态 + OpenClaw 两层审批）

Status: proposed — 差距审计已完成，方案未动工，等拍板。

范围：桌面端 ↔ 移动端这条链路（设备发现、配对协议、认证与加密传输、会话生命周期、网络切换与重连、跨平台）。
与同日两篇笔记不重叠：[capability-alignment-audit](2026-10-18-capability-alignment-audit.md)（能力面的 ZCode 对齐）、
[mechanism-level-alignment](2026-10-18-mechanism-level-alignment.md)（机制级源码抄写）。
前情：[2026-10-10-mobile-pairing-via-relay](../../implemented/feature/2026-10-10-mobile-pairing-via-relay.md)
（relay 独大 + 局域网直连退役）、[2026-10-04-mobile-relay-selfbuilt](../feature/2026-10-04-mobile-relay-selfbuilt.md)。

## Decision

**不推翻「全走 relay」这条主路径。** 它换来的「扫码即绑定、跨网络可用、零输入」是对方生态拿不到的 UX，
代价（relay 单点、二维码含可用凭据）用**工程手段**对冲，而不是退回让用户比对指纹。

落地动作收敛为 5 个阶段、21 个改造点，按「安全止血 → 可恢复 → 连接韧性 → 多路径与推送 → 跨平台」推进。
前三阶段是必做（有已知的 P0/P1 缺陷与真实的体验断点），后两阶段按资源取舍。

一句话优先级：**先把「断线即丢结果」和「任意网页可操控本机 API」这两件事解决掉，其余的都是加分项。**

## 实施进度

**P0 批次已落地（2026-10-18）**：改造点 1–6、8。

| # | 落地位置 | 回归测试 |
|---|---|---|
| 1 CORS 默认收紧 | `lib.rs::http_cors_layer` + `is_local_origin` | `local_origins_are_recognised` / `foreign_origins_are_rejected` |
| 2 代理证据不判 Local | `security.rs::has_forwarded_evidence` + `request_guard`（`HERMIX_TRUST_FORWARDED=1` 可显式信任） | `forwarded_headers_are_recognised` / `loopback_without_forwarding_stays_local` |
| 3 限流默认有限 | `security.rs::DEFAULT_RATE_LIMIT_PER_MINUTE = 600` | `default_rate_limit_is_finite` / `limiter_blocks_after_quota` |
| 4 未配对 403 计入失败预算 | `security.rs::request_guard` Unpaired 分支 | 复用 `lockout_*` 组 |
| 5 缺盖章哈希整帧拒收 | `pairing.rs::pairing_bucket` | `pairing_bucket_requires_a_real_stamped_hash` / `distinct_buckets_do_not_share_budget` |
| 6 key.exchange 身份交叉校验 | `relay_transport.rs::dispatch` | `key_exchange_cannot_claim_another_device_id` / `..._for_non_device_session` |
| 8 出站按 `eid` 去重 | `apps/hm/core/replay.ts` + `session.ts` | `apps/hm/test/replay.test.ts`（5 例） |

**改造点 7（`node_id` 32 → 64 bit）暂缓**：改 `node_id` 会让已部署节点在 relay 重新登记，
且 `relay_phones.json` 绑定表里记的 `node_id` 全部失效——属于**兼容性破坏**，
需要配套迁移（旧 id 别名期 + 绑定表重写）才能动。当前风险由 relay 侧 `attachNode`
拒绝挤占来兜，不构成可用性缺口，排到 P2 与版本窗口一起做。

**P1 批次已落地（2026-10-18）**：改造点 9、10、11，以及 13 的裁剪版。

| # | 落地位置 | 回归测试 |
|---|---|---|
| 9 会话日志 | 新模块 `session_journal.rs`（环形 512 条 / seq 单调 / `truncated` 显式缺口） | 7 例单测 |
| 10 `Sync` 帧 | **不走新 `FrameTag`**：明文内层 `{"type":"sync","since_seq":N}`，回 `{"type":"sync.result",events[],truncated}`。理由见下 | — |
| 11 手机 hydrate | `apps/hm/core/syncOrder.ts` + `session.ts`（`onOpen` 即请求，实时帧排队防竞态） | 7 例（`test/syncOrder.test.ts`） |
| 13 端到端 | 在 `SessionMux` 接缝测「断线 → 重建 mux → 补齐」，不开 socket | `reconnect_replays_events_missed_while_offline` 等 3 例 |

**两处与原计划不同，都是审计后改的**：

1. **不加 `FrameTag::Sync`**。补发是「一条会话的内部事务」，用内层明文字段就够；
   新增 tag 要改 relay 的路由白名单（`apps/relay/src/proto.js`）并升协议版本，
   而 relay 本来就不该理解这个语义。
2. **补发存的是映射后的明文事件，不是重封密文**。手机冷启动会换新的 P-256 静态
   密钥，重封旧密文必须换 `eid` 重做 ECDH；而一次性封成 batch 只带一份
   nonce/一次性公钥，还天然避开手机侧按 `eid` 去重的窗口。

**13 的裁剪说明**：没起真 relay + 真 socket，用的是「重建 `SessionMux` + 同一份日志」。
真实重连就是 mux 被丢掉重建，而这正是要验的不变量；开 socket 会让测试变慢且
引入端口/时序 flake，收益只有「顺带测了 relay 的 Node 实现」——那归 `apps/relay`
自己的 31KB 测试套。

**未动**：12（`ws.rs:67` 的 `spawn_broadcast_forwarder` 死注释——该文件当时有在途改动，
留待下一批）、14–21（P2 及以后）。

**P2 第一批已落地（2026-10-18）**：改造点 14、15。

| # | 落地位置 | 回归测试 |
|---|---|---|
| 14 退避抖动 | `relay_client::RelayConfig::jittered`（0.75–1.0，**只向下**，不顶穿上限） | `退避抖动落在四分之一区间内` |
| 15 网络变化探测 | 新模块 `net_watch.rs`（`NotifyAddrChange` + `OVERLAPPED`/事件），`connect_and_serve` 的退避改为可打断 | `网络变化打断退避立即重连` |

**两处判断修正**：

1. **抖动只向下不向上**。原方案写 ±25%，但向上会突破 `max_backoff_secs`——那个上限
   的意义是「relay 恢复后能接上」，不该被抖动顶穿。向下 0.75–1.0 同样能打散惊群
   （60s 档散布在 45–60s，15 秒窗口足够），且语义干净。
2. **平台 API 放在 `hermix-server` 而不是桌面壳**（推翻本文 5.3.3 的原意见）。
   跨进程传「网络变了」要在 stdio 协议上加消息类型、加分发、加失败路径，比一个
   `#[cfg(windows)]` 的后台线程复杂得多；而 `hermix-server` 早已硬绑 Windows
   （DPAPI 在非 Windows 是编译错误），这里不多欠一笔新债。非 Windows 编译成空实现。

**未做（下一步）**：16（休眠唤醒）——`WM_POWERBROADCAST` 要窗口与消息循环，
`RegisterSuspendResumeNotification` 要维护回调的 FFI 生命周期；且唤醒时网络栈重新绑定
通常也触发地址变化，能被 15 覆盖大半。17（`keep_awake` 联动）、18（N-1 版本窗口）同理待办。



---

## 一、调研对象与证据分级

| 级别 | 含义 | 本文怎么用 |
|---|---|---|
| `[代码]` | 本仓源码实证，已逐一打开文件核对 | 直接作为结论 |
| `[一手]` | 项目 README / 官方文档原文 | 直接引用，标注来源 |
| `[二手]` | 聚合站 / 博客转述 | 只用于指出方向，**动工前必须回源复核** |

**一个必须先说清楚的事实**：DeepSeek 官方 Harness（`deepseek-ai/deepseek-harness`，MIT，2026-08-13 开源，
developer preview，基于 Cordis 微内核「一切皆插件」）的 README **通篇没有 mobile / phone / remote / pairing**
这类字样——desktop 只在 Make 目标描述里出现一次 `[一手]`。

也就是说：**DSH 本体不提供桌面端与手机端的连接配对，这件事被「一切皆插件」的架构甩给了社区。**
所以本文对标的对象实际是**三个互不兼容的社区实现流派**，而不是一个统一方案。这既是差距来源，也是机会
（没有既成标准要兼容，我们可以直接抄各家最好的那部分）。

## 二、外部怎么做

### 2.1 DSH 生态的三个流派

| 流派 | 代表项目 | 拓扑 | 关键设计 |
|---|---|---|---|
| **A. 局域网直连** | `zsyu9779/dsh-desktop`(Wails/Go)、`saya-ch/dsh-mobile` | 同 Wi-Fi 直连，无云端 | 二维码带**一次性配对码**，成功后写 **HttpOnly 设备凭据**；**自签名 HTTPS + 证书指纹**供肉眼核对；关闭远程即停代理并清除未用码，已配对设备记录保留 `[一手]` |
| **B. 公网隧道** | `AercherC/dsh-remote`(MIT fork) | Gateway 只听 `127.0.0.1`，cloudflared 只出站 | 8 位一次性码（默认 5 分钟）；**长期配对码**（默认 9 位随机，可自定义 6–12 位 A-Z0-9，重启后仍有效）；每设备独立 Device Session，可单独/全部撤销；claim 限速 + **timing-safe** 凭据比较；**秘密放 URL fragment 不走 query**；隧道进程退出即清公网入口；自动下载的 cloudflared 用固定版本 + SHA-256 + Windows 另验 Authenticode `[一手]` |
| **C. 角色化网关** | `KyoMio/dsh-zen-remote` | 一个插件两种角色 `host`/`client`，内含 relay server | 配对码**按设备角色铸造，兑换时校验角色**——拿错渠道的码被拒、**不消耗、也不计入错码锁定**；8 位 / 10 分钟 / 一次性；**Web 应用端（手机/PWA）看全部会话，桌面客户端只看已共享的会话（服务端强制）**；**手机 scope 默认拒绝 + 白名单，fail closed**；管理动作（出码/改角色/撤销）**只能在本机设置页执行，经网关进来的变更一律拒绝** `[一手]` |

补充细节（来自 C 流派 README 与 Go 侧提交说明 `[一手]`）：

- 会话远程开关是**按会话**的，有 4 个入口（行内「…」菜单 / 标题栏地球图标 / 设置页列表 / 手机端会话信息卡）。
- **闲置 48 小时自动关闭**（`idleHours`，取值 (0, 8760]）；**正在跑、等授权、等回答的会话永不被清**；**仅仅开着页面看不算活跃**。
- 断连时页面「保持不动」，链路恢复后**自动拉新快照自愈**（例外：文件树与终端面板必须重开）。
- 推送：VAPID + **aes128gcm 端到端加密**；默认**只在等你授权或等你回答时推**，普通回合结束不推；
  审批有 `DSH_PUSH_APPROVAL_GRACE_MS`（默认 5000ms）宽限，让更快的自动应答者先赢；
  `pushSummary` 才带最终回复摘要（prose only，**裁剪 120 字符**，绝不带上 reasoning）。
- 反向代理自检：**用手机流量**（不是家里 Wi-Fi）打开 `https://your-domain/lan-gate/admin`，**正确结果是 403**；
  能看到说明页 = 代理没转发 `X-Forwarded-*`，公网请求被当成了本地请求。
- 密钥：Account identity 迁移到 **X25519**（旧 Ed25519 identity 保留 Host ID 走 server migration API）；
  relay 握手用静态 ECDH + HKDF-SHA256 + AES-256-GCM；每连接密钥，**新连接重新握手且不复用旧密钥**。

### 2.2 参考架构：OpenClaw（Gateway + Nodes）

选它的理由：**它是三者里架构最清晰、文档最硬的一份**，而且它把「配对」拆成了两层——这个拆法正好能补我们的短板。

- **拓扑**：Gateway 是唯一控制面（拥有 sessions / routing / channels / tools / events）；手机是 **node**（`role: "node"`），
  通过 **WebSocket 18789** 连 Gateway；局域网用 **mDNS/Bonjour 发现**，远程走 **Tailscale + wss://** `[一手]`。
- **两层配对**（都落在 SQLite 的 paired device record 上）`[一手]`：
  1. **Device pairing**（role `node`）把守 **connect 握手**；
  2. **Node capability approval**（`node.pair.*`）把守**能暴露哪些命令**。两个 requestId 互不通用。
- **待批准期的重连**：headless node 持续重连，**指数退避上限 30 秒**；pending 请求在**最后一次重试后 5 分钟**过期——
  持续重连的设备保持**同一个** requestId，不会每隔几分钟弹一次新提示 `[一手]`。
- **换凭据即作废**：如果 node 重试时换了 role / scopes / public key，旧 pending 请求被 supersede，生成新 requestId，
  客户端收到 `device.pair.resolved` `[一手]`。
- **审批分级**（按声明的命令）`[一手]`：
  - 无命令 → `operator.pairing`
  - 普通命令 → `+operator.write`
  - 含 `system.run` / `system.which` / `browser.proxy` / `fs.listDir` / `execApprovals.get|set` → `+operator.admin`
- **批准记录的是「上限」不是「当前」**：实时命令必须**同时**满足「已声明 + 已批准 + 通过 gateway 命令策略」`[一手]`。
- **SSH-verified 自动批准**：Gateway 反向 SSH 回配对主机跑 `openclaw node identity --json`，
  **只有 device id 和 public key 都精确匹配才批**。明文写着「reachability alone never approves」——
  NAT 同租户、共享主机上的其他用户、局域网伪造，全部穿透不了 `[一手]`。
- **无全局放行模式**：没有「LAN 或私网一律自动批准」的开关，trusted CIDR 必须显式列 `[一手]`。
- **版本窗口 N-1**：v4 Gateway 接受 v3 node（当且仅当声明 `role:"node"` + `client.mode:"node"`）；
  升级顺序 = 先 Gateway 后 node；老于 N-1 的 node 必须带外升级 `[一手]`。

## 三、我方现状（代码级事实）

### 3.1 拓扑

```
手机 apps/hm (uni-app)  ──WSS /relay/phone──┐
                                           ├── 自建 relay apps/relay (Node)
PC hermix-server ──WSS /relay/node(出站)───┘   默认 wss://hermix.cssso.com
                                                只按 sha256(token) 路由，字节级透传
```

`[代码]` relay 端：`apps/relay/src/auth.js:14-22`（凭据只存哈希）、`router.js:199-200`（**无条件覆盖 `envelope.token_hash`，手机自报无效**）、
`proto.js:69-73`（拒明文 route 帧）。PC 侧出站连接器：`relay_client.rs:7-9`；relay 可自建且完整实现在本仓 `apps/relay/` `[代码]`。

### 3.2 配对

| 项 | 值 | 证据 `[代码]` |
|---|---|---|
| 码长 | **6 位十进制**，≈19.9 bit | `pairing.rs:499` |
| TTL | **3600s** | `pairing.rs:56` |
| 一次性 | 是（`consumed` + `gc()`） | `pairing.rs:392`、`:490-493`、`:617` |
| 重新生成即作废 | 是 | `pairing.rs:558` |
| 二维码 | `{relay}/pair?c=<码>&t=<hrel_*>&k=<pk_pc>` | `pairing.rs:547-551` |
| 错误预算 | 5 次 / 600s，**按 relay 盖章的凭据哈希分桶** | `pairing.rs:61`、`:421`、`:728-731` |
| 设备凭据 | `hmxd_` + 32 字节随机（256 bit），库里只存 sha256 | `pairing.rs:172-173`、`:1184-1188` |
| 默认能力位 | `["chat"]` | `pairing.rs:107` |
| 存储 | `<hermix_home>/devices.db` | `pairing.rs:109-110`、`:137-153` |

### 3.3 认证与加密

- **握手**：出码时把 `pk_pc` 放进二维码 → 手机用 `pk_pc` 密封 `[device_id, pk_ph]` 发回 → PC 解密、学 `pk_ph`、写 `peers.json` → PC 判码后用 `pk_ph` 密封 `device_token` 回信 `[代码]`（`relay_crypto.rs:350-383`、`relay_transport.rs:539-565`、`pairing.rs:805-812`）。
- **算法**：P-256 ECDH + HKDF-SHA256（salt = 16B 随机 frame_id；info = `hermix/frame/v1/d2n` / `…/n2d`）+ AES-256-GCM（12B 随机 nonce）+ AAD `v={};tag={:?};sid={};nid={}` `[代码]`（`relay_crypto.rs:19-26`、`:62`、`:66-67`、`:152-159`、`:171-173`、`relay_proto.rs:115-125`）。
- **选型理由写在注释里**：P-256 而非 X25519，因为 App 侧只有 WebCrypto、没有 Curve25519 `[代码]`（`relay_crypto.rs:19-26`）。
- **前向保密**：每帧一个新 ephemeral，帧密钥不复用；**刻意不做 Double Ratchet**（单向控制流不是双向持续会话）`[代码]`（`relay_crypto.rs:25-26`）。
- **请求签名**：`X-Hermix-Signature: t=,n=,s=`，HMAC-SHA256(device_token, `METHOD\npath\nt\nnonce`)，±300s 窗口 + nonce 单次使用；**用信封里的 `session_id` 重建路径再验签，把签名绑死在会话上** `[代码]`（`request_signature.rs:6-22`、`:62-95`、`mobile_ws.rs:119-129`）。
- **顺序铁律**：鉴权先于 demux `[代码]`（`relay_client.rs:376-380`、`relay_transport.rs:18-22`）。
- **不降级三连**（这条写得好，必须保留）：未启用 E2EE 拒收密封帧 / 启用后拒收明文 / DPAPI 失败报错而非落明文 `[代码]`（`relay_transport.rs:504-514`、`:678-691`、`relay_identity.rs:17-20`）。

### 3.4 会话生命周期

| 层 | 持有者 | 生命周期 | 证据 `[代码]` |
|---|---|---|---|
| `SessionMux.entries` | `serve_relay_socket` 局部变量 | **每次 relay 重连即重建** | `relay_client.rs:307-314` |
| `sessions: HashMap<sid, JoinHandle>` | 同上局部变量 | 同上 | `relay_client.rs:314`、`:431-435` |
| `active_sessions` / `active_handles` | 全局 `HttpServerState` | 跨连接存活 | `state.rs:218`、`ws.rs:406` |

**移动端断开 ≠ 任务被杀**：`relay_client.rs:431-435` abort 的只是 `handle_ws_stream`；agent 任务在
`ws.rs:1636` 是**另一个独立 spawn**，会跑完并落库，事件写进 `broadcast(256)` 无人消费、溢出丢弃 `[代码]`。

### 3.5 重连与网络切换

| 项 | 值 | 证据 `[代码]` |
|---|---|---|
| PC→relay Ping | 30s | `relay_client.rs:322` |
| 半开判定 | 90s 无入帧 | `relay_client.rs:323`、`:336-342` |
| PC 退避 | `2·2^min(n,16)`，上限 60s，**无抖动** | `relay_client.rs:56-60` |
| 手机退避 | `2000·2^min(n,16)`，上限 30s，无抖动 | `apps/hm/core/relayClient.ts:41-44` |
| 发送重试 | 3 × 50ms，耗尽后 90s 宽限再软杀 | `ws.rs:242`、`:247-248`、`:2037-2049` |
| 网络变化感知 | **零**（无 `NotifyAddrChange` / `NetworkChange` / `WM_POWERBROADCAST`） | 全仓 grep 0 命中 |
| 防休眠 | 有实现，但只服务「自动化页开关」，手机会话不自动触发 | `keep_awake.rs:3`、`:11-13` |

最坏恢复时间 ≈ **90s 静默判定 + 60s 退避 = 150s**。

### 3.6 前端与安全面

- 前端**不是只有 API**：`ImChannelsCard.tsx`（1359 行）有完整二维码 SVG、6 位码展示、2s×300 轮询落地、撤销、换绑、relay 配置、重置身份；另有 `DevicesTab.tsx`、`DiagnosticsTab.tsx`（审计日志）`[代码]`。
- **问题面**：`lib.rs:244-246` 未配 `HERMIX_HTTP_CORS_ORIGINS` 时是 `CorsLayer::permissive()`；`security.rs:283-296` 的 loopback 判定**只看 `ip.is_loopback()`，不校验证据头**；全局限流默认 0（`security.rs:139-141` + `:211-213` 恒放行）`[代码]`。
- **缓解事实**：桌面端以 `--stdio` 运行（`commands.rs:390-420`、`stdio_bridge.rs:35`），HTTP/TLS 监听器在生产桌面形态下**不生效**。所以上述问题是**HTTP/ Dashboard 模式下的 P0，桌面 stdio 形态下不生效** `[代码]`。

## 四、逐项对比：差距与根因

### 4.1 设备发现

| | 外部 | Hermix | 差距 |
|---|---|---|---|
| 局域网发现 | mDNS/Bonjour（OpenClaw `[一手]`） | **无**，`.local` 只在 SSRF 黑名单里出现 `[代码]` | 中大 |
| 远程发现 | Tailscale `wss://` 端点（OpenClaw `[一手]`）；或固定 relay 域名 | relay 地址靠**手工配置**（`relay_settings.rs:472-535`） | 中 |
| 首次入网 | 扫码（二维码里带地址+凭据） | 扫码（二维码里带地址+凭据） | **持平，且我方这一步是对的** |

**根因**：2026-10-10 用户拍板「全走 relay」，局域网发现随直连一起退役（`ImChannelsCard.tsx:626` 明写「粘贴链接通道随局域网直连一起退役了」）。
**注意**：退役的是**直连传输**，不是**发现能力**。mDNS 只用来回答「这台 PC 在同网段的 LAN 地址是多少」，
不强制走 LAN——这两件事被一起砍掉了，属于误伤。

### 4.2 配对协议

| | 外部 | Hermix | 差距 |
|---|---|---|---|
| 码长/熵 | 8 字符（≈40 bit）`[一手]` | 6 位数字（≈19.9 bit）`[代码]` | 中大 |
| TTL | 5 分钟（dsh-remote）/ 10 分钟（zen-remote）`[一手]` | **60 分钟** `[代码]` | 大 |
| 一次性 | 是 | 是 | 持平 |
| 角色绑定 | **铸造时定角色、兑换时校验，拿错渠道的码不被消耗也不计入锁定** `[一手]` | 无角色概念 | 中 |
| 吊销 | 单设备 / 全部，且**立即掐断连接与推送订阅** `[一手]` | 单设备 + relay 联动，三态提示 `[代码]` | 小 |
| 长期码 | 有（可自定义 6–12 位）`[一手]` | **无**（每次出码都新签 relay 凭据） | 中 |

**根因**：6 位码是抄 cc-haha 的 IM 配对码（`pairing.rs:55-61` 注释自述）。信任锚后来从「本地 TLS 指纹」
换成了「二维码里的 `hrel_*` 凭据」（`2026-10-10` 笔记），**但熵和 TTL 没有跟着重新定预算**。
60 分钟 TTL × 无限次尝试（全局限流默认关）的组合，是这条链路上最实在的一个在线爆破窗口。

**待复核**：`pairing.rs:728-731` 桶键缺失时退化为 `"relay-unstamped"`，即**所有来源共享一个桶**。
官方 relay 会无条件盖章（`router.js:199-200`）所以不触发；但**自建 relay 若不盖章，一人错 5 次会锁死所有人**。
这条需要在自建路径上实测确认。

### 4.3 认证与加密传输

| | 外部 | Hermix | 差距 |
|---|---|---|---|
| E2EE 套件 | Noise_IK_25519_ChaChaPoly_SHA256（ds-harness-remote `[二手]`）；静态 X25519 ECDH + HKDF-SHA256 + AES-256-GCM（zen-remote Go 侧 `[一手]`） | P-256 ECDH + HKDF-SHA256 + AES-256-GCM `[代码]` | 小（都在成熟套件内，我方选型有 WebCrypto 的硬约束，合理）|
| 前向保密 | 每连接密钥，新连接重新握手 `[一手]` | **每帧** ephemeral `[代码]` | **我方更强** |
| 帧级签名 | — | **无**（E2EE 层只有 AEAD） `[代码]` | 中 |
| 请求级签名 | — | 有：HMAC + 时间窗 + nonce，且**绑 session** `[代码]` | **我方更强** |
| 入站防重放 | — | `ReplayGuard`（eid 去重，600s / 256 每桶，解密成功后才登记）`[代码]` | **我方更强** |
| **出站防重放** | — | **无** `[代码]`（`relay_transport.rs:127`） | **大** |

**根因**（出站重放）：协议设计是「单向控制流」——只防了「指令被重投」（入站），没防「事件被重投」（出站）。
而 `relay_transport.rs:349-356` 的威胁模型注释**自己承认 relay 运营方是潜在攻击者**。
也就是说：在自建 relay 场景下，运营方可以重投一条旧的 `approval.request` 或 `turn.done`，手机端会重复渲染、
审批按钮可以重复点。这是「注释写了威胁模型但实现没闭环」的典型。

### 4.4 会话生命周期

| | 外部（zen-remote `[一手]`） | Hermix | 差距 |
|---|---|---|---|
| 断连后自愈 | 页面保持不动，链路恢复后**自动拉新快照**，无需手动刷新 | **只发 `Connected` + `voice.stt_config` + TodoSnapshot hydrate，不发消息历史** `[代码]`（`ws.rs:2664-2794`） | **极大** |
| 闲置回收 | `idleHours=48`，跑着/等授权/等回答的永不清 | 无 | 中 |
| 断连时 agent | — | 继续跑成孤儿（见 3.4） | 中 |
| 可见性闸门 | 按会话共享开关，**服务端强制过滤**（列表/历史/实时进度/审批事件） | 全局 `["chat"]` 能力位，无按会话闸门 | 中 |

**根因**（最关键的一条）：`GET /v1/sessions/{id}/subscribe?since_seq=` 这个快照能力建在 **HTTP 面**
（`routes.rs:995-1046`、`lib.rs:408-411`），而手机走的是 **relay 帧通道**——relay 只透传 WS 帧、
**不做 HTTP 中转**。于是「重连补历史」这件事在协议上根本走不通。
`ws.rs:23-26` 明确写着「订阅前的事件不重放」，`ws.rs:67` 挂着一条 `spawn_broadcast_forwarder`
**从未实现**的死注释。这是**分层断裂**，不是漏写代码。

**后果**：手机切后台 / 切 Wi-Fi / 进出地铁 → 这一轮的结果手机上**永久看不到**。
用户只能等回到电脑前。这条直接抵消了「远程控制台」这个产品定位的一半价值。

### 4.5 网络切换与断线重连

| | 外部 | Hermix | 差距 |
|---|---|---|---|
| 心跳 | OpenClaw 走 WS 标准帧；zen-remote 有 tunnel edge 持续监测 `[一手]` | 30s Ping + 90s 半开判定 `[代码]` | 小 |
| 退避 | 指数退避**上限 30s**（OpenClaw `[一手]`） | 60s，**无抖动** `[代码]` | 中 |
| 惊群 | — | 纯指数无 jitter，relay 重启后所有 PC 同步撞 | 中 |
| 网络变化感知 | cloudflared 自动恢复 + 保留公网地址 `[一手]` | **零**，靠 90s 静默兜底 | 大 |
| 休眠 | 需要用户手动开「允许锁屏远程」`[二手]` | `keep_awake.rs` 只服务自动化页，手机会话不联动 | 中 |

**根因**：只做了「半开检测」（被动等超时），没做「链路变化主动探测」。
Windows 上有现成的 `NotifyAddrChange` / `NotifyNetworkChange`（IP Helper）和 `WM_POWERBROADCAST`，
但全仓 0 命中——历史上一直接的是「重连足够快」这个假设，在切 Wi-Fi / 唤醒这种场景下不成立。

### 4.6 跨平台兼容性

| | 外部 | Hermix | 差距 |
|---|---|---|---|
| 桌面端 | Wails/Go 跨平台（macOS Universal + Windows）`[一手]` | **Windows-only**（AGENTS.md 明写 MSVC） | 结构性差异（当前不算缺陷）|
| 手机端 | iOS/Android 原生（Swift/Kotlin，Agents Anywhere `[二手]`）、PWA、Android APK | uni-app（一套代码多端）`[代码]` | 小，我方成本更低 |
| 私钥保护 | 随平台 | **DPAPI 硬耦合**，非 Windows 直接编译错误 `[代码]`（`relay_identity.rs:305-313`） | 中（是未来跨平台的结构性障碍）|

**根因**：项目从第一天就是 Windows-only，DPAPI 是最省事的选择。当前不是 bug，
但只要「跨平台」还在 roadmap 上，私钥保护的抽象层迟早要补——越晚补越疼（因为 `identity.json` 已落盘）。

## 五、优化方案

### 5.1 架构调整

**调整 1：把「传输」从「单一 relay」抽成 `LinkPath` 多路选路**

```
                    ┌── LinkPath::Lan   ── 直连（mDNS 发现 + 自签 TLS + 指纹带外校验）
SessionMux ─ Link ──┤
                    └── LinkPath::Relay ── 现有出站 WSS（兜底，永远可用）
```

选路策略：**LAN 优先，relay 兜底**（对齐 ds-harness-remote 的 LAN→P2P→TURN→Relay 顺序，
但只做 LAN→Relay 两级——P2P/TURN 的收益撑不起复杂度，见 Alternatives）。

关键点：**LAN 直连默认关闭，由 mDNS 发现自动开启**。这同时解决了 2026-10-10 笔记里
否决直连的**全部三个理由**：

| 原否决理由 | mDNS 方案下 |
|---|---|
| 要求同网 | 变成自动检测，不用用户判断 |
| 暴露本地 TLS 端口 | 只在同网设备存在时按需监听，且可绑定单网卡 |
| 肉眼比对 64 位指纹 | 指纹通过 mDNS TXT 分发 + 二维码带外二次校验，不用肉眼看 |

**这条是「推翻」还是「继承」？** 是继承——主路径仍然是 relay（跨网络、零配置），
LAN 只是**同网时的加速与逃生通道**，用户完全无感。

**调整 2：把快照能力下沉到帧协议**

新增帧类型，让「重连补历史」在 relay 通道上走得通：

```rust
// crates/hermix-server/src/relay_proto.rs
pub enum FrameTag {
    // ... 现有 5 个 tag
    Sync,  // 新增：重连增量补发
}

pub struct SyncRequest  { pub sid: String, pub since_seq: u64 }
pub struct SyncResponse { pub sid: String, pub from_seq: u64,
                          pub events: Vec<JournalEvent>, pub truncated: bool }
```

`SyncResponse.truncated = true` 表示环形缓冲已被覆盖，UI 应提示「部分历史已丢失，建议回电脑查看」——
**明确告知，不要假装完整**。

**调整 3：会话生命周期与连接生命周期解耦**

现在 `SessionMux` / `sessions` 是 `serve_relay_socket` 的**局部变量**，relay 一重连就全没了。
改成挂在全局 `HttpServerState` 上，relay 连接只是「挂载/卸载」：

```
HttpServerState.session_links: HashMap<sid, SessionLink>
  SessionLink { inbound: Sender<InboundFrame>, journal: Arc<SessionJournal>, agent: Option<JoinHandle> }
```

这样：relay 断了 → 只摘掉 transport，journal 继续记，agent 继续跑 → 重连 → 按 `since_seq` 补发。

**调整 4：能力面治理从「一次性授予」变「声明 + 批准」两层**

照 OpenClaw 的拆法，但**保持我方的默认最小集**：

- 第 1 层「设备准入」= 现有配对，产出 `hmxd_*`（不动）
- 第 2 层「能力面」= 新增，按敏感性分级：
  - `chat`（默认给）
  - `read.files`（需桌面确认）
  - `approve.dangerous`（需桌面确认，**且不持久化**——每次会话重新确认）

同时补上 `DEFAULT_DEVICE_SCOPES` 之外的**按会话可见性闸门**：桌面端逐会话决定「这个会话允许手机看吗」，
由 **PC 侧过滤**（不是手机侧隐藏）。

### 5.2 协议 / 流程改造点

| # | 阶段 | 改什么 | 怎么改 | 落点 |
|---|---|---|---|---|
| 1 | P0 | CORS 默认 | 未配 env 时改为同源 + localhost 白名单，不再 `permissive()` | `lib.rs:244-246` |
| 2 | P0 | loopback 判定 | 有 `Forwarded` / `X-Forwarded-*` / `X-Real-IP` 证据时不认作 Local | `security.rs:283-296` |
| 3 | P0 | 全局限流默认 | `unwrap_or(0)` → 具体默认值；`per_minute==0` 的语义改为「显式配置才放行」 | `security.rs:139-141`、`:211-213` |
| 4 | P0 | 未配对 403 计费 | Unpaired 403 也计入 `AuthFailLockout` | `security.rs:620` |
| 5 | P0 | 桶键兜底 | 缺失时**拒绝服务**，不再退化成全局共享桶 | `pairing.rs:728-731` |
| 6 | P0 | `key.exchange` 身份交叉校验 | 明文载荷里的 `device_id` 必须等于 `session_id` 里的那一段 | `relay_transport.rs:544-553` |
| 7 | P0 | `node_id` 熵 | 4 字节 → 至少 8 字节（两端同改）**暂缓，见实施进度** | `relay_settings.rs:187-200`、`apps/relay/src/auth.js:181` |
| 8 | P0 | **出站防重放** | 手机侧按信封 `eid` 去重（滑动窗口 + TTL），对齐 PC 侧 `ReplayGuard` | `apps/hm/core/replay.ts`、`apps/hm/core/session.ts` |
| 9 | P1 | 会话日志 | `SessionJournal`：有界环形缓冲（默认 512 事件 / sid）+ 单调 seq | 新模块 |
| 10 | P1 | 补发协议 | `Sync` 帧：手机重连带 `since_seq` → PC 补发 | `relay_proto.rs` |
| 11 | P1 | 手机 hydrate | 重连后先发 `Sync` 再发 `Connected` | `ws.rs:2664-2794`、`apps/hm` |
| 12 | P1 | 死注释 | 实现 `spawn_broadcast_forwarder` 或删掉注释 | `ws.rs:67` |
| 13 | P1 | 端到端测试 | 起真 relay + 假手机客户端，跑通「出码→扫码→绑定→常驻→断线→重连→补历史」 | `crates/hermix-server/tests/` |
| 14 | P2 | 退避抖动 | 加 ±30% jitter；PC 上限对齐 30s | `relay_client.rs:56-60` |
| 15 | P2 | 网络变化探测 | `NotifyAddrChange` 后台线程 → 触发健康检查/立即重连 | 新模块 |
| 16 | P2 | 休眠唤醒 | 监听 `WM_POWERBROADCAST` resume → 立即探活 | 桌面壳 |
| 17 | P2 | 防休眠联动 | 手机会话在线时自动 `set_keep_awake(true)` | `keep_awake.rs:44-49` |
| 18 | P2 | 版本窗口 | `version_ok()` 支持 N-1：老版本可连但禁用新能力 + UI 标「版本有差异」 | `relay_proto.rs:164-166` |
| 19 | P3 | LAN 直连 | 按需监听 + rcgen 自签 + 指纹带外校验 | `tls.rs:96-137` |
| 20 | P3 | mDNS 发现 | `_hermix._tcp.local`，TXT 带 `nid`/`v`/`pkfp` | 新模块 |
| 21 | P4 | 私钥保护抽象 | `trait KeyProvider { fn protect/unprotect }`，Windows 走 DPAPI | `relay_identity.rs:224-313` |

**协议版本处理原则（第 18 项，重点说明）**：
「不降级」这条铁律**只保留在密码学与授权判定路径上**——那条谁都不许动。
但**能力协商**要做 N-1 窗口：老手机连新 PC，允许连、允许看历史、允许发消息，
只是新能力（比如新的审批卡片）不渲染，并在 UI 上明说「版本有差异，建议更新」。
否则一次 PC 升级 = 所有老手机失联，这个代价比「少渲染一个卡片」大得多。

### 5.3 关键模块实现细节

#### 5.3.1 出站防重放：用 `eid`，不加序号（改造点 8）

原计划加一条单调 `seq`。审计后放弃，理由是 `eid` 已经是这条链路上稳定的帧身份：

- 发送方每帧随机生成 16 字节 `eid`，同时进 HKDF salt（协议本来就把它当"帧唯一性"用）；
- relay **没有密钥**，重投只能是字节级复制 → `eid` 必然相同；
- 新增 `seq` 反而要处理"重连后计数器归零"的 epoch 问题，还得升协议版本。

```ts
// apps/hm/core/replay.ts
export class ReplayWindow {
  constructor(cap = 512, ttlMs = 600_000, now = () => Date.now())
  /** 首次出现的 eid → true 并登记；重复/空串 → false。必须在解密前调用。 */
  accept(eid: string): boolean
}
```

窗口有界（512 条 / 10 分钟）：长连设备上无界集合就是内存泄漏。

#### 5.3.2 `SessionJournal`（改造点 9）

```rust
// 新模块 crates/hermix-server/src/session_journal.rs
pub struct SessionJournal {
    events: VecDeque<(u64, JournalEvent)>,   // (seq, event)
    cap: usize,                              // 默认 512
    next: u64,
}

impl SessionJournal {
    pub fn push(&mut self, e: JournalEvent) -> u64;
    /// since_seq 之后的所有事件；若缓冲已被覆盖，返回 truncated=true
    pub fn since(&self, since_seq: u64) -> (Vec<(u64, JournalEvent)>, bool);
}
```

挂载点：`ws.rs` 里所有走 `broadcast` / 出站 sink 的事件，**同时**写 journal。
journal 是**内存**的（不落盘），PC 进程重启即清空——这符合「临时补偿」的定位，
也不引入新的持久化隐私面。

#### 3.3.3 网络变化探测（改造点 15）

```rust
// 新模块：NotifyAddrChange 后台监听
// 拿到通知后不直接重连，先做一次「探活」：
//   - relay 连接最近 5s 内有入帧 → 忽略（可能是 DHCP 续租噪声）
//   - 否则 → 立即 close + 重连（把最坏 150s 压到秒级）
```

Windows 上 `NotifyAddrChange` 会阻塞，需要独立线程 + `CancelIPChangeNotify` 退出路径；
`notifyroutechange` 同理。这块要放在 **桌面壳（Tauri 侧）**，不要在 `hermix-server` 里引
平台 API——`hermix-server` 已经够 Windows 耦合了，别再加。

#### 5.3.4 能力面两阶段（改造点，「调整 4」）

```rust
// pairing.rs 增
pub enum CapabilityTier { Auto, NeedsDesktopConfirmation }
pub fn tier_of(c: Capability) -> CapabilityTier;

// 新增待批准表（复用 audit 同库）
CREATE TABLE IF NOT EXISTS pending_capability_grants (
    device_id TEXT NOT NULL, capability TEXT NOT NULL,
    requested_at INTEGER NOT NULL, decided_at INTEGER, decision TEXT,
    PRIMARY KEY (device_id, capability, requested_at));
```

`NeedsDesktopConfirmation` 的能力**不随设备持久化**，每次新连接重新弹桌面确认。

### 5.4 分阶段实施计划

| 阶段 | 周 | 内容 | 出口条件（缺一不进下一阶段）|
|---|---|---|---|
| **P0 安全止血** | 1–2 | 改造点 1–8 | `cargo test -p hermix-server --lib` 全绿；CORS/loopback/限流各有回归测试；手工验证「带 X-Forwarded-For 的本地请求不再被判 Local」|
| **P1 可恢复性** | 3–5 | 改造点 9–13 | **端到端集成测试跑通**：真 relay + 假手机，覆盖「绑定→常驻→kill 连接→重连→按 since_seq 补齐断连期间事件」；`ws.rs:67` 死注释处理掉 |
| **P2 连接韧性** | 5–8 | 改造点 14–18 | 切 Wi-Fi 恢复 < 5s（对比基线 150s）；休眠唤醒自动探活；`keep_awake` 联动开关；N-1 版本窗口有测试 |
| **P3 多路径与推送** | 8–12 | 改造点 19–20 + 推送通道 | 同网自动走 LAN（relay 不可用时仍在线）；审批请求在手机杀后台时能唤醒 |
| **P4 跨平台抽象** | — | 改造点 21 | `hermix-server` 在非 Windows 上能编译（哪怕功能降级）|

**并行关系**：P0 与 P1 可并行（改的文件基本不重叠）；P2 依赖 P1 的 journal 落地；P3 依赖 P0+P1；P4 独立，随时可做。

### 5.5 风险控制

| 风险 | 触发场景 | 控制措施 |
|---|---|---|
| **协议双端不同步** | PC 升级、手机 App 未升级 | `PROTO_VERSION` N-1 窗口（改造点 18）；握手失败时手机端提示「请更新 App」而非静默失败 |
| **出站 seq 导致旧手机收不到帧** | 老 App 不认 `seq` 字段 | `seq` 设为 `Option`，旧端忽略；**先发新字段、后强制校验**，中间留一个版本缓冲 |
| **journal 内存膨胀** | 会话多 / 事件大 | 每 sid 硬上限 512 事件 + 单事件大小上限；超限 oldest-first 淘汰并置 `truncated` |
| **LAN 监听引入新攻击面** | 恶意同网设备 | 默认关闭；仅 mDNS 发现到已配对设备时按需开；证书指纹必须与二维码里的不一致即拒 |
| **relay 仍单点** | 官方 relay 不可用 | P3 的 LAN 逃生通道 + relay 地址可配置 + 401 自愈路径已存在（`state.rs:1320-1337`）|
| **改动回归** | 动 `security.rs` / `relay_*` | 每阶段跑 `./scripts/ci-gate.sh`；P0/P1 各补专项回归测试 |

**回滚**：P0/P1/P2 各阶段独立可回滚（每个阶段一个语义 commit，按项目「一语义一 commit」纪律）。
P3 引入 LAN 路径，用特性开关 `HERMIX_LINK_LAN=0` 可整体关闭。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| **退回局域网直连为主路径** | 用户 2026-10-10 已拍板否决：要求同网 + 暴露本地端口 + 肉眼比指纹 = 把实现复杂度摊给用户。不重新讨论。 |
| **接 Tailscale 做直连** | 2026-10-04 笔记已记录弃因（用户拍板不做直连优先）。且引入第三方账号体系与常驻进程，与「便携零安装」冲突。 |
| **relay 侧做 HTTP 中转，让手机复用 `since_seq` 快照端点** | 破坏 relay「字节级透传、不解析不授权」的核心不变量，把 relay 从哑管道变成有语义的代理，攻击面显著扩大。不如把 Sync 做成帧类型。 |
| **把 E2EE 换成 Noise_IK / X25519** | App 侧只有 WebCrypto、无 Curve25519（`relay_crypto.rs:19-26` 写明）。收益（套件更时髦）撑不起重写密码学层 + 跨端重测的风险。 |
| **补 Double Ratchet** | `relay_crypto.rs:25-26` 已论证：单向控制流不是双向持续会话。且我方已是**每帧**前向保密，比每连接更强。 |
| **一次性把 P0–P4 全做** | 「配对→中继→会话」链**至今没有自动化端到端测试**（2026-10-10 笔记里「未做真机端到端」已连续两次列为 P0）。先补测试再动大结构，否则是盲改。 |
| **照抄 zen-remote 的「手机看全部会话」** | 与 Hermix 现有的按渠道/按 workspace scope 治理冲突（`2026-09-30-im-channel-workspace-scope.md`）。我方保留按会话闸门，更严。 |

## Consequences

**得到**：

- 断线不再丢结果（P1）——这条直接兑现「远程控制台」的产品承诺。
- HTTP 模式下不再有任意网页可操控本机 API 的口子（P0）。
- 切网/唤醒的恢复时间从 ~150s 压到秒级（P2）。
- 出站重放闭环，威胁模型注释与实现对齐（P0）。
- PC 升级不再让老手机失联（P2）。

**代价 / 放弃**：

- **多一个概念**：`LinkPath` 选路器。原来是「一条 relay 管道走天下」，现在多一层判断。
  缓解：LAN 路径默认关，只有 mDNS 命中时才启用，正常路径零分支。
- **内存**：每会话 512 事件的 journal。按 50 会话算约几十 MB 级，可接受；超了调 `cap`。
- **协议复杂度**：`Sync` 帧 + `seq` 字段 = 两个新东西要跨端对齐，且要跨端固定向量测试。
- **推送通道（P3）是真正的大工程**：要 VAPID 密钥管理、订阅生命周期、App 侧厂商通道对接。
  它的收益（手机杀后台也能收到审批）很大，但**不要和 P0/P1 抢资源**。
- **不解决 relay 单点**：P3 只是加了「同网时」的逃生通道；真正跨网时仍依赖 relay。
  彻底解决需要多 relay 故障转移，本次不做。

**待复核项（动工前必做）**：

1. `pairing.rs:728-731` 的 `"relay-unstamped"` 退化桶，在自建 relay 路径上实测是否可触发。
2. `security.rs:448-450` 的 `unpaired_gate_allows` 放行 `/api/pairing/complete` 与 `/pair`——
   这两条路由在 `lib.rs` 里**已不存在**，确认是死代码后随清理批次删除（2026-10-10 笔记已挂账）。
3. 外部对标里的 `[二手]` 项（zen-remote 的 Go 侧提交说明、Agents Anywhere 的连接机制）
   在方案评审前回 GitHub 源码复核一次。
