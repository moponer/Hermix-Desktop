# Decision Note: 接上 relay 出站链路（配置 + 凭据 + 启动）

Status: implemented — 2026-10-09

## Decision

把 relay 子系统从「写完了但一根线没接」变成「真的在跑」。分五件：

1. **配置落地**（新 `crates/hermix-server/src/relay_settings.rs`）
2. **HTTP 面**：`GET/PUT /api/relay/config` + `POST /api/relay/phone-credential`，均 Local-only
3. **启动接线**（`HttpServerState::with_relay_client()`）
4. **两端拿到凭据**：桌面端渠道面板填写；手机端设置页填写并带头连接
5. **手机凭据自助签发**（relay 新增 `POST /api/relay/issue-phone`）

第 5 件是在并行开发提交（`a08cf8ff`，21:03）之后才做的——`apps/relay/src/server.js`
在那之前一直被占用。没有它，加一台手机要登服务器敲一次 `relayctl add-phone`。

## Alternatives considered

### 为什么之前这套完全没跑

`relay_client::connect_and_serve` **没有任何生产调用方**：`#[cfg(test)] mod tests`
从 `relay_client.rs:363` 开始，唯一一次调用在 `:404`（测试里）；`RelayConfig`
只在 `:367` 与 `:378` 被构造，两处都在测试模块内；全仓库 grep 也没有任何
`/api/relay/*` 端点。`relay_client` / `relay_transport` / `relay_identity` /
`relay_proto` 是四个完整但从未被启动的模块。

### 为什么 relay 与 Rust 几乎不用改协议

数据通路**本来就通**：`router.js` 按 **node_id** 路由（不是 session_id），
`auth.js::authenticate(token)` 直接从手机的 `hrel_*` 查出它注册在哪个 node 名下。
所以 relay **不需要任何配对逻辑**，也不需要新 tag、不需要 `PROTO_VERSION` bump。
这修正了先前「relay 配对中转 + Rust pair tag，跨三端 3 个 PR」的判断——那个判断
错在假设 relay 必须自己拥有配对码的判定权。

### 被否决的选项

| 方案 | 否决理由 |
|---|---|
| 明文存 `hrel_*`（照抄 `~/.hermix/` 其它文件的做法） | DPAPI 已经在 `relay_identity.rs` 里用着了，复用零成本。`hrel_*` 泄露即可冒充这台电脑接入路由。 |
| 配置读坏了静默回落成「未配置」 | 用户的现象会变成「我明明配过」，而真相是「文件坏了」——排查方向完全相反。`load_error` 带着走，由 API 原样回给界面。 |
| 用 `std::sync::Mutex` 存设置 | `PUT /api/relay/config` 是 async handler，在异步上下文持 std 锁是经典自死锁来源。 |
| 保存配置时杀连接重连 | 正在推进的连接不该被一次保存打断。退避循环下一轮自然重连。 |
| 手机端用 `new WebSocket` 带头 | 浏览器 WebSocket **无法设置自定义头**。必须 `uni.connectSocket({ header })`，App/小程序端由运行时映射到原生实现。 |
| relay 里加「签发手机凭据」端点 | **已做**（第 5 件）。它不扩大攻击面：node 凭据本来就能代表这台电脑接入路由，给它签发同 node 名下的手机凭据只是换个形式；反过来**没有 node 凭据就拿不到手机凭据**，所以不是匿名签发口。手机凭据不能再换手机凭据（防凭据扩散）。 |

## Consequences

- **得到**：桌面端能配 relay、能看真实连接状态（`disabled` / `connecting` /
  `connected` / 上次失败原因），并**真的发起出站连接**。
- **得到**：`node_id` 由 E2EE 公钥前 4 字节派生（`hermix-<8 hex>`），稳定且与
  `QrPayload::server_id` 同形状，两边能对上。不引入新的随机源。
- **得到**：`GET /api/relay/config` **不回 token 明文**，只回 `has_token`。
  界面没有理由显示它，回了只会出现在 devtools / 截图 / 日志里。
- **⚠️ 有地址没凭据时不连接**：直接返回并写明原因，而不是连一次必然被 401。
  否则界面上只会显示「未连接」，用户查不到是缺凭据。
- **⚠️ 电脑侧凭据的第一次签发仍然是手动的**：要在 relay 服务器上执行一次
  `node src/relayctl.js add-node <node_id>`，每台电脑一次，界面里已经把这条
  命令连同本机 node_id 直接显示出来了。
  **刻意保持手动**：relay 若能自助签发电脑侧凭据，就等于对公网开放注册——
  任何人都能凭空造一台「已登记的电脑」。第一次签发必须是运维在服务器上做的，
  这与「谁能注册节点」本来就是自建服务的运营者决定一致。
  之后的每台**手机**都是自助的（桌面端一键生成），不再需要登服务器。
- **⚠️ 手机凭据明文只在签发那一刻可见**：桌面端面板会显眼地显示一次并提示
  「关掉就只能在 relay 服务器上重新签发」。这是与 `relayctl` 同一约定，
  不做「随时回看」——那等于在桌面端长期留一份明文凭据。
- **不涉及 H5**：交付目标只有 App（Android），H5 不在范围内。`apps/hm` 的
  `npm run build:h5` 保留的理由与 H5 目标无关——仓库没有 JDK / Android SDK，
  它是 `scripts/ci-gate.sh` 第 4b 步里手机端**唯一**能跑的编译门禁；
  拿掉它，手机端就完全没有自动化验证了。
- **不变**：`PROTO_VERSION` 未 bump（没加 tag）；帧格式、E2EE、`verify_frame_auth`
  全部未动。
- **回滚**：`git checkout -- crates/hermix-server apps/hm apps/desktop-tauri/src/components/settings/`。

## 验证记录

- `cargo check -p hermix-server --all-targets` —— 通过（过程中修掉：`put` 未导入、
  router state 是 `HttpServerState` 而非 `Arc<…>`、`tokio::sync::Mutex` 不可 `Clone`
  故字段外层套 `Arc`、三个测试构造器缺新字段）。
- `cargo test -p hermix-server --lib` —— **393/393 通过**（含新增 7 例）。
- `apps/relay` `npm test` —— 25/25 通过（新增 4 例：签发端点的 401 / 403 /
  签发出的凭据**真的能通过校验且挂在正确 node 名下** / 手机凭据不能再换手机凭据）。
- `apps/desktop-tauri` `./node_modules/.bin/tsc --noEmit` —— 无错误。
- `apps/hm` `vue-tsc --noEmit` —— 无错误；`npm test` 51/51；`npm run build:h5` 通过。

新增的 Rust 测试集中在「配置读坏了怎么办」，因为那里最容易写出静默降级：

- 文件不存在 = **正常**（没配过不是错误）
- 损坏的 JSON → 带 `load_error` 回来，且不误判为启用
- 版本号不认识 → 同样带错回来
- token 密文被手改 → 解不开、带错回来，**不**拿一个错的 token 去连
- token 往返一致，且**明文不出现在磁盘上**
- 空 token 往返后仍是「有地址、未配凭据」

- **未做端到端验证**：没有真跑一个 relay 服务器 + 一台真手机。连接是否真的
  建立、帧是否真的通，需要 `relayctl add-node` / `add-phone` 之后在真机上确认。
- **未做真机 UI 确认**：桌面端设置页需要起 Tauri 进程才能看 relay 配置区的
  实际观感。
