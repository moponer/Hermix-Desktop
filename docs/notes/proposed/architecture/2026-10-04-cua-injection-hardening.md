# Decision Note: CU / 浏览器 注入面防护方案

Status: implemented — 2026-10-04 同日落地（四项修法全部实施）。**本篇初稿有一处误判已修正**：事实 4 声称 `browser_navigate` 的 url「零过滤直达 Page.navigate」——**不成立**。`BrowserNavigateHandler` 默认构造（`browser.rs` 注册点用 `new()`）就带 `SsrfGuard`，其 scheme gate 只放行 http/https（`ssrf.rs`），`javascript:`/`file:`/`data:` 在 handler 层已被拒，且有测试钉着（`test_browser_navigate_blocks_file_scheme` 等）。这是「只看 cdp.rs 底层没追 handler 入口」的反向误判——与上一轮 `cua_ui_click` 误判（有防护没看见/没防护看见了）同族。**真实的修 1 缺口是两条**：① `browser_ptc` 的 Navigate 步骤直连 backend 绕过 SsrfGuard（审批挡不住已批准程序内的 `javascript:` URL）；② `about:blank` 无 host 被 guard 的 "URL has no host" 误杀（自动化合法起点）。

## 核实过程：差距 B 的表述需要更正

上一轮差距分析写「`cua_ui_click` 收模型编造的 `auto_id`，无签发校验」。**逐条核实后，这个描述与代码不符**：

| 上一轮说法 | 代码实际 | 结论 |
|---|---|---|
| `cua_ui_click` 收 `auto_id` | schema 只有 `name`（`cua.rs:1316`），走 `uia::click_by_name` | ❌ 不成立 |
| `cua_ui_find` 收 `auto_id` | 确实收（`a11y` 无关，`cua.rs` 的 `automation_id` 参数） | ✅ 成立，但**只读不写** |
| CU 存在"签发"概念 | 全仓无任何 ref 签发/校验机制 | ❌ 不存在这个概念 |

`cua_ui_find` 的 `automation_id` 只做 `find_all` 查询，**不发任何输入**。所以 CU 侧目前**没有可被利用的注入写入面**——差距 B 描述的攻击面在 CU 侧是空的。

**但真正的注入面在浏览器侧，而且比差距 B 说的严重。** 逐条核实如下。

## 真实的注入面

### 事实 1：`[ref=eN]` 句柄无签发、无过期

`a11y.rs:94-146` 生成句柄：

```rust
let mut ref_counter: u32 = 0;
// ...
if interactive {
    ref_counter += 1;
    let ref_id = format!("e{ref_counter}");
    refs.insert(ref_id, selector);
}
```

`resolve_selector`（`cdp.rs:76-88`）解析：

```rust
if let Some(rest) = trimmed.strip_prefix("[ref=") {
    if let Some(ref_id) = rest.strip_suffix(']') {
        if let Some(sel) = map.get(ref_id) { return sel.clone(); }
    }
}
trimmed.to_string()   // ← 查不到就原样返回
```

三个问题：

1. **计数器可预测**——`e1`、`e2`、`e3` 顺序递增，无随机分量、无 session 绑定。模型若见过一次快照，就知道下一次快照的句柄大致范围。
2. **无过期**——`last_snapshot_refs` 是 `Arc<Mutex<HashMap>>`（`cdp.rs:66`），每次 snapshot 整体覆盖，但**页面导航后不失效**。`browser_navigate` 到一个全新页面后，旧句柄仍在 map 里。
3. **查不到就降级为 CSS 选择器**——这是最关键的一条。`[ref=e999]`（不存在的句柄）**不会被拒绝**，而是原样传给 CDP 当 CSS 选择器执行。

第 3 条的实际后果：模型输出 `[ref=e5]` 但 map 里没有 `e5` 时，这个字符串会以 CSS 选择器语义送进 `Runtime.evaluate`。它匹配不到任何元素，**不会误伤**——但错误信息会以 `not_found` 形式返回（`cdp.rs:673`），模型据此重试。

**所以纯 ref 路径的注入后果有限**（最坏是找不到元素），但下面这条不是。

### 事实 2：selector 字符串拼接只转义单引号，**反斜杠未转义**

`a11y.rs:10-17`：

```rust
fn ax_node_selector(role: &str, name: &str) -> String {
    if name.is_empty() {
        format!("[role=\"{}\"]", role)
    } else {
        format!("[role=\"{}\"]:has-text('{}')", role, name.replace('\'', "\\'"))
    }
}
```

