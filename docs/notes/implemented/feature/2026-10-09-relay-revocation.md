# Decision Note: 撤销设备要真断到 relay（修「撤销按钮是摆设」）

Status: implemented — 2026-10-09

## Decision

`设置 → 设备 → 已配对设备 → 撤销` 此前**只撤了一半**：hermix 侧设备令牌失效
（手机发不出帧），但 relay 侧那条连接原封不动，手机仍在接收广播给该 node 的
全部出站帧。本次把撤销补成一条真正贯通的链路，同时修掉 relay 的无条件顶号。

### 之前的实际行为

```
桌面端点「撤销设备」
  → pairing.rs::revoke_device → registry.revoke(&device_id)   // 只动本机 devices.db
  → 全仓库 grep：没有任何一行代码通知 relay
```

手机**发不出**东西（这一层撤销确实生效），但**还在收**这台电脑产生的每一个
会话的帧。帧是密封的读不了内容，但 `session_id`、时间、流量大小全暴露，且只要
它一直连着就一直收。

这与 `apps/relay/src/auth.js` 自己写的判据直接冲突：

> 撤销过的凭据**直接拒绝**……必须立刻断掉，否则**撤销按钮就是个摆设**

按钮已经是摆设了。

## 三处改动

### 1. 只按**凭据哈希**撤销，hermx 全程不碰手机明文

手机配对时把 `sha256(hrel_*)` 报给 hermix（`CompletePairingBody` 新增可选字段
`relay_token_hash`），hermix 存在 `~/.hermix/relay_phones.json`，按 `device_id`
索引。撤销时凭哈希去 relay 换，**中间任何一环都没见过那台手机的明文凭据**。

哈希本来就在 relay 库里当主键，不是额外泄露。

放 JSON 而不是 `devices.db`：后者是 SQLite 且正被审计线改动，理由与
`relay_identity::PeerStore` 相同。

### 2. relay `POST /api/relay/revoke-phone`

只认 node 凭据（与 `issue-phone` 同一信任前提），**先撤销凭据再断连接**——
反过来的话连接在被踢之前还有一瞬间能用已撤销的凭据发东西。

`router.dropPhone(hash)` 立刻关闭连接并清路由表。

### 3. `attachNode` 只允许同一凭据顶号

原实现无条件替换，于是**任何持有该 node_id 的另一条凭据都能把真电脑踢下线**。
这不是理论风险：`node_id` 只有 32 位熵，而且明晃晃印在二维码的 `server_id`
与桌面端设置页里。

改成「槽位为空、或来的是同一凭据」才允许替换，别的一律 `conflict` 并以 4003
关闭。这条同时也是开放自助注册的前置条件——没有它，任何人都能凭空注册一台
电脑把受害者挤下线。

### 4. 撤销结果必须分两半报

`revoke_device` 现在返回 `{ local_revoked, relay_bound, relay_revoked, relay_error }`，
界面据此分别提示：

- 两边都断 → 「已撤销（含中转连接）」
- 只断了本地 → **明确说「这台设备可能仍在接收消息」**
- 没绑过中转 → 「该设备未连接中转」

**刻意不因 relay 失败而让整个请求失败**：本地撤销已经生效，报成失败会让用户
以为什么都没发生、于是不再重试——那比重试更糟。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 让 relay 收明文 `hrel_*` 再撤销 | hermix 就必须持有每台手机的明文凭据。只传哈希能完全避免，凭据只在签发那一刻存在于手机上。 |
| 把手机 token 加密存进 `devices.db` | 审计线正在改那张表；且没必要——存的是哈希，不是秘密。 |
| 撤销时让手机主动断开 | 手机可能已经不在跑、或者干脆被改坏了。**服务端必须能单方面断**，否则撤销依赖被撤销方配合。 |
| relay 失败就返回 5xx | 本地撤销已经生效，报失败会让用户以为什么都没发生而放弃重试。 |
| 让 relay 直接断开该 node 下**全部**手机连接 | 撤销的是**一台**设备。把同 node 的其他手机一起踢掉是副作用更大的行为，不该由一次单设备撤销触发。 |

## Consequences

- **得到**：「撤销」名副其实——断了就是断了，断了不彻底时界面会说出来。
- **得到**：顶号不再可能，开放自助签发的前提成立。
- **⚠️ 仍未解决（一台手机只能连一台电脑）**：手机凭据在 relay 侧绑定到单个
  `node_id`，路由在连接建立时固定；hm 端存储也是单设备（一组
  `deviceToken` / `relayToken` / `sessionId`）。每台桌面端有各自的
  `devices.db`，配一次拿一个 token，所以手机配了 A 电脑之后就**连不上 B 了**。
  这是结构性限制，要改是 relay + Rust + hm 跨三端的协议改动（手机身份与绑定
  分离，绑定表显式可查）。本次**没做**。
- **⚠️ 绑定记录缺失时撤销只断一半**：没配 relay 的手机（纯局域网直连）在
  `relay_phones.json` 里查不到，界面会显示「该设备未连接中转」——这是事实，
  不是失败。
- **回归面**：`attachNode` 签名从 `(nodeId, conn)` 变成 `(nodeId, tokenHash, conn)`，
  返回值从 bool 变成三态字符串。relay 内部与测试全部同步更新。
- **顺带发现**：`revoke-phone` 收到的请求若 `content-type` 不是
  `application/json`（`fetch` 字符串 body 的默认值是 `text/plain`），
  `req.body` 为空。若照旧报「哈希格式不对」会把人引向完全错误的方向，
  故单独给了「请求体为空或不是 JSON」的分支。
- **回滚**：`git checkout -- apps/relay crates/hermix-server apps/hm apps/desktop-tauri/src/components/settings/`。

## 验证记录

- `apps/relay` `npm test` —— 32/32 通过。新增 8 例：
  - 别的凭据不能顶掉在线的电脑（真连接对象原封不动）
  - 同一凭据重连可以顶掉自己的旧连接
  - `dropPhone` 断开指定手机、不影响别的、重复撤销幂等
  - `revoke-phone` 按哈希撤销后凭据随即失效
  - 手机凭据不能撤销别人（403）
  - 哈希格式不对 400
  - 撤销一个本机没有的哈希是幂等 200（否则 hermix 侧撤销会被拖成失败）
- `cargo test -p hermix-server --lib` —— **398/398 通过**（原 393 + 新增 5 例绑定表
  测试：往返、坏哈希必须拒、remove 幂等、同 device_id 换新哈希必须覆盖旧值、
  文件损坏不 panic）。
- `apps/desktop-tauri` `tsc --noEmit` 无错误；`vitest` 243/243。
- `apps/hm` `vue-tsc` 无错误；`npm test` 51/51；`build:h5` 通过。

- **⚠️ 踩到一个语言级陷阱（已写进 AGENTS.md）**：Rust 1.92 起**即使 edition 2021
  也不再把 `/…/` 解析成正则字面量**，写 `if !/^[0-9a-f]{64}$/.test(s)` 会报
  `expected expression, found '/'` —— 看起来像括号配平错误，其实是语言特性没了。
  改用手写 `bytes().all(...)`；仓库既有代码本来就用 `regex::Regex::new()`。
- **未做端到端验证**：需要一台真 relay + 真手机，确认「撤销后连接真的断、
  界面状态真的变」。这是唯一能确认贯通的方式。
