# Decision: MCP 客户端换 streamable HTTP 传输 + token 字段

Status: implemented
Class: architecture
Date: 2026-10-11

## Decision

桌面端 MCP 客户端的远程传输从自研 `HttpSseTransport` 换成标准 **streamable
HTTP**(2025-03-26+ 规范);`McpServerEntry`(mcp_servers.json)新增 `token`
字段,连接时以 `Authorization: Bearer <token>` 附带;粘贴导入(Claude/Cursor
风格 JSON)把 `headers.Authorization` 收敛为 token,不再静默丢弃。

## 背景(为什么必须换)

原 `HttpSseTransport` 无法与任何符合规范的服务端工作,三处独立缺陷:

1. `start()` 对 `GET /sse` 做 `resp.text()`——把整条响应体读到底,而 SSE 流
   永不结束,握手无限挂起(reqwest 无任何超时兜底);
2. 端点解析只认 JSON `{"endpoint":…}`,规范发的是裸路径
   `data:/messages/?session_id=…`,解析失败后 POST 到错误的默认端点;
3. 通知的 202 空响应让 `send()` 的 JSON 解析直接报错。

且 `McpServerEntry` 无凭据字段,`with_auth`/`BearerTokenAuth` 管线存在但从未
被接通——用户粘贴的 headers 被静默丢弃,远程服务必然 401 且无诊断。

## Alternatives considered

| 方案 | 为什么不 |
| --- | --- |
| 修复 legacy SSE 传输 | 要把 start() 改成流式保活 + 每消息经 SSE 通道收响应,改动更大;legacy SSE 本身已被规范废弃;streamable 是现行标准,自研 ops-mcp / Claude / Cursor 同吃。 |
| token 走 DPAPI 加密落盘 | 正确方向但独立成步:先让链路通(明文存 mcp_servers.json,与 relay.json 同信任级);DPAPI 包裹是后续增强,不阻塞。 |

## Consequences

- **得到**:桌面端可直连任意标准远程 MCP 服务端(自建 ops-mcp、托管网关),
  粘贴连接 JSON 即用;hermix-mcp 与 hermix-server 全测试绿
  (31 + 415),另有 `#[ignore]` 的线上联测
  (`tests/streamable_live.rs`,OPS_MCP_URL/OPS_MCP_TOKEN 驱动)。
- **代价**:token 在 mcp_servers.json 明文落盘(本机用户账户即边界);
  请求超时上限 120s,慢工具调用超过此值会失败——对运维类工具足够。
- **边界**:客户端改造不授予任何权限;访问控制在服务端(key + scope 闸)。
  "谁能连接" = "谁被发给含 key 的连接 JSON",key 可按人签发、即时撤销。