`role` 与 `name` 都直接来自页面（`Accessibility.getFullAXTree` 的节点属性），**页面完全控制这两个字符串**。只做了 `name.replace('\'', "\\'")`。

一个页面只要让某个可交互元素的 accessible name 含 `\` + `'`，就能让生成的 selector 提前闭合、注入额外 CSS。例如 name 为 `a\'[role="button"], script`，生成的 selector 是：

```
[role="button"]:has-text('a\'[role="button"], script')
```

配合 `querySelectorAll` 的解析规则，这可能匹配到预期外的元素。

**严重性评估：有限。** 后果是「点到另一个元素」，而不是「执行任意代码」——因为这段字符串最终只进 `querySelectorAll`，不进 `eval`。但「点到另一个元素」在自动化场景里等价于「按错了按钮」：删文件、确认支付、发消息，都可能因为 selector 偏移而命中错误目标。

**这是本方案的第一优先项。**

### 事实 3：`browser_ptc` 的 `Js` 步骤是页内任意 JS

`browser.rs:146-152` 的 `PtcStep::Js { expression, store }` 直接把 LLM 提供的字符串送进 `Runtime.evaluate`。

注册档是 `Dangerous`（`browser.rs:110`），运行时逐步骤判定。**这符合设计预期**——用户审批一次，就授权了整段程序。

### 事实 4：`browser_navigate` 的 URL 无 scheme 校验（**已核实为误判，见下方更正**）

> **更正（实施时发现）**：这一节的前提不成立。`BrowserNavigateHandler` 默认构造（`browser.rs` 注册点用 `new()`）就带 `SsrfGuard`（`browser.rs:775` 附近的前置校验），其 scheme gate **只放行 http/https**（`ssrf.rs:104`），`javascript:` / `file:` / `data:` 在 handler 层已被拒，且早有测试钉着（`test_browser_navigate_blocks_file_scheme` / `test_browser_navigate_blocks_cloud_metadata`）。「零过滤」的判断来自**只看了 cdp.rs 底层的 `Page.navigate`，没追 handler 入口**——与上一轮 `cua_ui_click` 误判同族，方向相反。初稿写本节时没有跑过一条真实的 `javascript:` 导航来复现，这是方法论教训：**"已核实"三个字要配得上，至少得有一条能跑通/跑失败的证据链**。
>
> 修 1 的真实缺口变成两条（实施时发现）：
> 1. **`browser_ptc` 的 Navigate 步骤直连 backend，绕过 SsrfGuard**——审批挡不住已批准程序内的 `javascript:` URL，且审批卡当时不显示步骤（事实 5），用户无从发现自己批了什么。
> 2. `about:blank` 无 host，被 guard 的 "URL has no host" 拒绝——它是浏览器自动化的合法起点，属于误杀。

以下为初稿原文（保留以记录误判现场）：

`cdp.rs:388-391`：

```rust
let (result, event) = self.cdp_command_with_event(
    "Page.navigate",
    json!({"url": url}),   // ← LLM 提供的 url 原样进 CDP
    Some("Page.loadEventFired"),
    wait_for_load,
).await?;
```

`url` 从 handler 到 `Page.navigate` 之间**没有任何过滤**。`file://` 可读本地文件，`javascript:` 可直接执行页内脚本。

`browser_navigate` 注册档是 `Safe`（`browser.rs:63` 附近，8 个浏览器工具里除 `browser_ptc` 外全是 `Safe`）——**即默认不弹审批**。这是本篇严重性最高的一项：`javascript:` URL 在 `Safe` 档下静默执行。

### 事实 5：`browser_ptc` 的审批卡**不显示脚本内容**（已核实）

`approval_diff.rs:20-53` 是**形状驱动**的（`let _ = tool;` 明确注释"dispatch is shape-based, not name-based"）——它只认三种形状：`old_string`+`new_string`（edit）、`content`+绝对 `path`（write）、以及它们的变体。

`browser_ptc` 的参数是 `{ steps: [...] }`，**顶层没有任何一个字段匹配上述形状** → `approval_diff` 返回 `None` → 前端走 fallback。

前端 `ApprovalBatchBubble.tsx` 的 fallback 实际展示：
- ② Verb row：`toolVerb(dominantTool, args)` —— 一个动词
- ③ File block：`extractPath(args)` —— `browser_ptc` 无路径字段，**不渲染**

**结论：用户看到的是一张只有「工具调用 · 危险 · 1 项」+ 一个动词的卡片，点「允许」时看不到将要执行的那段 JavaScript。**

