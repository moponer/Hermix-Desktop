# Decision Note: 提示词资产质量升级——专家 / 技能 / 域指令 / 工具描述对齐开源一线写法

Status: implemented — 全部 5 个批次于 2026-10-17 落地，域模式工具面接线拆分至姊妹笔记（见下）

## Decision

把「模型可见文本」的写作质量作为独立一层来治理：审计现有专家 / 技能 / 域指令 / 工具描述，对照开源一线项目（Claude Code、Codex、superpowers 等）的写法，给出**统一写作规范**（每类资产各自的模板 + 判据），并按 5 个批次直接改写落地。

改写判据统一为三条：说清**什么时候用**、说清**怎么用**（含边界与反例）、避免教科书式泛清单。机制层不动——Hermix 在机制层已属第一梯队，问题出在装入的文本本身。

## 背景

机制层的全面对比（5 层上下文、路由门控指引、子代理编排、技能渐进加载等）结论是
Hermix 属第一梯队，见本文末「机制层对比摘要」。但**模型可见文本的写作质量**与
开源一线项目差距显著：同一套机制，装入的提示词文本是教科书式泛清单、单句
description 和通用角色话术，导致路由正确、执行粗糙。本笔记聚焦文本质量层，
给出审计证据、开源对照、统一写作规范与可直接采用的改写示例。

## 现状证据（粗在哪）

| 资产 | 位置 | 现状 | 具体粗糙点 |
|---|---|---|---|
| 内置专家 | `apps/desktop-tauri/src/data/builtinExperts.ts:50` 起 | LangGPT 模板（#角色/##技能/##工作流/##规则），约 400 字/个 | ①零工具接线：通篇不提 hermix 60+ 工具中该用哪个、传什么参数；②技能段是"熟悉主流语言惯用法"式空话，无可执行细节；③注释引用旧 crate 名 `hermes-tools`（实际 `hermix-tools`）；④"审查是代码不是人"类规则与系统提示已有纪律重复注入 |
| 内置技能 md | `crates/hermix-server/src/dashboard/skills/code-review.md:28-45` | 教科书安全/性能/可维护性三级清单 | ①检查项与「实践指南」节内容重复（28-45 vs 64-68），同一文件说两遍；②提到 Clippy/ESLint/SonarQube 但没说何时调、什么参数、结果怎么读；③无反例、无"何时不该用我"；④触发词是字面枚举（"审查代码"/"code review"），换一种说法就匹配不上 |
| 域指令 | `crates/hermix-intelligence/src/domain_mode.rs:134-168` | 5 个域各 5 条编号原则，约 150 字 | ①无一条具体命令（Code 域没说 cargo check / tsc / 测试怎么跑）；②无工具名（"先读文件"没说用 `read_file` 带 offset/limit）；③对比：Claude Code 的编码纪律是数百行、含具体工具与边界条件 |
| 工具描述 | `crates/hermix-tools/src/tools/terminal.rs:67`、`tools/delegation.rs:28` | 单句英文 description | 缺"何时不该用我 / 参数纪律 / 与相邻工具的边界"三要素；工具间分流规则被迫堆在系统提示 `TOOL_USAGE_GUIDANCE`（`directions.rs:26-71`）而不是工具自身 |
| 子代理角色 | `crates/hermix-agent/src/sub_agent_orchestrator.rs:161-238` | 三个角色（General/Explore/Plan）有工具白名单，无差异化系统提示 | 角色只差一行 description；Explore 角色的"如何搜索而非通读"策略没有随角色注入——只对子代理全体注入一份通用自卫块（969-1047） |
| 思考/域检测 | `crates/hermix-intelligence/src/thinking.rs:1-31` | Quick/Deep 只定义路由信号 | Deep 模式承诺的 self-critique 步骤无对应提示词文本，是注释级设计 |

### 张力实例（文本之间未对齐）

- CODE_DOMAIN_DIRECTIVE 说"展示修改用 diff 格式"；代码审查专家提示说"可附最小代码
  片段"——同一会话两处格式指令并存，模型行为取决于注入顺序。
- 技能 md 的「输出格式」代码块（如 debugging-methodology.md:51-66）与系统提示
  `REPORT_SECTIONING` 的分节规则各自表述，措辞不一致。
