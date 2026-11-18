# Decision Note: 手机配对全走 relay 中转（扫码即绑定，局域网直连退役）

Status: implemented — 2026-10-10

取代 `手机直连配对方案（已删除）.md`（已归档）。

## Decision

配对链路改为**全部经 relay 中转**，用户操作收敛为一步：**电脑展示二维码 → 手机扫 → 绑定完成**。

拍板原话（用户）：「手机端和桌面端都是我们开发的，全部走中转发送请求的，电脑端展示二维码，手机端扫描，通过就绑定连接，这是最简单的。实现的复杂度不是用户的问题，你要考虑用户操作的便利性。」

链路全景：

```
桌面端                                        relay                手机
  │ 生成配对码时当场 issue-phone 签发手机凭据 ──→ 登记并绑定本 node
  │ 二维码 = {relay}/pair?c=码&t=凭据&k=PC公钥
  │        ┆（屏幕 → 相机，唯一的秘密信道）
  │                                            ←── 扫码后凭据连 /relay/phone
  │                            密封配对帧（d2n）──→ 按 node 路由 ──→
  │ ←── relay 盖章转发（token_hash 由 relay 强制盖，防伪造）
  │ 判码（TTL/一次性/按凭据分桶预算全保留）
  │ ── 密封回信（device_token，NodeToDevice）──→ 广播给 node 下手机 ──→
  │                                            按 request_id 认领，落盘凭据
```

两端凭据在配对请求/回信里都是**密封帧**（relay 拒收明文 route 帧，且
device_token 绝不能明文过 relay）：请求封给二维码里的 PC 公钥，回信封给
请求里带的手机公钥。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 维持局域网直连为主路径 | 要求同网、暴露本地 TLS 端口、肉眼比对 64 位指纹——三样都是把实现复杂度摊给用户（用户拍板否决）。 |
| 手机手动输入 IP + 6 位码（跳过指纹校验） | 安全降级无人拍板；且 uni.request 对自签 TLS 的 `sslVerify` 大概率先撞墙。用户要的是零输入，不是换一种输法。 |
| 粘贴配对链接（2026-10-10 早间短暂上线的方案） | 剪贴板不跨设备，「电脑复制→手机粘贴」要借微信传输助手之类中转一次——被用户点名不是人话，弃。 |
| relay 加 HTTP 配对端点自行签发 token | relay 签的 `hmxd_*` hermix 不认（路由级 ≠ 授权级）；判定权必须在 PC。 |
| PC 侧为配对帧放宽 `verify_frame_auth` 为全局开关 | 只对 `session_id == im:pairing` 精准豁免，且配对请求仍是密封帧 + 码预算约束；全局放宽会波及业务会话。 |

## Consequences

- **得到**：配对零输入、跨网络可用，「同一 Wi-Fi」要求彻底消失。
- **得到**：`PhoneBindingStore` 的凭据哈希在出码时就被 PC 知晓（自己签的），
  配对成功即写绑定表——撤销链路一次打通，不再依赖手机自报哈希。
- **得到**：`QrPayload.node_public_key` 终于进入二维码 URL（`k=` 参数），
  `key.exchange` 可以真正密封——补上了 2026-10-09 笔记记录的「pk_pc 拿不到」缺口。
- **改了信任锚**：从「本地 TLS 指纹比对」变成「桌面屏幕上的二维码」。能持
  配对码里那条凭据连上 relay 的，就是扫码的设备；预算按 relay 强制盖章的
  凭据哈希分桶（`router.js::phoneToNode` 无条件覆盖 `token_hash`，手机自报
  无效），穷举只烧自己的额度。
- **代价**：二维码含可用手机凭据，被拍照 = 凭据泄露（与旧直连码等价风险，
  但作用范围从局域网扩大到 relay 可达处；缓解：60min TTL + 一次性 + 预算锁定 +
  撤销联动）。二维码生成次数 = relay 凭据签发次数，relay 侧凭据表会积累未用行（待清）。
- **退役但未删**：`POST /api/pairing/complete` 与 `/pair` 落地页保留编译
  （App 端不再调用）。删它们是下一个清理批次的活，连带 `tls_addr_from_env`
  在配对路径上的依赖。
- **已知坑**：`apps/relay` `npm test` 的「安装页」用例在本决策之前就红
  （`public/index.html` 已改官网分站，测试断言没跟上），与本决策无关，待修。
- **回滚**：恢复直连 = revert 本批 commits（hm + Rust + relay 三端同回）。

## 验证记录

- `cargo test -p hermix-server --lib` —— 全绿（配对会话/预算/grant 绑定测试
  已按新签名重写；QR 格式断言 `c=&t=&k=`、无 `fp=`）。
- `apps/relay npm test` —— 33 例 32 过，1 例「安装页」为前存红（见上）。
- `apps/hm vue-tsc --noEmit` 无错误；`npm test` 52/52（direct.test.ts 重写为
  relay 配对向量 + AAD 字节一致性钉子）；`build:h5` 通过。
- `apps/desktop-tauri tsc --noEmit` 无错误（QrPayload 前端类型删 `cert_fingerprint`）。
- **未做真机端到端**：扫码 → relay → 绑定 → 常驻连接，需要在真机 + 真 relay
  上跑通。这是交接以来第二次把这条列为 P0。