这与注入防护同源——注入攻击的最后一环通常就是「让用户批准一段自己没读过的脚本」。而这里连「读」的位置都没给。

## Decision

### 修 1（安全，最高优先）：`browser_navigate` 加 URL scheme 白名单

**实施（2026-10-04，commit `4c3d1c50`）**：与方案的差异——没有新建白名单，因为 `SsrfGuard` 的 scheme gate 已经是那个白名单。实际落地：
- 抽 `check_navigable_url()` 共享函数（`tools/browser.rs`）：精确放行 `about:blank`（大小写不敏感），其余过 `SsrfGuard`。
- `browser_navigate` 的 guard 分支换用该函数（保留 `disabled()` 测试旁路语义）。
- **`browser_ptc` 的 `PtcStep::Navigate` 步骤接同一道闸**——真旁路封死，新增 2 测试（js 拒 + about:blank 放行）+ navigate handler 侧 2 条新测试（`javascript:` 拒 / `about:blank` 放行）。
- `file://` 提 Caution 档的建议**未做**：`file:` 已被 guard 拒绝（`ForbiddenScheme`），本地 HTML 预览用例若真出现，届时再按"放行 + Caution 档"立项，不为当前不存在的用例先加档位。

**问题（初稿判断，已被上方更正取代）**：`url` 无过滤直达 `Page.navigate`，工具档是 `Safe`（默认不弹审批）。

**方案**：在 `BrowserNavigateHandler::execute` 入口拒绝非白名单 scheme。最小白名单：`http`、`https`、`about`（`about:blank` 是浏览器自动化必需）。

`file://` **需要单独决策**，不能一刀切禁掉：本地 HTML 预览/测试是真实用例。建议——`file://` 保留但**提到 `Caution` 档**（弹一次审批），其余非白名单一律拒绝。`javascript:` / `data:` 直接拒绝，无例外。

**为什么不能只靠注册档**：`Safe` 档在 `local_agent_service.rs:1299` 直接 `Allow`，连 Tirith 都不过。改档只是把「每次都问」变成「每次都问」，不解决 `javascript:` 的性质问题——**它压根不该被执行，不是该不该审批的问题**。白名单是入口校验，档位是用户体验，两回事都要。

**改动量**：handler 入口一个 `match` + 单测。

### 修 2（安全）：selector 构造改为转义完备

**实施（2026-10-04，commit `e756572a`）**：`ax_node_selector` 重写为 `escape_css_quoted` 辅助函数——先中和控制字符（`\r\n`→空格、`\0`→U+FFFD，裸换行会让 CSS 字符串字面量语法错误），再转 `\`、再转引号（`role` 用 `"`，`name` 用 `'`）。5 条单测含变异测试（旧实现对 `a\'b` 会产出可逃逸的 selector，新实现断言精确输出）。方案原文：

**问题**：`ax_node_selector` 只转义单引号，页面可借反斜杠逃逸（`a11y.rs:10-17`）。

**方案**：转义顺序必须是**先反斜杠、后引号**，且 `role` 同样要转义（它也来自页面）。

正确顺序：
```
\ → \\
' → \'
```
先转 `\`（否则第二步产生的 `\` 会被再次转义），再转 `'`。

**为什么不用 `serde_json::to_string`**：那条路产出的是 JSON 字符串字面量（带双引号），不是 CSS 选择器里的字符串。CSS 单引号字符串的转义规则是 CSS 自己的，只能手写。**但可以顺带禁掉控制字符**——`name` 里的换行/空字符会让 selector 在某些引擎上解析异常。

**改动量**：`ax_node_selector` 一个函数 + 单测。

**验证**：单测断言 `name = "a\\'b"` 时输出不含未转义的单引号。这条必须做变异测试（构造一个当前实现会失败的输入）。

### 修 3（权限）：`[ref=eN]` 加签发校验，查不到即拒绝

**实施（2026-10-04，commit `60b18c58`）**：按方案取 b+c，**未做随机盐 a**（六步阶梯第 1 条：不为尚未发生的攻击建机制）：
- `resolve_selector` 签名 `String → Result<String, ToolError>`：`[ref=…]` 形态查不到返回明确错误（文案带「页面可能已导航，请重新 browser_snapshot」），纯 CSS 选择器原样通过。
- `navigate_with_wait` 成功后清空 `last_snapshot_refs`（修 3b）。
- 三调用点（click/type/highlight）`?` 传播；已核实 `resolve_selector` 全仓仅 `CdpBrowserBackend` 一处实现，无其他调用方。
- 2 条新单测：未知句柄拒绝（含「请重新 snapshot」文案断言）+ 清空后旧句柄必报错。

