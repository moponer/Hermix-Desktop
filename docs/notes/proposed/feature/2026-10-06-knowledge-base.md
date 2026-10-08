# Decision Note: 本地知识库（无向量 RAG：BM25 全文 + 事件-实体图谱）

Status: proposed — 完整方案已评审成形，待拍板后按里程碑实施。11 项硬性需求逐条有落点，无缩水。

## Decision

为 Hermix 增加一个**本地优先、无向量库**的知识库子系统，核心选型一句话：

> **tantivy(BM25 全文检索) + SQLite(元数据与事件-实体图谱) + LLM 结构化抽取(价值判断/分类/实体事件) + sigma.js(图谱探索 UI)**，文件本体留在原地不复制、可配置远端同步。

检索链路刻意**不含 embedding/向量库**（用户明确要求，见 Alternatives）：召回由三路补偿——

1. **BM25 全文**（tantivy，`tantivy-jieba` 中文分词）：关键词、短语、布尔、字段过滤；
2. **图谱导航**（实体/事件关联查询）：从已知实体出发扩展，弥补关键词的语义缺口；
3. **LLM 查询改写**：`kb_search` 工具让 agent 在检索质量差时自动改写/拆分查询再检索——这是无向量架构下补同义改写召回的关键机制。

### 需求逐条映射（11/11，无缩水）

| # | 需求 | 落点 |
|---|------|------|
| 1 | 定期扫描指定盘符 / 手动上传（单个+批量）→ 价值分析 → 入库 | `kb_scanner`（hermix-cron 定期 + notify 实时监听 + 手动触发三通道）；上传走 HTTP API；两者汇入同一条入库管线 |
| 2 | 不用 RAG/向量库 | 架构级排除：无 embedding 字段、无 ANN 索引；召回 = BM25 + 图谱 + LLM 改写（上文） |
| 3 | 相似文件比对：该更新更新、重复取消 | 双层去重：SHA-256 精确命中→跳过；SimHash 近似（海明距离 ≤ 6）→ 交 LLM 判「同一文档的新版本（更新）」还是「重复（取消）」；mtime+size 变化是重分析的触发器 |
| 4 | 自动分类入库 + 问答朔源 | LLM 结构化抽取一次产出 `{value, category, title, summary, entities[], events[]}`；规则兜底（扩展名/目录）；每条检索结果携带 `doc_id + para_seq`，前端点击即打开原文段落（复用桌面端现有预览面板） |
| 5 | 本地存储 + 可配置远端同步 | 文件**原位存储**（不搬动用户文件，只建索引）；上传件落 `data/kb/files/`；远端同步是「元数据库+上传件目录」的 rsync/S3/WebDAV 推送器，配置开关，默认关 |
| 6 | 任务产出的有价值文档/产物自动入库 | agent loop 加产物 hook：`write_file`/`office_cli` 等写类工具完成后，产物路径进 KB 队列走同一管线（价值门槛照走，不是无脑全收） |
| 7 | 长期/短期记忆自动入库 | `hermix-focus` 的 `memory_router` 在记忆晋升（短期→长期）时发出 webhook 进 KB 队列；显著度阈值复用现有 `salience` 评分，避免低价值记忆刷库 |
| 8 | 探索模式：知识宇宙视图（搜索事件/实体、浏览关系、打开细节/原文、不离开同一视图） | 新增「知识宇宙」页面：中央 sigma.js 力导图 + 左侧搜索/过滤栏 + 右侧详情抽屉（事件详情→原始来源文档），全部在单一路由内完成，点开任何节点不清空画布 |
| 9 | 列表视图 ↔ 图视图切换 | 同一数据源两种投影：SQLite 图查询 API 返回 `{nodes, edges}`，列表/图切换只是前端渲染层开关，保持选中态与过滤态 |
| 10 | 知识图谱 | `entities` / `events` / `event_entities` 三表 + 共现/引用边；LLM 抽取 + 实体归一（别名表 + 编辑距离合并） |
| 11 | 速度快、性能强 | 见「性能预算」节：MFT/USN 秒级扫描复用、tantivy 增量索引、SQLite WAL、LLM 队列并发限流、WebGL 渲染万级节点 |

## 架构与模块布局

新增 crate `hermix-kb`（依赖链：`hermix-core` → `hermix-config` → `hermix-kb` → `hermix-agent`/`hermix-server`），复用现有能力不重造：