- `domain_mode.rs` 文档注释自述曾瘦身删除 `priority_tools` 等字段及消费方——
  域模式目前只改提示词不改工具面，改写的域指令应同时规划工具面接线（另立笔记）。

## 开源对照（每项 3+，标注可抄的写法）

改编引用均为大意转述（模式级），非逐字抄录。

### 专家 / 角色 system prompt

| 项目 | 写法 | Hermix 可抄什么 |
|---|---|---|
| **Claude Code**（Explore/Plan/code-explorer 类内置 subagent 定义） | 角色定义 = 职责边界（"You are a specialist in X, not Y"）+ 明确的"Do NOT"清单 + 输出格式契约 + 可用工具范围，全文数百行密度 | 给每个专家补"不做什么"清单与输出契约，砍掉与系统提示重复的通用纪律 |
| **CrewAI / LangGraph Supervisor** | role/goal/backstory 三元组，backstory 写"过去处理过什么类型的失败"而非头衔年限 | 把"15 年以上经验"换成"典型失败模式清单"（如审查专家列出本项目最常漏的三类缺陷） |
| **OpenHands microagents** | 每个角色包带 keyword frontmatter + 正文全是具体动作指令，无形容词 | 技能段从"能力形容词"改为"动作序列" |

### 技能 SKILL.md

| 项目 | 写法 | Hermix 可抄什么 |
|---|---|---|
| **Anthropic Agent Skills 官方写作指南** | description 用第三人称、必须含"做什么 + 何时用"且带触发词；正文渐进披露（先流程后细节）；明令避免空洞导语 | 重写 14 个技能的 description 为"做什么 + 何时用 + 不适用"三段；正文砍重复节 |
| **Claude Code 内置技能（pdf/xlsx/docx）** | 技能正文 = 工作流步骤 + **随附可执行脚本与确切调用命令** + 失败时的替代路径 | 每个技能加「工具接线」节：用哪个工具、什么参数、失败走哪条路（如 code-review 写明先跑 `cargo clippy` / `tsc --noEmit` 再人工清单） |
| **OpenHands microagents 仓库** | 每条规则短、具体、带反例（"Don't run `pytest` without `-x` on this repo"级别） | 检查清单每项配一个反例；删除与「实践指南」重复的段落 |

### 域/模式指令

| 项目 | 写法 | Hermix 可抄什么 |
|---|---|---|
| **Claude Code 编码纪律段** | "Read before edit / verify after change / diff not full file / don't commit unless asked"，每条附工具级动作与例外条件 | Code 域指令逐条落到工具动作（read_file 带 offset、改后跑什么验证命令、Tirith 会拦什么） |
| **Aider `prompts/base_prompts.py`** | "Act as an expert software developer…"之后立即是 edit format 的**语法级示例**（search/replace 块长什么样） | 域指令附一个最小正确示例 + 一个最小反例，而非纯原则 |
| **Codex CLI base instructions** | 输出纪律与环境 payload 分离；"personality" 段极短，纪律段极具体 | 域指令只写纪律，语气类内容全部留在 SOUL/人格层，避免重复注入 |

### 工具 description

| 项目 | 写法 | Hermix 可抄什么 |
|---|---|---|
| **Claude Code `Bash` 工具** | 三要素齐备：做什么（Executes a given bash command…）+ 何时不该用（"Avoid using this to run `find`/`grep`/`cat` — use dedicated tools"）+ 参数纪律与超时行为 | terminal 描述补"不要用它做 read_file/grep 能做的事"与输出上限行为 |
| **MCP 工具注解规范** | readOnlyHint / destructiveHint / idempotentHint 标准化语义注解 | 用统一注解词汇替换各工具自由发挥的 RiskLevel 文字描述（RiskLevel 机制保留，描述词汇对齐） |
| **Cline system prompt 的 TOOL USE 节** | 每工具一节：说明 + parameters 表 + 一个完整 usage 示例（XML 块）+ guidelines | delegate_task / compact_tool_chain 等复杂工具的 schema description 附一个完整调用示例 |

### 子代理角色提示

