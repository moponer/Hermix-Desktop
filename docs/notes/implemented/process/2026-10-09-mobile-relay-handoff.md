# Decision Note: 交接 —— 手机端重构 + relay 链路 + 撤销修复

Status: implemented — 2026-10-09 · 本篇是**交接索引**，细节在末尾列出的五篇笔记里

## Decision

> **接手前必读这一节。** 一句话现状：
> **代码全部完成、全绿、但一行都没提交，且端到端链路一次都没在真机上跑过。**

### 起点与基线

- 仓库：`F:\<workspace>\hermix-desktop`，分支 `main`
- 基线提交：`a08cf8ff`（在线更新链路 + 自研安装器 + 欢迎词动态化）
- **工作区有 39 个未提交改动**（33 个已跟踪文件修改 + 6 个新增目录/文件）。
  接手第一件事确认 `git status` 与下面「改了什么」一致，**不要**假定这些已入库。

### 本次做了四件事

| # | 事项 | 状态 | 决策笔记 |
|---|---|---|---|
| 1 | 手机端启动故障修复 + 界面重构 | 完成 | `2026-10-09-mobile-ui-refactor.md` |
| 2 | 桌面端连接渠道改主从布局 | 完成 | `2026-10-09-desktop-connect-channels-layout.md` |
| 3 | 手机配对改局域网直连 | 完成 | `手机直连配对方案（已删除）.md` |
| 4 | relay 出站接线 + 撤销贯通 | 完成（两篇） | `2026-10-09-relay-client-wiring.md`、`2026-10-09-relay-revocation.md` |

### 改了什么（按目录）

**`apps/hm/` 手机端 — 界面全部重写**

- 新增 `core/http.ts`（唯一出网口：`uni.request` + 显式超时 + 永不 reject）、
  `core/boot.ts`（致命错误落点）、`core/direct.ts`（局域网直连配对）
- 新增 `components/{hm-shell,hm-empty,hm-step}/`、页面 `pages/boot`、`pages/settings`
- 重写 `pages/{chat,pairing,tasks,approval,call}`、`App.vue`、`main.js`、`uni.scss`、`pages.json`、`core/{clock,pairing,relayClient}.ts`
- `manifest.json`：splashscreen 去死锁、加 CAMERA 权限、改 iOS 隐私描述
- 新增 `test/direct.test.ts`

**`crates/hermix-server/`**

- 新增 `relay_settings.rs`（配置落盘 + `/api/relay/config` + `/api/relay/phone-credential`
  + 手机绑定表 + revoke 联动）
- 改 `lib.rs`（模块、路由、`.with_relay_client()`）、`state.rs`（三个新字段 + builder）、
  `pairing.rs`（配对带 relay 哈希、撤销联动 relay）、`relay_identity.rs`（DPAPI helper 复用）、
  `approval.rs` / `test_state.rs`（补新字段）

**`apps/relay/`**

- `router.js`：`attachNode` 凭据归属 + `dropPhone`
- `auth.js`：`revokeByHash` / `isActiveHash`
- `server.js`：`POST /api/relay/issue-phone`、`POST /api/relay/revoke-phone`、`express.json`

**`apps/desktop-tauri/src/components/settings/`**

- `ImChannelsCard.tsx` 重写为主从布局 + relay 配置区 + 手机凭据签发
- `DevicesTab.tsx` 撤销结果分两半报

**`AGENTS.md`** — 新增一条 Gotcha（见下）

### 验证状态

| 检查 | 结果 |
|---|---|
| `cargo test -p hermix-server --lib` | **398/398 通过** |
| `apps/relay` `npm test` | **32/32 通过** |
| `apps/desktop-tauri` `tsc --noEmit` | 无错误 |
| `apps/desktop-tauri` `vitest run` | **243/243 通过** |
| `apps/hm` `vue-tsc --noEmit` | 无错误 |
| `apps/hm` `npm test` | **51/51 通过** |
| `apps/hm` `npm run build:h5` | 通过 |

**但没有端到端验证。** 一次都没有真 relay + 真手机跑通。以下必须由接手人实机确认：

1. 手机扫桌面端二维码 → 直连配对成功
2. 配 relay 凭据后，手机经 relay 连上电脑
3. 桌面端点「撤销设备」→ 手机侧**连接真的断**、收不到后续帧
4. 桌面端设置页 relay 配置区与手机凭据签发按钮的实际观感

### 必须知道的三个坑

1. **Rust 1.92 起没有正则字面量**，edition 2021 也不行。写
   `if !/^[0-9a-f]{64}$/.test(s)` 会报 `expected expression, found '/'`，
   **看起来像括号没配平**。用 `regex::Regex::new(&pat)?` 或手写 `bytes().all(...)`。
   （已写进 AGENTS.md）