```
hermix-kb/src/
  lib.rs      // KbStore 门面：open(data/kb/)，WAL 模式
  scan.rs     // 盘符扫描：复用桌面端 MFT/USN helper + jwalk 兜底
  ingest.rs   // 入库管线状态机：Discovered→Extracting→Analyzing→Ready/Rejected
  analyze.rs  // LLM 结构化抽取（价值/分类/实体/事件）+ 规则兜底
  dedup.rs    // SHA-256 精确 + SimHash 近似
  index.rs    // tantivy 封装（tantivy-jieba 中文分词）
  graph.rs    // 实体/事件图查询（SQL 递归 CTE）
  sync.rs     // trait SyncBackend：WebDAV / S3 / 本地文件夹，默认关
  watch.rs    // notify 实时监听已配置目录
```

- **扫描**：桌面端已有 MFT/USN 提权 helper（秒级，见
  `implemented/architecture/2026-10-04-local-index-startup-and-progress-channel.md`），
  kb_scanner 复用其输出流按扩展名过滤（md/txt/pdf/docx/xlsx/pptx/csv/html/rtf/epub
  + 白名单）；服务端/CLI 形态走 jwalk 兜底。
- **文档→文本**：复用 `hermix-tools` 既有链——`read_pdf`（Poppler）、
  `office_cli`/`markitdown`（Office→MD）、纯文本直读；抽取文本缓存进
  `data/kb/extracts/<doc_id>.txt`，重分析零重复解析。
- **定时**：`hermix-cron` 加 `kb_rescan` 任务类型；盘符/间隔设置页可配，默认每周。
- **agent 工具**：注册 `kb_search` / `kb_ingest` / `kb_graph` 三个工具。

## 数据模型（SQLite，WAL）

```sql
documents(doc_id PK, source,           -- scan|upload|artifact|memory
  origin_path,                         -- 文件原位存储，不复制
  file_hash,                           -- SHA-256 精确去重键
  simhash, mtime_ms, size,
  title, summary, category,
  value_score,                         -- <阈值(默认0.35) → rejected
  status,                              -- pending/analyzing/ready/superseded/rejected
  supersedes,                          -- 版本链 → 旧 doc_id
  created_at, updated_at);
paragraphs(para_id PK, doc_id FK, seq, text, UNIQUE(doc_id,seq));
entities(entity_id PK, name, norm_name UNIQUE, type, aliases JSON);
events(event_id PK, doc_id FK, para_seq, kind, title, detail, occurred_at);
event_entities(event_id FK, entity_id FK, role);
doc_entities(doc_id FK, entity_id FK, mentions);   -- 共现边
```

tantivy 字段：`title` / `body`（段落拼接，position 映射回 para_seq 高亮）/
`summary` / `category`(facet) / `entities`(多值) / `doc_id`(stored)。
图查询全部 SQL（递归 CTE 做 N 度邻居），**不引入图数据库**——单机 10⁴~10⁵
实体规模 SQLite 足够，图 API 直接吐 `{nodes, edges}`。

## 入库管线（扫描/上传/产物/记忆同一条）

```
发现 → ①指纹层：SHA-256 同 → 丢弃；mtime+size 未变 → 跳过
     → ②文本层：格式抽取 → SimHash；近邻(海明≤6)存在
        → LLM 判「新版」(旧 doc 标 superseded，建版本链) 或「重复」(取消)，
        判定理由写入 audit
     → ③语义层：LLM 一次产出 {value_score, category, title, summary,
        entities[], events[]}（JSON schema 约束）；失败→规则兜底，
        value_score 缺省 0.5 待复核
     → ④落地：SQLite 事务 + tantivy 批量 commit（每批一次，非逐文档）
     → ⑤同步钩子：远端同步开启时入推送队列
```

## 问答与朔源（需求 4）

- `kb_search`：BM25 top-k → 结果实体 1 度邻居文档补召回 → 重排
  （title/category 命中加成）。返回片段 + `{doc_id, para_seq, origin_path,
  title, updated_at}`。
- LLM 引用格式 `[kb:doc_id#para]`，前端渲染可点链接 → 打开现有预览面板
  定位段落（复用 `readArtifactFile` 链）。
- 查询改写：零结果/低分时 agent 按工具 guidance 自动拆词/同义/换分类重试
  （≤2 轮）——无向量召回的核心补偿。

## 远端同步（需求 5）

`trait SyncBackend`（push 元数据快照 + 上传件文件树）：WebDAV / S3 /
本地·网络文件夹三个实现。默认关。上行增量（`updated_at` 导出 JSONL +
文件镜像）；下行启动对账（doc_id+file_hash 合并，冲突保本地记日志）。
本地永远是权威副本，同步是备份/多机手段，不是在线依赖。