| 项目 | 写法 | Hermix 可抄什么 |
|---|---|---|
| **Claude Code subagent 的 Explore 类** | "Findings, not dumps"：规定返回物是路径+证据行号+结论，禁止贴大段源码 | Explore 角色注入专属返回契约（Findings-only），与结果 8KB 帽呼应 |
| **OpenAI Swarm handoff 文档** | agent 间传递的是结构化函数结果而非自由文本 | Plan 角色注入结构化计划格式（目标/步骤/验证/风险四字段） |
| **Cline Plan/Act 双模式提示** | Plan 模式明确"只读+产出计划文本"，禁改文件 | 与现有工具白名单对齐，把白名单**理由**写进角色提示（为什么不给 write） |

## 写作规范（所有模型可见文本统一执行）

1. **三要素 description**：每个专家/技能/工具的描述必须含「做什么 + 何时用（含
   触发词）+ 何时不用/不适用」。第三人称。
2. **规则必须具体到工具动作**：禁止"先确认再操作"式空话；写成"先 `read_file`
   带 offset/limit 确认当前内容，再 `edit`"。每条规则附一个最小正例或反例。
3. **零重复注入**：与系统提示已有块（REPORT_SECTIONING、TOOL_FAILURE、Git 安全、
   文件生命周期）重叠的纪律一律不重写，只写「遵循系统提示的 X 规则」；文本间
   格式指令冲突时以 `directions.rs` 块为准。
4. **工具接线节**（技能与专家专用）：列出本技能/角色应使用的工具、关键参数、
   失败时的替代路径；没有接线就删掉工具罗列（现状的 Clippy/ESLint 罗列）。
5. **长度预算**：description ≤ 2 句；域指令 ≤ 400 字（预算换具体性，不换篇幅）；
   技能正文 ≤ 150 行且无重复节。改写前后跑 token 对比，注入成本不得显著上升
   （见 Consequences）。

## 改写示例（可直接采用）

### 示例 1：`code-review.md` 技能（节选改写）

```markdown
---
name: "code-review"
description: "对已写代码做分级审查：先跑项目自带的静态检查工具，再按正确性>安全>可维护性人工复核，产出带文件:行号与修复建议的报告。用于用户要求审查/复查/评估刚写完或指定的代码。不适用于：从零设计新代码（用 architecture-design）。"
---

# 代码审查

## 工具接线（先机检后人审）
1. `terminal` 跑 `cargo clippy --workspace --all-targets 2>&1 | tail -50`（Rust）
   或 `./node_modules/.bin/tsc --noEmit`（TS）。工具零输出 = 该项通过，不要口头复述。
2. `read_file` 带 offset/limit 分段读目标代码，单次 ≤ 500 行；> 1500 行的文件
   改走 `delegate_task`（Explore 角色），把审查清单作为 task 传下去。
3. 只报机检和人工都确认的问题；机检已报的不重复定性，只补影响面。

## 检查清单（每项先想反例再下结论）
- 正确性：边界条件、并发/资源泄漏、错误路径是否吞掉失败状态
  （反例：把 `unwrap_or_default()` 当成功路径上报）。
- 安全：注入面、越权、密钥硬编码。安全项必须拦，不许降级为"建议"。
- 可维护性：只报会真实坑下一个人的问题，不凑风格分。

## 输出契约
按严重度分三级，每条 = 文件:行号 + 为什么是问题 + 最小修复方向。
结尾一句总评（可合并/需修改/需讨论）。整体报告遵循系统提示的 REPORT_SECTIONING 分节规则。
```

### 示例 2：`CODE_DOMAIN_DIRECTIVE`（改写）

```text
# 代码开发模式
1. 改前先读：任何编辑前用 read_file 看当前内容（> 1500 行用 offset/limit 分段）；
   凭记忆改代码是本项目最高频返工原因。
2. 改后即验：Rust 改动跑 cargo check --workspace --all-targets；TS 改动跑
   node_modules/.bin/tsc --noEmit；有对应测试就补跑最小测试集。验证失败不得报告完成。
3. 展示用 diff：给用户看修改时用 diff 片段，禁止重贴整个文件。
4. 不自动 commit：除非用户明确要求；git 安全规则以系统提示 git_safety 块为准。
5. 禁止写入源码树的临时文件：脚手架去 %TEMP%（系统提示 file_lifecycle 块为准）。
```

### 示例 3：`terminal` 工具 description（改写）