2. **H5 不是交付目标**。`npm run build:h5` 留着只是因为仓库没有 JDK/Android SDK，
   它是 `scripts/ci-gate.sh` 第 4b 步里手机端**唯一**能跑的编译门禁。别当发布目标。
3. **`apps/relay/src/server.js` 曾与在线更新的并行开发冲突**，现已提交完（`a08cf8ff`）。
   若再有并行开发，先确认归属再改。

## Alternatives considered

以下是**已经排除、不要重做的路**，每条都记在对应笔记的 Alternatives 里：

| 做法 | 为什么不做 |
|---|---|
| 手机端配对走 relay 中转 | relay 里没有该路由且判定权在 PC，跑不通。改用局域网直连 + 长期 token。 |
| relay 侧为手机配对加 pair tag / bump `PROTO_VERSION` | router 本就按 node_id 路由，relay 不需要知道配对码。判断错了一次，别重犯。 |
| 手机端「必须有指纹才放行」硬阻断 | 读不到指纹时改为人工勾选确认——保留决策点，但不放一道本环境根本过不了的闸。 |
| 把 relay 配置做成独立设置页 | 用户明确要求放在「手机端」渠道面板里（主从布局已建）。 |
| 保留 relay 的 6 位码配对入口 | 该路径必然 404，留着等于让用户再撞同一堵墙。 |
| 撤销时 relay 失败就返回 5xx | 本地撤销已生效，报失败会让用户以为什么都没发生而放弃重试。 |
| 把手机 token 存进 `devices.db` | 审计线正在改那张表；且存的是哈希不是秘密。 |

## Consequences

### 欠着的活，按优先级

**P0 — 端到端验证**（见上「验证状态」四条）。没有它，以上所有绿灯都只是编译绿。

**P1 — 一台手机只能连一台电脑（与产品需求相违，未做）**
现状：手机凭据在 relay 侧绑定单个 `node_id`，路由在连接建立时固定；hm 端存储也是
单设备（一组 `deviceToken`/`relayToken`/`sessionId`）。每台桌面端有各自
`devices.db`，配一次拿一个 token ⇒ **手机配了 A 电脑后就连不上 B**。

> **需求（用户明确提出，不是待确认的设计）**：
> **一台手机可以连接多个桌面端，随时与不同的桌面端对话。**
> 现状与之相违。这条需求尚未实现，**任何人不要再拿它当"待讨论项"**
> ——它是既定要求。

改动方向：relay 侧把「手机身份」与「手机↔桌面端绑定」拆开，手机凭据不再钉死
某个 `node_id`；选路改由帧里声明的目标桌面端决定；hm 端存储从单设备改成
「多台桌面端各自的凭据集合 + 切换当前会话」。跨 relay + Rust + hm 三端，
工作量大于本次已完成的任何一块。

**P2 — 一台桌面端同时连多台手机的行为（现状广播，用户表述的是对现状的误解）**
用户原话是「一台手机可以连接多个桌面端设备，**但是**桌面端设备只能被一个手机端
连接」——前半句是需求（见 P1），后半句是对**现状**的理解，核对后与代码不符：
`nodeToPhones` 广播给该 node 下**全部**手机（`router.js:121`，刻意为之，注释有说明）。
用户未就此表达过要改成 1:1。**若将来要改，绑定表需能表达"这台手机绑定了哪台电脑"。**

**P3 — relay 侧的其他欠账**
- `POST /api/relay/revoke-phone` 需 relay 跑新代码；**部署前旧 relay 不支持该端点**
- relay 的 `/health` 可补 `Access-Control-Expose-Headers: x-hermix-cert-sha256`，
  让自报指纹在允许读取该头的环境里真正可见（一行配置）
- 手机端 `uni.setStorageSync` 明文存凭据的未决项**依然存在**
  （`apps/hm/core/pairing.ts` 文件头有说明），别当成已解决

**P4 — 桌面端描边统一**
新做的连接渠道组件是**零描边**（与手机端同一套规则），而桌面端设置页其余部分
仍大量用 `border-border-primary`。要全量统一需单独一轮，用户已表示"要做再说"。

### 风险与不可逆点

- **未提交**：39 个改动全在工作区。任何 `git checkout --` / `git clean` 都会丢。
  建议接手后**先提交**再继续。
- `crates/hermix-server/src/relay_settings.rs` 是新增文件，`PhoneBindingStore`
  写的是 `~/.hermix/relay_phones.json`（只存哈希，不是秘密）。
- `apps/hm/pages.json` 首屏改为 `pages/boot`；boot **无条件**进主界面，
  未配对时主界面里是一张「还没有连接电脑」卡片，不是拦截层。
- `apps/relay/src/router.js` 的 `attachNode` 签名从 `(nodeId, conn)` 变成
  `(nodeId, tokenHash, conn)`，返回值从 bool 变三态字符串。relay 内部与测试已同步，
  外部若有调用方需一并改。