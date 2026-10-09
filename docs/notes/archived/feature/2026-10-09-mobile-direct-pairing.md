# Decision Note: 手机端配对改走局域网直连（先接通能用的那条）

Status: archived — 2026-10-10（被 2026-10-10-mobile-pairing-via-relay.md 取代）
Archived: 2026-10-10

## Decision

手机端配对从「`POST {relay}/api/pairing/complete`」改为**扫桌面端二维码、直连那台
电脑的 TLS 监听器**。本 PR 只动 `apps/hm/`，不碰 relay 与 Rust。

## Alternatives considered

### 为什么原来的 relay 路线现在走不通（证据）

1. `apps/hm/core/pairing.ts:129` 打 `POST {relay}/api/pairing/complete`，而
   `apps/relay/src/server.js` 全部路由只有 9 条（`:64`–`:211`），**没有这一条** →
   必然 404。
2. 设计文档 `2026-10-04-mobile-relay-selfbuilt.md:145` 规划过 `src/pairing.js`
   （配对码中转）与 `src/devices.js`（设备登记），`apps/relay/src/` 里**两个都没写**。
3. **补这条链路不是 relay 单端能做的事**：配对码的判定权在 PC 的
   `PairingSessions::consume`（`pairing.rs:589`），而 PC 只出站连 relay
   （`relay_client.rs:143 connect_and_serve` → `connect_async`）。relay 收到码后
   必须经 WS 转给 PC 验，验过才有资格签发 `hmxd_*`——relay 自己签的 token
   hermix 侧不认（`auth.js:6-9`：relay 的校验是**路由级**的，不是**授权级**的）。
4. 因此 relay 中转路线 = relay 加 `pairing.js`/`devices.js` + Rust 加 pair tag
   + hm 改握手，跨三端。

### 为什么直连这条路已经通了

PC 自己的 pairing 监听器（`HERMIX_TLS_ADDR`，默认 `0.0.0.0:8443`，
`tls::tls_addr_from_env`）**已经挂着** `POST /api/pairing/complete`，而
`POST /api/pairing/sessions` 返回的 `pairing_url` 就是
`https://{lan_ip}:{port}/pair?c={code}&fp={fingerprint}`（`pairing.rs:538`）。
桌面端设置页「手机端」渠道已经在发这个二维码，落地页的 JS 已经在调
`fetch("/api/pairing/complete")`（`pairing.rs:717`）。

**这条链路早就是通的，手机端只是还没用它。** 本 PR 让 App 做和落地页一样的事。

### 被否决的选项

| 方案 | 否决理由 |
|---|---|
| 先补 relay 中转（三端一起上） | 能解决跨网络，但跨三端、含 `PROTO_VERSION` 耦合；而局域网场景现在就是零成本可用。先接通能用的，跨网络留第二步。 |
| 保留 relay 码输入作为主路径 | 它必然 404。留着一个注定失败的入口，用户还是会撞同一堵墙。 |
| 直连 + relay 并列两个入口 | relay 那条还是死的；并列只是把「哪个能用」的选择推给用户。 |
| 允许 `http://` 的配对 URL | 配对依赖加密面，`PairingSessions::create` 在关掉 TLS 时直接返回 Err，根本发不出码。放行 http 等于给中间人开门。 |
| 手工输入 host + 码 + 指纹 | 三个字段、其中两个是 64 位 hex 指纹，肉眼抄不现实。扫是唯一合理的入口。 |

## Consequences

- **得到**：局域网配对**真的可用**了，且只动一个端。
- **得到（顺带）**：指纹来源从「relay 自己声明的 `x-hermix-cert-sha256`」变成
  **桌面端本机 TLS 层的真值**。前者是自证的（`pairing.ts` 文件头与 relay
  注释都承认），后者服务端会强制比对——`cert_fingerprint` 不符直接 403 拒发
  token（`pairing.rs:810-830`）。`core/crypto.ts` 里「二维码里带 `pk_pc`」的
  说法只对了一半，见下面的缺口。
- **得到**：配对屏从「relay 身份核对 + 6 位码」两件事收窄成「扫码 + 核对指纹」，
  职责单一。
- **⚠️ 发现一个既有设计缺口**：`core/crypto.ts` 注释称「手机扫码已经拿到
  `pk_pc`（二维码里带的）」，**但二维码 URL 里没有它**——`pairing_url` 只有
  `c` 与 `fp` 两个参数，`node_public_key` 只存在于 `QrPayload` 的 JSON 里，
  `/pair` 落地页的 URL 没带、页面也没传给前端。所以直连这条路拿不到 PC 的
  E2EE 公钥，必须等 WS 建连时用 `key.exchange` 帧协商。
  `storePairedDevice` 因此**显式不落盘** PC 公钥（宁可不存：存空值会让
  `pcPublicKey()` 看起来有值，后续 seal 时才炸，比现在明确地缺更难查）。
- **仍未解决（第二步）**：
  - 跨网络配对（relay 中转，跨三端）；
  - WS 握手缺 relay token。`apps/hm/core/relayClient.ts:108` 是
    `new WebSocket(url)`，不带任何头，而 relay `server.js:247` 无 Bearer 直接
    401、`auth.js:130` 只认 `Authorization`（`?token=` 通道已被刻意删除）。
    **注意：`hmxd_*` 与 `hrel_*` 是两套凭据**——前者是 hermix 的设备 token，
    后者是 relay 的参与者 token，签发方不同、存储位置也不同（`KEY_DEVICE_TOKEN`
    vs `KEY_RELAY_TOKEN`，后者目前**没有任何地方写入**）。hm 端改成
    `uni.connectSocket({ header })` 即可带头；**只有 H5 目标做不到**，那时得改用
    短时票据而非 token 直传。
  - `PROTO_VERSION` 需要 bump，但理由不是「加了 tag 必须 bump」：
    `relay_client.rs:185` 是 `from_str(raw).ok()?`，未知 tag 只会**静默丢弃**。
    bump 的价值是把「静默丢弃 → 手机永远等下去」换成「握手阶段就报版本不一致」。
- **权限**：`manifest.json` 增加 `android.permission.CAMERA`（`uni.scanCode`），
  iOS `NSCameraUsageDescription` 改为「扫描桌面上显示的配对二维码」，
  `NSLocalNetworkUsageDescription` 改为「连接同一网络下的电脑完成配对」。
- **回滚**：`git checkout -- apps/hm/`。不影响桌面端与 relay。

## 验证记录

- `npm test` —— 51/51（新增 `test/direct.test.ts` 9 例，覆盖真实 pairing_url
  格式、http 拒绝、路径拒绝、码位数、指纹格式、非 URL 输入、含更新包地址的干扰输入）。
  解析器的向量按 `pairing.rs:538` 的 format 串构造，两边不同步时这套用例会红。
- `npm run typecheck`（`vue-tsc --noEmit`）—— 无错误。
- `npm run build:h5` —— 通过。
- 浏览器实测（H5 构建 + 本地静态服务）：配对屏渲染「扫码连接」态与四角标记，
  设置页「配对方式 / 说明 / relay」三段正常。
- **未做真机验证**：`uni.scanCode` 与真实配对回环需要在手机上对着桌面端二维码
  跑一次，本次环境无法覆盖。这是本 PR 最需要你亲自确认的一步。