```text
Execute a shell command in the user's workspace (Windows, unified PowerShell layer).
Use for: building, running tests, git operations, and anything the structured tools
cannot do. Do NOT use for: reading files (read_file), searching (search_files),
moving/copying (move_file) — purpose-built tools handle those with correct argument
validation. Output is capped; unbounded dumps (`dir /s /b`, raw logs) are blocked —
pipe through `Select-String`/`head` first. Commands flagged by the approval engine
show a confirmation card you cannot bypass.
```

## 内置技能增删清单（排行榜依据）

两份榜单交叉验证（均为 2026 年数据）：

- **ClawHub（OpenClaw 生态）2026 Top10**，按下载量：self-improving-agent 31.1万、
  Summarize 21.1万、Agent Browser 17.1万、Skill Vetter 15.7万、Ontology 13.4万、
  Gog 13.4万、GitHub 13.4万、Proactive Agent 12.2万、Self-Improving 11.8万、
  Weather 11.5万（数据截至 2026-03，来源 ClawHub 官方 API）。
- **AgentSkills.to Trending**（开发向市场）：anthropics 官方 PPTX（19K 装）、
  XLSX（18K）、MCP-BUILDER（16K）霸榜； awesome-claude-skills 类合集共识
  「xlsx 数据处理是最常用办公技能，没有之一」，高频推荐 docx/pdf/pptx/xlsx、
  webapp-testing、mcp-builder、skill-creator、find-skills。

### 删（对开发/办公无增量价值）

已验证 `crates/` 下 Rust 代码无对这些技能名的硬引用（技能由加载器扫描目录发现，
删除/改名不产生断链；`document-authoring` 在 `directions.rs:37` 被引用，本清单
不动它）。

| 技能 | 处置 | 理由 |
|---|---|---|
| `web-novel` | **删**（转社区可选包） | 网络小说创作不在开发/办公场景内，且写作类需求已由 `document-authoring` + 写作专家覆盖 |
| `code-craftsmanship` | **删**（要点并入 code-review） | 泛风格说教，与 code-review/tdd-practice 三者重叠；榜单上无同类独立技能，说明社区也不把它当独立场景 |
| `devops-practice` | **删** | Hermix 是 Windows 单机产品，无部署/集群场景；保留会持续产出无操作细节的泛清单（本笔记禁止的形态） |
| `api-design` / `database-design` / `frontend-architecture` / `architecture-design` | **四合一**：合并为一个 `architecture-design` | 同为教科书式设计原则，四份重复维护；榜单设计中无按层拆分的先例，Anthropic 官方技能按「产物类型」拆分（pptx/xlsx），不按学科拆分 |

### 增（榜单验证的高频需求，对齐 Hermix 已有工具）

| 新技能 | 榜单依据 | Hermix 已有基建（技能只需补"怎么用"的纪律层） |
|---|---|---|
| `summarize-research` | Summarize 21.1万下载（#2） | `markitdown`（PDF/Office/网页转 md）+ `delegate_task` 隔离长文档读取 + `web_extract`；缺的是"多文档对比摘要、引用来源标注"的流程纪律 |
| `spreadsheet-analysis` | XLSX 18K（开发榜#2）、awesome 办公类第一 | `markitdown` 读表格 + `code_execution`（Python）做透视/合并/校验 + `write_file` 产出；缺的是"读 schema→验数据→用代码改而非手改"的操作纪律 |
| `github-workflow` | GitHub 13.4万（#7），开发刚需 | `terminal`（gh CLI）+ 系统提示 GIT_SAFETY_GUIDANCE；缺的是 Issue/PR/CI 的标准操作序列与禁止事项清单 |
| `self-improvement` | self-improving-agent 31.1万（#1）+ Self-Improving 11.8万（#9），合计超 43万下载，榜单最大趋势 | `skill_manage` patch（Improvement Feedback 机制已存在）+ `flush_memories` + salience 打分；缺的是"任务复盘→错误归因→沉淀为 patch/memory"的触发与格式规范 |
| `skill-vetting` | Skill Vetter 15.7万（#4），安全意识趋势 | `hermix-skills` 的 validator（结构）+ guard（危险模式/URL）已实现；缺的是把审计过程暴露为用户可调用技能（装前审查第三方 SKILL.md） |
| `browser-automation` | Agent Browser 17.1万（#3） | `browser_navigate/click/type/snapshot` 工具集已实现 + WEB_AUTOMATION_GUIDANCE 能力地图；缺的是"登录态、表单、等待、失败恢复"的操作纪律沉淀 |
| `pdf-processing` | anthropics 官方 pdf 技能是 awesome-claude-skills 常驻推荐（Claude Code 内置技能带 merge/split 处理脚本） | `read_pdf`（pdftotext/lopdf/pdfminer 三层 fallback）+ `markitdown` + `read_image`（扫描件走视觉模型）；缺的是"文本型 vs 扫描件分流、合并/拆分/加密操作序列、页数多走 delegate"的纪律层——分流规则目前孤悬在 `TOOL_USAGE_GUIDANCE`（`directions.rs:33`），应下沉至此 |
| `image-workflow` | 生成/编辑图像是榜单趋势常客（AgentSkills 新上架区 FLUX-IMAGE / AI-IMAGE-GENERATION 等；Claude Code 生态同类高热） | `gpt_image`（文生图 + 参考图编辑）+ `image_gen` + `file_converter`（格式转换）+ `delegate_task`（批量隔离）；缺的是提示词结构纪律、编辑用参考图而非盲改、批量 3-5 张分批的流程规范 |

