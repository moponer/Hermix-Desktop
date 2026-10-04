# Decision Note: MCP 目录三层取数全断的修复（limit 422 / fallback 解析失败 / modelscope 假源）

Status: implemented — 官方 registry 拉取改为 100/页游标分页并与精选目录合并；8 条 pypi 条目改 manual；删除未接线的 ModelScope 实验源。

## Decision

目录（`hermix-server/src/dashboard/mcp_catalog.rs`）此前**任何情况下都拉不到数据**，三层取数全部失效，本改动按层修复：

1. **网络层**：`fetch_remote` 从 `limit=200`（官方 registry 校验 `limit <= 100`，超限一律 422，因此从未成功过）改为 `limit=100` + `version=latest`（服务端每 id 只回最新版，实测同样的 100 条从 54 个唯一 id 变 100 个）+ `metadata.nextCursor` 游标翻页（官方源取 5 页；probe 探测只取 1 页）。
2. **精选层**：官方源联网成功时把内置精选目录（`catalog_fallback.json`）垫底合并后再写缓存；精选 id 命中的在线条目不覆盖——精选条目的安装命令是人工核验过的固定版本。registry 按 id 字母序返回，只拉前几页永远排不到 `io.github.*` / `com.*` 段的高频 server，精选目录垫底正是补这个洞。
3. **兜底层**：`catalog_fallback.json` 里 8 条 `kind: "pypi"`（枚举只有 remote/npm/manual）导致整表反序列化失败、断网兜底也是 0 条；全部改为 `kind: "manual"`。新增 `builtin_fallback_parses_nonempty` 回归测试。

同时删除「ModelScope MCP 广场」实验源及整套实验源可见性开关（枚举变体、源清单、`show_experimental`、前端下拉勾选）。该源从未接入魔搭：`fetch_remote` 没有 modelscope 分支，选它实际一直在请求官方 registry，只是换了名字。旧配置文件里存了 `"modelscope"` 的会解析失败并回落官方源。

## Alternatives considered

- **全量爬取 registry（数千条）** — 否决：50+ 页串行请求拖慢首开，绝大多数是小众条目，客户端搜索也消化不了。
- **只拉一页 100 条** — 否决：字母序 slice 全是无名 server，目录打开即"看起来坏了"；与精选合并才能让首屏可逛。
- **实现 ModelScope 适配器** — 否决：该 API 形状在开发机网络上无法验证（TLS 握手被掐断），盲写适配器只会重演"源存在但名不副实"。
- **保留 modelscope 选项但如实改名** — 否决：展示一个必然拉不到"魔搭数据"的选项比没有这个选项更误导。
- **给 CatalogInstall 增加 pypi 变体** — 否决：Manual 的语义（不自动执行未知包管理器、给手填提示）本来就是为这类包设计的，加变体是平行复制。

## Consequences

- 目录首屏 = 精选 32 条（带中文、带分类）+ 官方 registry 字母序头部约 500 个社区条目；字母段之外的社区条目不在列表里——放弃全量覆盖，换取 bounded 的首开耗时。
- 丢弃了"数据源可以只是换个名字"的错觉：现在下拉里的每个源都有真实对应的取数路径。
- 失去 ModelScope 入口；其复职条件见下节。

## Reintroduction conditions

ModelScope（或其他国内镜像）官方文档给出稳定 API 形状、且在真实国内网络下通过本机探测（`/api/mcp/catalog/probe` 全绿）时，可按第四源恢复；届时适配器必须真实存在于 fetch 层并带响应形状测试。