## 任务产物与记忆自动入库（需求 6/7）

- **产物 hook**：`process_post_response` 收尾处收集本轮 `write_file`/
  `office_cli`/`file_converter` 成功产物路径 → KB 队列（source=artifact）。
  价值阈值照过，`data/`、`target/` 在忽略清单，临时文件不刷库。
- **记忆晋升 hook**：`hermix-focus::memory_router` 短期→长期晋升点发事件
  （短期记忆本身不入库），带 salience 分；source=memory 入库，设置可整体关。

## 性能预算（需求 11 的量化口径）

| 阶段 | 预算 | 手段 |
|---|---|---|
| 全盘扫描（增量） | ≤5s/盘（文件名级） | MFT/USN helper 复用；仅新/变文件进管线 |
| 单文档抽取 | ≤2s 文本类 / ≤15s PDF·Office | 抽取缓存 extracts/；外部转换器进程复用 |
| LLM 分析 | 并发≤3，单文档≤8s | 队列限流；小文档合并批量调用 |
| BM25 检索 | P95≤50ms @10⁵ 段落 | tantivy mmap，热索引常驻 |
| 图查询（2 度） | P95≤80ms @10⁴ 实体 | SQL 索引 + 递归 CTE，命中即停 |
| 图谱渲染 | 首帧≤1s，交互 60fps | sigma.js WebGL；>5000 节点按分类聚合抽稀 |
| 入库吞吐 | ≥30 文档/分钟（混合格式） | 管线分层并行（tokio 抽取 + Rayon 哈希） |

## 知识宇宙 UI（需求 8/9/10）

新路由「知识宇宙」，单视图完成全部探索动作：

- **中央画布**：sigma.js 力导图（节点=实体/事件，边=关联/共现；节点大小=
  连接数，颜色=类型）。WebGL 渲染，滚轮缩放/拖拽/框选。
- **左侧栏**：全局搜索（BM25 联动——搜到的文档其实体在图中高亮）、
  分类/时间/实体类型过滤器、视图切换（列表 ↔ 图，需求 9：同一查询数据
  两种投影，切换保持选中态与过滤态）。
- **右侧抽屉**：点节点 → 实体/事件详情（关联列表、出现文档）；点文档 →
  原文预览（复用预览面板，定位到 para_seq）。**抽屉打开不清空画布**，
  高亮当前路径——"不离开相同视图"（需求 8）。
- **事件-实体图**（需求 9/10）：过滤 `kind` 即得"索引产生的事件、实体和
  关联"视图；列表视图按事件时间线排布，图视图看关联结构。

## 外部轮子清单（全部活跃维护，2026-10 核实）

| 轮子 | 版本 | 用途 | 活跃度证据 |
|---|---|---|---|
| tantivy | 0.26.2 | BM25 全文检索 | 2026-09 发布，1930 万下载 |
| tantivy-jieba / cang-jie | — | tantivy 中文分词桥 | jieba-rs 0.11（2026-09-16 发布）官方 README 推荐 |
| jieba-rs | 0.11.0 | 中文分词（tfidf/textrank 关键词） | 427 万下载，2026-09 更新 |
| notify | 7（已在 workspace） | 目录实时监听 | 既有依赖 |
| sigma.js + graphology | — | 图谱 WebGL 渲染 | React 19 兼容（@react-sigma/core）；react-force-graph 因 React 19 ref 破坏性变更出局 |
| sha2 | 0.10（已在 workspace） | 精确去重 | 既有依赖 |
| SimHash/MinHash | 自写（约 200 行）或 txtfp/gaoya | 近似去重 | 候选 crate 存在但成熟度一般；SimHash 算法本身 30 行核心，倾向自写减少依赖 |
| lopdf/Poppler/office_cli | 既有 | 文档→文本 | 既有链复用 |
| rusqlite (WAL, 递归 CTE) | 0.32（已在 workspace） | 元数据+图存储 | 既有依赖 |

结论：**新依赖只有 tantivy + tantivy-jieba + jieba-rs + sigma.js 系**，
其余全部复用现有 workspace 依赖。

## 里程碑

- **M1 检索底座**：hermix-kb crate 骨架、SQLite schema、tantivy 索引、
  上传 API + 手动入库管线（含双层去重）、`kb_search` 工具 + 朔源链接。
  交付判据：上传 100 份混合文档，问答引用可点开原文。
- **M2 自动化**：MFT/USN 扫描接入、cron 定期 + notify 实时、价值分析
  （LLM 抽取 + 规则兜底）、产物 hook、记忆晋升 hook。