净变化：14 → 16（删 3、四合一 -3、增 8）。所有新技能按本笔记写作规范
（三要素 description、工具接线节、零重复注入）编写，不新增工具、不新增依赖。

### 已有但须明确的办公/文档覆盖

- **PPT / 报告 / 周报 / 数据表格**：由保留技能 `document-authoring` 覆盖
  （画布流式渲染 + 版面规范 + 落盘 + impeccable 质量门 + 反例，是现有 14 个
  技能中唯一达到本笔记写作规范的）。故不在"增"清单重复立项；批次 1 重写时
  按规范做增量复检即可。
- **二进制 Office 能力盘点（2026-10-17 已查证）**：读取与转出两条链路存在——
  `markitdown` 读 .docx/.pptx/.xlsx → Markdown（`directions.rs:34` 分流引用）；
  `file_converter` 支持 Word/Excel/PPT → PDF、PDF → 图片、图片 → PDF
  （`tools/file_converter.rs:52-80`）。**生成可编辑 .pptx/.docx 的链路不存在**：
  全 crates 无 python-docx / python-pptx / openpyxl 引用，`code_execution` 未绑
  Office 生成库；`document-authoring.md:77-79` 明令禁止硬造打歪的二进制 Office
  冒充交付。若用户高频要求可编辑 Office 产物，正确路径是接生成管线——属
  工具面机制决策，另立笔记：见
  [2026-10-17-binary-office-export-pipeline.md](2026-10-17-binary-office-export-pipeline.md)。
- **PDF 与图片**：此前"增"清单遗漏，上表已补 `pdf-processing` 与
  `image-workflow` 两项——工具面完整（`read_pdf`/`gpt_image`/`image_gen`/
  `file_converter`），纪律层为零，是"有工具没章法"最典型的两块。

### 明确不跟榜的项

- `weather`（#10）：Hermix 无气象工具面，纯提示词技能是幻觉温床。
- `gog` / Google 全家桶（#6）：需要外部凭证与集成，违反"不感知外部资源"禁区。
- `proactive-agent`（#8）：Hermix 的 automation/cron 机制已覆盖，无需技能层重复。
- 营销类（launch-strategy / email-sequence）：不在开发/办公目标场景。

## 分批落地

- **批次 1（已落地 2026-10-17）**：删 `web-novel` / `code-craftsmanship` /
  `devops-practice`；api-design/database-design/frontend-architecture 三技能
  并入 `architecture-design`（0.2.0）；`code-review`/`debugging-methodology`/
  `tdd-practice`/`plan-writing` 按写作规范重写；`custom-app-development`/
  `impeccable-design`/`document-authoring` 审计为已合规（custom-app 审计中
  修掉对已删技能的两处引用）；`builtinExperts.ts` 旧 crate 名引用**待改**。
- **批次 2（已落地 2026-10-17）**：新增 8 个技能——summarize-research、
  spreadsheet-analysis、github-workflow、self-improvement、skill-vetting、
  browser-automation、pdf-processing、image-workflow——全部只写纪律层，
  零新工具零新依赖。技能目录现为 16 个。
