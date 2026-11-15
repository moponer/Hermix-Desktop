# Decision Note: 是否自建 MCP / Skills / Custom Apps 市场服务（远程拉取型）

Status: rejected — 2026-10-04 拍板「不建，纯本地，取消远程拉取」；本文档保留作为**被否决方案**的记录，替代方案见 `implemented/feature/2026-10-04-mcp-catalog-local-only.md`

## 否决理由

- **官方 registry 本身对国内线路不友好**（其自述即"境外线路，有代理时数据最全"），实测直连超时。取消远程拉取后这条失败路径整个消失，目录变成一个确定性函数——同一个版本永远给出同一份列表。
- **自建服务解决的是信任问题，不解决可用性问题**，而当前瓶颈在后者：21 条精选里 13 条需要用户填凭据，UI 此前硬编码 `env: {}` 让它们**看得见装不上**。给一个装不上的市场做品牌化服务，信任损失比没有市场更大。
- **没有使用数据支撑策展**：`hermix-telemetry` 只有进程内聚合计数，`record_tool_call(duration)` 连工具名都不收。在没有任何按扩展维度使用数据的情况下运营一个市场，"首屏推荐哪几个"只能靠拍脑袋。
- **Skills 与 Custom Apps 的阻塞与市场正交**：Skills 是签名验证为占位实现且架构文档已判 TRIM；Custom Apps 是打包形态与源码扫描器的契约冲突，且插件 crate 零网络代码。把三个不同性质的问题绑进一个服务只会叠加风险。
- **运维成本与收益不成比例**：一旦用户切到自建源，市场不可用就是他的目录不可用；而"可用性"本来已被官方 registry + 缓存 + 兜底解决大半。

## 问题定义

「内建市场」= **自建一个远程服务**，hermix 从它拉取市场数据；不是本地打包的静态清单。范围问题：MCP / Skills / Custom Apps 三类，哪些该纳入这个服务。

## 决定的前提事实（均来自代码或本机实测）

### A. 客户端侧：MCP 已经支持自建源，且是官方认可的路子

| 事实 | 证据 |
|---|---|
| 三个数据源 `Official` / `BuiltinOffline` / `SelfHosted`，后者注释为「用户自建子注册中心（官方 ghcr 镜像，**API 与官方同形**）」 | `mcp_catalog.rs:164-171` |
| 自建源与官方同路径同形状，「官方文档明确子注册中心复用同一路径」 | `mcp_catalog.rs:411` |
| 优先级：`HERMIX_MCP_REGISTRY_URL` env > `mcp_registry.json` 的 `registry_custom_url` > official 回落 | `mcp_catalog.rs:226-252` |
| 契约：`GET {base}/v0.1/servers?limit=100&version=latest[&cursor=name:version]`，响应 `{servers:[…], metadata:{next_cursor}}`，envelope 的 `_meta["io.modelcontextprotocol.registry/official"].isLatest` 标最新版 | `mcp_catalog.rs:395-442` |
| 客户端容忍自建镜像不支持 `version=latest`（退化取"最后出现者"） | `mcp_catalog.rs:444-454` |
| UI 有自建 URL 输入框 + 保存前 probe 试测 | `CatalogSourceBar.tsx:119-150`、`mcp_catalog.rs:826-848` |

**推论：MCP 侧建服务不需要改客户端。** 这是本方案最大的成本优势。

### B. 供给不缺，缺的是转化

本机 12 天日志（2.5 MB）正则挖掘：156 次 LLM 工具调用，**79 个已注册工具只用到 18 个**，`terminal` 51.9% + `write_file` 23.7% = 75.6%。32 条精选 MCP **实际安装 0 条**（`mcp_servers.json` 唯一那条 `computer-use-mcp` 是应用 seed 的，`"seeded": true`）。

`hermix-telemetry` 只有进程内 `AtomicU64`，`record_tool_call(duration)` 连工具名都不收，`metrics.db` 也不是它写的 —— **产品内没有任何按扩展维度的使用数据**。

### C. 安装闭环有硬伤，与有没有服务无关

32 条精选按可安装性分桶：**A 真能一键装 10 条 / B 差一个 env 输入框 14 条 / C `manual` → 501 共 8 条**。可用率 31%。

- B 桶：`McpCatalogGrid.tsx:83` 硬编码传 `env: {}`，14 条（github/slack/notion/exa/firecrawl/supabase/neon/redis…）必 400
- C 桶：`Manual` 分支直接 `501 NOT_IMPLEMENTED`（`mcp_catalog.rs:958-963`）
- `builtin-offline` 源不写缓存而 install 只查缓存 → 该源下点安装必然 412

### D. Skills 与 Custom Apps 的阻塞都不在"市场"