- **M3 图谱与探索**：实体归一、事件抽取落库、`kb_graph` 工具、知识宇宙
  页面（图/列表双视图、详情抽屉、搜索联动）。
- **M4 同步与打磨**：SyncBackend 三实现、设置页（盘符/间隔/阈值/同步/
  记忆入库开关）、性能达标验证（预算表逐项压测）。

## 对抗评审（在什么场景下这套方案会崩）

1. **LLM 抽取质量差/服务不可用** → 规则兜底只保"能入库"，实体/事件质量
   会明显下降，图谱变稀疏。缓解：抽取失败标记 `status=ready,quality=low`，
   图谱 UI 可过滤低质量节点；不阻塞检索主链路。
2. **大 PDF（>200 页）抽取阻塞队列** → 15s 预算被撑爆。缓解：页数上限
   （默认 300 页）+ 队列公平调度（大文件让位小文件）。
3. **SimHash 对短文档误判**（<50 词的文档指纹区分度低）→ 短文档跳过
   SimHash 层，直接走 SHA-256 + 标题精确比对。
4. **实体归一过度合并**（"苹果"公司 vs 水果）→ 归一只在 norm_name 完全
   一致 + 类型一致时合并；LLM 抽取时强制输出实体类型；不做自动歧义消除，
   歧义实体保留多个节点（宁多勿错）。
5. **扫描盘符含海量无关文件**（系统盘 200 万文件）→ 扩展名白名单过滤在
   MFT 流上完成（文件名级，零内容读取）；默认不选系统盘；忽略目录清单
   （Windows/node_modules/target…）。
6. **SQLite 单写者瓶颈** → 管线批量事务（每批 ≤200 文档）+ WAL；读多写
   少场景 WAL 足够；写压力过大时降级为队列削峰。
7. **图谱规模失控**（10⁵+ 实体卡渲染）→ 默认只加载当前过滤视图的子图
   （≤5000 节点），聚合节点可下钻；全量数据仍在列表视图可用。
8. **用户隐私顾虑**（LLM 分析本地文档）→ 设置页明示"入库分析会调用
   LLM"，可切规则兜底模式（零外发，价值分恒 0.5，实体事件全靠规则）。

## Alternatives considered

- **传统向量 RAG（Qdrant/sqlite-vec/fastembed）**：语义召回最强，但用户
  明确否决向量库路线（需求 2 是硬约束）。且 embedding 模型本地跑有
  geo-block/算力问题（fastembed 的 cdn.pyke.io 已知被墙）。三路召回
  补偿（BM25+图谱+LLM 改写）是该否决下的工程对策。
- **图数据库（Neo4j/sled-embed/Kuzu）**：图查询表达力强，但引入独立
  存储引擎与运维面，单机桌面场景 10⁵ 节点 SQLite 递归 CTE 已达标
  （P95<80ms），多一个引擎是纯成本。
- **复用 hermix-focus 嵌入检索（embedded/ 模块已有 store/retrieve）**：
  该模块是记忆召回不是文档检索，无 BM25/分词/段落定位，且若用它等于
  绕道向量路线，违背需求 2。
- **Meilisearch/Typesense 外部搜索服务**：部署重（独立进程/端口），桌面
  便携形态不可接受；tantivy 进程内 mmap 是零部署方案。
- **文件复制入库（Zotero/Obsidian 式vault）**：复制会制造双份真相与同步
  地狱；原位索引 + 上传件单目录是桌面工具的正确形态。

## Consequences

- 无向量召回意味着**纯语义、无关键词重叠的查询**（"讲讲那篇讲手机芯片
  的报告"→文档里全是型号词）召回会弱于 RAG。LLM 改写与图谱导航能补
  大半，但不保证追平。这是需求 2 的固有代价，接受。
- LLM 抽取是入库管线的成本大头：每文档一次调用（约 1-2k token）。批量
  初始入库 1000 份文档约消耗 1-2M token；设置页提供"仅规则模式"开关。
- 新增 1 个 crate + 4 个前端依赖（sigma.js 系）+ 3 个后端依赖（tantivy
  系），项目规模 +8~12k 行（含测试），远期由简化阶梯约束。
- MFT/USN helper 依赖管理员权限（既有约束），自动扫描在提权被拒的机器
  上退化为 jwalk 慢路径——与本地索引现状一致，不新增负担。
- 知识宇宙页是新的重前端面（约 2-3k 行），需要独立的性能预算与测试
  （表格/图渲染在低端机上 60fps 压测）。
