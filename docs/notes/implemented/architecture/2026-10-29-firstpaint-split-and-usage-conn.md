# Decision Note: 首屏/会话挂载瘦身（katex、设置页、语法包）+ token_usage 连接复用

Status: implemented — 2026-10-29 交付

## Decision

延续同日《timeline-payload-cap-and-stream-tail》，处理性能清单第 3、4 项。

### 3）首屏与会话挂载的代码量

实测（`release/frontend`，改前/改后同口径）：

| 指标 | 改前 | 改后 |
|---|---|---|
| `App-*.js`（进会话必载） | **1005 KB** | **403 KB** |
| `vendor-katex` | 512 KB，**随 MarkdownBody 静态载入** | 525 KB，**只在出现公式时载入** |
| 首屏（`index.html` 引用的全部资源） | 300 KB | 301 KB（本来就干净） |
| 长尾代码语法 | 全部并入 `vendor-highlight` 620 KB | 各自 0.5 KB 独立 chunk，按需取 |

三处改动：

1. **设置页全部懒加载**（`settings/SettingsShell.tsx`）：此前 12 个设置页静态引入，
   打开设置弹窗要付模型列表 / 诊断 / 归档 / 记忆（echarts ~1MB）的全部解析成本。
   现在逐页 `lazy()` + 单一 `Suspense`（兜底空盒子，不写字面量加载文案）。
2. **KaTeX 按需**（`MarkdownBody.MathNode`）：照 Mermaid 的既有形态改成
   `import("katex")`，取到前先渲染原文 `<code>`；CSS 仍静态（23KB，换首帧不闪）。
3. **代码语法包拆分**（`lib/codeHighlight.ts` + `vite.config.ts`）：
   - 25 个常用语法（ts/js/tsx/jsx/python/rust/go/java/c/cpp/csharp/bash/powershell/
     css/json/yaml/toml/markup/sql/diff/markdown/js-extras/js-templates）静态注册；
   - 其余 39 个（kotlin/swift/scss/lua/docker/protobuf/csv/…）进 `LAZY_LANGS` 映射，
     首次遇到才 `import()`，注册完成后 `langVersion++` 通知订阅者重渲染
     （`subscribeLangLoads` + `useSyncExternalStore`，`CodeBlockImpl` 已接）；
   - `manualChunks` 不再把 `refractor/lang/*` 收进 `vendor-highlight`——否则
     "懒加载"会退化成"先拉 616KB 再挑一个语法"。

### 4）后端每往返成本

- `persist_token_usage` 此前**每轮 LLM 往返**都 `open_with_wal` 一次（`Connection::open`
  + 重设 `journal_mode` / `busy_timeout`，Windows 上 1–3ms），且压在 async 路径上。
  新增 `session_persistence::write_conn(path)`：进程内按 db 路径缓存**一条写连接**
  （`Mutex` 串行写；读路径仍各自 `open_with_wal`，互不阻塞）。一次 run 十几次往返
  省下几十毫秒。
- **未做**：`provider.rs` / `prompt_assembly.rs` 每次往返 2–3 次整段会话深拷贝。
  评估后不做：单次省 ~0.2–0.4ms（400 条消息量级），而这三个 pass（孤儿 tool_result、
  tool 配对邻近性、相邻 assistant 合并）是踩过 400 错误的**正确性关键路径**，
  改成 `Cow` 借用式收益与风险不成比例。若日后会话规模再上一个量级再动。

## Alternatives considered

- **把 refractor 核心也懒加载**（连 616KB 都不进会话挂载路径）：能再省一次解析，但
  首个代码块会先显示纯文本再跳成高亮；代码块在这个产品里出现频率很高，观感代价大于
  ~30ms 的解析收益。语法包拆分已经把"无人用到的 39 种语言"摘出去了。
- **设置页只 lazy 少数几个**：页与页之间没有依赖，全 lazy 的边际风险为零（已有
  MemoryTab 先例），没必要保留静态入口。
- **katex 连 CSS 一起懒加载**：会闪一帧无样式公式；23KB 换稳定首帧更值。
- **缓存读连接**：读请求分散在多个 handler，长连接可能持有过期快照且难排查；
  只给"每往返都写"的那一条路径做缓存。

## Consequences

- 打开设置弹窗只解析外壳 + 当前页；进会话的必载 JS 从 1MB 降到 400KB。
- 首次出现公式 / 冷门语言的代码块时，会先看到原文或纯文本，语法包到位后
  （本地文件，通常 <30ms）自动升级为渲染结果。取不到时保持原文，不吞内容。
- `write_conn` 让进程长期持有 sessions.db 的一个写句柄：DB 文件在进程存活期间
  不能被删除/移走（此前可以）。桌面端不存在这个场景；测试用独立 tempdir 路径，
  互不干扰。
- 验证：`cargo test -p hermix-agent --lib session_persistence` 59 项通过；
  `tsc --noEmit` 通过；前端 `vite build` 通过（拆包结果见上表）；
  DesignGuard `clean`、theme-tokens 全项 ✓、primitives 仍为存量 advisory。