- **Skills**：`SkillsHubClient` 的 search/download/check-updates/upload **全部实现但从未接线**（`with_hub` 无生产调用方），且 `DEFAULT_HUB_URL` 写死 `agentskills.io/api/v1`，`verify_skill_signature` 是占位实现（`hub.rs:344-369`，测试只验十六进制长度与字符集）。而 `2026-09-24-plugin-kernel-design.md:232` 明确判 hermix-skills 为 **TRIM**（"留 loader ≈650；hub/validator/version 删"）。
- **Custom Apps**：`hermix-plugin` 无 reqwest、零网络代码。更根本的是**形态契约冲突** —— `hx-packager install` 产出 `dist/*.exe` + `installed.json`，而 `PluginScanner` 只认源码形态（须有 `index.html` 或 `src/main.tsx`，`scanner.rs:86-111`）。日志里的 `loaded 0 custom apps` 是必然结果，不是用户配置错误。

### E. `mcp_scan` 是纯本地词法启发式

Pass/Warn/Block 依据来源白名单、仓库卫生（star/fork/文档）、版本钉死、安装命令存在性、名称语义匹配（`mcp_scan.rs:13-15, 88-90, 159-162`）。**没有真实验签、没有审计状态、没有下架/公告机制** —— 这正是自建服务能创造增量价值的地方。

### F. 一条硬约束：目录最多露出 ~500 条

`CATALOG_PAGES = 5` × `PAGE_LIMIT = 100`，且 registry 按 id 字母序返回，代码只取字母表头部。本机缓存 528 条 = 411 remote + 97 npm + 20 manual，与"5 页 × 100 归一化"吻合。

**自建市场因此只能定位成"策展短名单"，不可能是全量索引。** 这不是缺陷 —— 策展本来就是自建服务该做的事 —— 但它决定了产品设计意图必须写清楚。

## Decision

**分三档，本次只推荐第一档。**

1. **MCP：做，但定位为「策展层 + 第一方层」，不是竞争型 registry。**
   - 服务只服务官方 registry 的**策展子集 + 第一方条目 + 自有信任元数据**，不同步全量生态
   - 增量价值集中在 E（信任）与版本生命周期，不在可用性（官方 registry + 24h 缓存 + 三层兜底已经够了）
   - **前置条件**：C 桶的安装闭环必须先修或同期修 —— 给用户一个装不上的市场比没有市场更糟
2. **Skills：暂不做。** 阻塞在签名验证是占位实现 + 架构文档已判 TRIM，两者都与"建服务"正交。且客户端形态是 `agentskills.io` 专属，不是通用 hub 客户端
3. **Custom Apps：不做，且不是现在。** 阻塞在形态契约冲突（打包 vs 源码）与零网络代码，连"目录"都还谈不上

## 建议的第一步（不是建服务）

**用一份静态 JSON 文件验证自建源链路，而不是写服务。** 把符合 A 节契约的静态文件丢到任意静态托管（GitHub Pages 即可），把 `registry_custom_url` 指过去、切到 self-hosted 源，看目录是否正常消费。

成本约一小时、零服务端代码，却能一次性验证整条客户端路径。**客户端连自建源都吃不对的话，任何服务设计都还轮不到讨论。**

## Alternatives considered

| 方案 | 为何否决 / 待定 |
|---|---|
| 建全量竞争型 MCP registry | 可用性无差异化（官方 registry 已在工作），却要承担 uptime、下架流程、第三方代码的版本承诺与供应链责任。运维成本与收益不成比例 |
| 只修安装闭环 + 加 telemetry，不建服务 | 保守选项。10/32 → 近 32/32 的可用率提升是确定的；但"该内建什么"仍然只能靠拍脑袋，因为没有使用数据。**如果预算只够做一件事，选这个** |
| MCP + Skills + Custom Apps 三类统一市场 | 一次性解决碎片化（当前三套互不兼容的分发机制）。但 Skills 缺真实验签、Custom Apps 连网络层都没有，属于三个不同性质的问题绑在一起做，风险叠加 |
| 现在就建服务，装机闭环之后再说 | 市场服务会放大"装不上"的问题：条目越多、信任标识越强，用户点了装不上时的信任损失越大 |
| 用 GitHub raw 静态 JSON 当"市场" | 与 A 节契约同形即可，零运维。本质是"策展层"的最小实现——可作为第 1 档的起点形态，不必一开始就上动态服务 |

## Consequences

- **MCP 侧客户端零改动**是本方案成立的基础；若将来官方 registry API 形状变更，自建源需同步适配（与官方源同一风险面，不新增）
- **策展是持续成本**：现在是 32 条手工维护（`catalog_fallback.json` 的 `comment` 字段已写明准入规则：安装命令真实存在 + `mcp_scan` 全绿 + 中英文描述齐）。扩到数百条需要明确谁审、审什么
- **自建服务会引入新的运维面**：一旦用户切到 self-hosted 源，市场不可用就是他的目录不可用。必须保留 official 回落（现已有）与 24h 缓存
- **未解决**：telemetry 缺失会让"策展该选哪 3 个放首屏"仍然无据可依。建议把按扩展维度的调用埋点列为与第 1 档同期的工作项
- **未验证**：自建源在真实场景下的消费效果。本文所有关于自建源的判断均来自代码阅读与契约推导，未做过端到端实测——这正是建议先做静态文件实验的原因