- **批次 3（已落地 2026-10-17）**：terminal / delegate_task / markitdown /
  read_image 四个工具 description 按三要素升级（何时不该用 / 参数纪律 /
  输出行为）；`compact_tool_chain` 审计为已合规（原文已含意图/边界/反例并
  有单测锁定），未动。`TOOL_USAGE_GUIDANCE` 的 per-tool 分流段删减为一条
  指针（规则下沉到工具 description），保留 delegate 纪律与 Tirith 段。
- **批次 4（已落地 2026-10-17）**：4 个域指令按工具级动作模板重写（每条
  落到具体工具与验证命令）；域模式工具面接线落地为姊妹笔记
  [2026-10-17-domain-mode-toolset-wiring.md](2026-10-17-domain-mode-toolset-wiring.md)。
- **批次 5（已落地 2026-10-17）**：`builtinExperts.ts` 全部 8 位专家提示
  升级（头衔话术 → 失败模式清单 + 工具接线 + 输出契约）；子代理
  Explore/Plan 角色注入专属返回契约（`SubAgentRole::return_contract`，
  Explore = Findings-not-dumps，Plan = 结构化计划四段），General 不变。

每批次验收：`cargo check --workspace --all-targets` ✓、
`cargo test --workspace --lib` ✓（约 1800+ 用例，新增域偏置/角色契约测试
全绿；`hermix-environments` 5 个沙箱测试为预存环境失败，该 crate 零改动）、
`tsc --noEmit` ✓、token diff 见写作规范第 5 条口径（主提示净减约 140 token，
4 个高频工具 schema 合计约 +310 token，单域激活的域指令 +约 130 token）。

## Alternatives considered

- **只改专家不改技能/工具**：专家只在会话内注入，影响面小；技能与工具描述
  每轮路由都参与匹配，性价比更高。否决"只改一处"。
- **用 LLM 批量生成改写**：质量不可控且会引入新的泛话术；本笔记先钉死规范与
  示例，人工（或人审机改）逐批执行。否决全自动生成。
- **把所有规则堆进系统提示而非下沉到工具/技能**：系统提示按 token 付费且
  prefix-cache 敏感（见 `directions.rs` workbench 注释的教训）；规则应内聚到
  消费点。否决继续膨胀 directions.rs。
- **双语化（中英对照）所有技能**：维护翻倍，模型读单语无障碍；description
  保留英文触发词混排即可。否决全文双语。

## Consequences

- **token 成本上升**：三要素 description 与示例会让单工具 schema 变长
  （估算 +50~150 token/工具 × 高频 5 工具，域指令 +200 字）。换来的是错误路由
  与返工轮次下降；批次 2 的 `TOOL_USAGE_GUIDANCE` 减肥对冲部分增量。
- **维护面扩大**：提示词从"代码里的字符串"变成需要评审的资产；本笔记的写作
  规范即评审清单。技能 md 今后按本规范审，避免回到重复节状态。
- **放弃的东西**：放弃"极简一句式工具描述"的整洁观；放弃域指令的纯原则风格。
- **不改机制**：本笔记只动文本；DirRoute 门控、渐进加载、审批引擎一律不动。
- 域模式工具面接线、技能语义检索为独立决策，另立笔记，避免一篇提案背多个决策。

---

## 附：机制层对比摘要（2026-10-17 全量审计结论）

机制层属第一梯队的项：5 层上下文 + 3 层缓存（对标 Letta/Claude Code）、
路由门控 + full/short 两态指引块（精于 Cline/OpenHands）、MiniMax 纠偏 +
流式 redactor（细于多数竞品）、子代理结果帽/lineage/防再委托（细于
CrewAI/LangGraph）、Tirith 参数形状预检（同类少见）、记忆排除清单
（"Code is live, memory is static"，优于多数开源的记忆写入纪律）。

机制层已记录的差距（另行处理，不属本篇）：MCP 执行面未接线、模型家族子串
判定脆弱、域模式无工具面消费方、技能关键词匹配无语义检索、自动化无重试/
投递闭环、扩展机制三轨并行。这些各自的落地若需要，按笔记制度单独立项。
