# Decision Note: 审批 REST 面补会话归属闸（require_own_session）

Status: implemented — `list_pending_approvals` 与 `approve_via_rest` 已加 `require_own_session`，测试全绿（hermix-server 415 passed）

## Decision

审批 REST 面的两个端点现在与手机面其它端点共用同一把 sid 归属闸：

- `GET /v1/sessions/{session_id}/approvals/pending`（`routes.rs::list_pending_approvals`）
- `POST /v1/sessions/{session_id}/approval`（`routes.rs::approve_via_rest`）

两个 handler 新增 `Extension<Origin>` 提取（Origin 由全局中间件
`security::request_guard` 注入，security.rs），并调用
`mobile.rs::require_own_session`：`Origin::Local` 全放行（桌面端行为不变），
`PairedDevice` 只能访问 `own_session_id` 派生的那一条会话，越权返回 403。
`require_own_session` 因此从 `mobile.rs` 私有提为 `pub(crate)`。

动机：这两个端点注册在桌面段，最初是给 Tauri 前端的 WS 掉线兜底设计的，
直连场景下靠本机隔离，handler 签名里没有鉴权提取器。手机端（hermix M）
经 relay 接入后，这些路径同样从公网可达——设备签名验证通过只回答
"你是谁"，不回答"这条会话是否属于你"；不补归属闸，一台配对设备可以
枚举/批准另一台设备的审批（批准 = 放行一次工具调用，是真实的越权面）。
与 `interrupt` / `/v1/tasks` 已有的按设备过滤语义对齐：签名有效 ≠ 会话属于该设备。

## Alternatives considered

- **在 relay 代理层拦**：否。relay 是字节级透传（见 `relay_transport.rs`
  模块注释，签名字节级透传是手机端签名有效性的根因），在代理层解析
  `/v1/sessions/{sid}/…` 并做归属判断等于把服务端信任边界复制到另一台
  服务器上，两份判定必然漂移；且 relay 无权读桌面端的设备注册表。
- **给审批面新写一套 mobile 专用端点**（如 `/v1/approvals` 设备级列表）：
  否。会引入第二套端点语义与第二份过滤逻辑；已有的 `approvals/pending`
  形状正确，缺的只是一道闸。手机端已改为从 `/v1/tasks` 派生 sid 后
  按会话拉取，设备级端点无需存在。
- **只加 `Capability` 检查不加归属闸**：否。Capability 回答"这类操作
  允不允许"，不回答"这条数据归不归你"——中断端点的实践（先
  `check_capability` 再 `require_own_session`，`mobile.rs`）证明两层
  都必要；本次两个 handler 沿同一顺序，后续若需要收紧能力位照此补。

## Consequences

- 服务器上任何 `PairedDevice` 来源对非本会话 sid 的审批读取/提交从
  静默成功变为 403。依赖跨设备代批的假设场景（不存在已知用例）会破。
- `require_own_session` 跨模块可见（`pub(crate)`），mobile.rs 之外多了
  一个调用方；闸的语义修改现在影响手机面 + 审批面全部调用点。
- 本地（Local）来源路径零行为变化——桌面端 WS 兜底、测试均不受影响
  （415 个库测试通过佐证）。
- 审批面仍未做 `Capability::Chat` 级检查（interrupt 有）；当前配对设备
  scopes 默认含 Chat，实际不可达，但这是已知的最小改动边界，不是
  完整的对称收敛。若日后引入不含 Chat 的设备 profile，需回头补能力位检查。