**问题**：计数器可预测 + 不过期 + 查不到降级成 CSS。

**方案（三件事，缺一件都不完整）**：

a) **句柄加随机盐**。`ref_id` 从 `e{counter}` 改为 `e{counter}_{salt}`，`salt` 是 backend 实例构造时取的随机 u32。模型无法从旧快照推出新快照的句柄。

b) **导航即失效**。`browser_navigate` 成功后清空 `last_snapshot_refs`。理由：跨页面的句柄本来就无意义，保留只会让模型误用。**这是最小且最有效的一条**——同时解决「过期」和大部分「可预测」的实际危害。

c) **`[ref=...]` 形态查不到时直接报错，不再降级**。降级成 CSS 只对「模型本来就想写 CSS」有意义，而那种情况模型应该直接写 CSS（schema 允许）。把 `[ref=` 前缀视为**明确的句柄请求**，请求失败就明确失败。

**为什么不全做**：a 单独做收益有限（模型可以自己 snapshot 一次拿到新句柄，但那需要一次额外工具调用，恰是 b 要挡的场景）。b 单独做最省事。**建议 b + c 优先，a 作为加固**——六步阶梯第 1 条，不为尚未发生的攻击建机制。

**注意副作用**：c 改完后，模型拿到过期 `[ref=e5]` 会得到明确错误而非静默的 `not_found`。这**是想要的**——错误信息带「请重新 snapshot」提示（`cdp.rs:673` 已有该文案，复用）。

**已核实**：`resolve_selector` 全仓**只有 `CdpBrowserBackend` 一处实现**（`cdp.rs:76`）。`CamoFoxBrowserBackend`（`:1175`）与 `AutoBrowserBackend` 内部都包着 Cdp 实例，`SessionedBrowserBackend` 走 trait 转发。**修 3 只需改一处**，不必担心遗漏。

### 修 4（权限）：审批卡显示 `browser_ptc` 的完整步骤

**实施（2026-10-04，本次提交）**：按方案做在前端 fallback——`ApprovalBatchBubble.tsx` 加形状驱动的 `extractSteps`（任何 args 带 `steps` 数组的工具都渲染，非 browser_ptc 特例）：
- 逐项渲染：序号 + op 中文标签（navigate→导航…js→执行 JS）+ 关键参数摘要。
- **`js.expression` / `fill.text` 是用户批准的载荷本体——全文展示不截断**；>120 字符用 `<details open>`（默认展开）+ 字符数标注，短载荷直接 `pre`。
- `js` 步骤的 op 标签用 `--color-error` 警示色（全卡风险最高的载荷）。
- 样式加在 `components-tool-card.css` §19（apv- 类族所在文件；全部 token 经 tokens.css 核实真实存在，`--color-danger` 不存在已改用 `--color-error`）。
- `tsc --noEmit` 全绿。

**问题**（事实 5）：用户批准时看不到脚本。

**方案**：**不扩展 `approval_diff` 的形状匹配**——它是通用机制，为一个工具加特例会污染它。改为在前端 `ApprovalBatchBubble` 的 fallback 分支里，**对 `steps` 数组字段做通用渲染**（任何工具的 args 里若有 `steps`，就展开逐项列出 op + 关键参数）。

这是「形状驱动」哲学的正确延伸：`approval_diff` 处理文件类形状，前端处理结构化参数形状，两者互补而非重复。

**要挡的注入变体**：`Js` 步骤的 `expression` 必须**全文展示、不截断**（截断的脚本等于没展示）。若超长则折叠但默认展开，并显示总字符数。

## 明确不做

- **不做 CU 侧 ref 签发**——CU 侧 `cua_ui_*` 只收 `name`，无注入写入面（见核实过程）。**为不存在的攻击面建签发机制是过度工程。**
- **不改 `browser_ptc` 的 `Js` 步骤设计**——Dangerous + 审批是正确的分层。`javascript:` URL 同理已由 `browser_navigate` 的审批覆盖（待核实）。
- **不做 selector 白名单**——限制模型能点哪些元素，等于替模型做决定，且无法穷举。不如把审批卡做对。
- **不引入能力令牌体系（ZCode 的 `hasOfficialCuaFrameAuthority` 那套）**——那是给帧数据打权威标记，防的是"坐标契约被污染"。hermix 的坐标是模型自己算的，没有这个契约，**照抄会得到一个防不存在威胁的机制**。
- **不改 `browser_navigate` 的注册档**——修 1 已把非白名单 scheme 挡在入口，剩下 `file://` 的档位调整是修 1 的一部分，不单独作为一条。

## Alternatives considered

- **把 `ax_node_selector` 换成 `serde_json::to_string` 构造选择器** — 否决：CSS 字符串字面量与 JSON 字符串字面量的转义规则不同（JSON 转义双引号，CSS `:has-text()` 用单引号）。硬套会产出语法非法的 selector。
- **用 `document.querySelectorAll` 的 `CSS.escape` 替代手工转义** — 否决：`CSS.escape` 是给**标识符**用的（类名/ID），不是给 `:has-text('...')` 的字符串参数用的。语义不匹配。
- **给 ref 句柄加 HMAC 签名** — 否决：签名要验证方持有密钥，而验证方就是发起方自己（同一个 backend 进程），**自己签自己验没有安全增益**，只是把计数器换成哈希。随机盐 + 导航失效达到了同样的实际效果，且可读性更好（错误信息里能看出句柄来源）。
- **把 `[ref=eN]` 降级路径保留但加长度/字符白名单** — 否决：`[ref=eN]` 的 N 位置只能是 `[0-9a-zA-Z_]`，白名单能挡的字符串本来也匹配不到任何元素。**拦截 `[ref=` 前缀本身比校验内容更直接。**
- **只把 `browser_navigate` 提到 `Dangerous` 档，不加白名单** — 否决：见修 1 的「为什么不能只靠注册档」。`javascript:` URL 的问题是**它压根不该被执行**，审批只是让人有机会点「否」，但每次点「是」它照样执行。**入口校验是性质问题，档位是体验问题。**
- **在 `approval_diff` 里给 `browser_ptc` 加 `steps` 特例** — 否决：`approval_diff` 是通用形状匹配器，为单个工具开特例会持续腐化（下一个多步工具再加一条）。前端做通用 `steps` 渲染，任何工具受益。
- **一并禁掉 `file://`** — 否决：本地 HTML 预览/测试是真实用例，一刀切会打断正当流程。改为保留 + 提到 `Caution` 档。

## Consequences

- **获得**：页面无法通过 accessible name 里的反斜杠偏移出预期外的 selector（修 2）；`javascript:` / `data:` URL 在入口被拒（修 1）；跨页面陈旧句柄不再静默生效（修 3b）；模型请求不存在的句柄时拿到明确错误（修 3c）；用户批准 PTC 前能看到完整脚本（修 4）。
- **代价**：`[ref=...]` 查不到时从「静默降级为 CSS」变成「明确失败」，模型偶尔会因此多走一轮——**刻意如此**，失败看得见优于看不见。`file://` 会多一次审批弹窗。
- **风险（已排除的部分）**：初稿担心「修 1 改完非白名单 scheme 被误杀——若现有用法依赖 `chrome://` 等会被误杀」。实施核实：guard 本来就在拒这些 scheme，本次只是把同一道闸补到 ptc 步骤层，**行为面零扩大**，唯一新增放行是 `about:blank`（此前被误杀，现在可用）。
- **范围**：修 1 / 修 4 跨 `hermix-tools`（后端 handler）与 `apps/desktop-tauri`（前端组件），**不再是一个纯后端批次**。修 2 / 修 3 只动 `hermix-tools`。
- **方法论**：本篇推翻了上一轮差距分析的核心论断（差距 B「CU 侧 ref 无签发校验」），因为差距分析只读了 `cua.rs` 的 schema，**没追到 `cdp.rs` / `a11y.rs`**。而真正的两个缺口（`javascript:` URL、审批卡不显示脚本）差距分析完全没看见。**注入面审计必须追到字符串拼接与审批展示这两层**——schema 层看不出转义缺陷，也看不出审批卡少显示了什么。**实施时再次自我推翻**：事实 4 的「零过滤」同样是没追 handler 入口的产物——审计要追到底层（schema→handler→被调函数→字符串拼接），初稿只追了半程。

## Reintroduction conditions

- 若将来 CU 侧引入「按签发句柄操作」的元素寻址（如 `cua_ui_click_by_ref`），修 2 的句柄机制需同步覆盖 CU。
- 若 `browser_ptc` 引入「多轮循环 + 条件分支」（即真正的程序而非线性步骤），`Js` 步骤的风险面会变复杂，届时需重新评估审批粒度。
