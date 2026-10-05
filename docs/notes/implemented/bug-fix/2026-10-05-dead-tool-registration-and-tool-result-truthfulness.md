# Decision Note: 死工具注册与工具结果真实性

Status: implemented — 10 个已写完但从未注册的工具接通；browser 动作不再报假成功；两条基于错误根因的"修复"被否决

## 背景

一份来自真实业务会话（登录 `sy.cssso.com` 并遍历 24 个页面）的缺陷报告列出
12 项问题，标注 4 项 P0，并估算修复后可省 >90% token。**该报告是在没有源码访问
的前提下写成的**——其 BUG-02 根因栏原文即标注"推断，无法读源码"。本轮把 12 条
逐条对源码验证后，发现其中 3 条根因方向错误、2 条指错了位置、1 条漏了一半，
并挖出报告完全未提的 10 个孤儿工具。

## Decision

**先验证、后动手。** 12 条按「真 bug / 误判 / 设计如此」分档后再改，不照单全收。
验证方法是只读审计，逐条给出 `file:line` 证据；已实现的判为 REFUTED 或
PARTIALLY CONFIRMED 并写明报告错在哪。

已实施：

1. **接通 10 个死工具。** `tool_search` / `tool_describe` / `tool_call`
   （`tools::tool_search` 从未被 `register_all` 调用，因此从未进入
   `get_definitions()`），以及 7 个 `browser_*` handler（实现、schema、自描述
   description 俱全，只是没有 `register_with`）。三重失效叠加使其不可见：
   `bridge_tool_registry` 只桥接 `get_definitions()` → `run_loop` 用
   `tool_registry.get(name).is_some()` 校验 → `tool_call_validator` 报
   `does not exist`，而 `directions.rs` 仍在宣传它们。模型看到工具、调用撞墙。
   补两条回归测试，断言 `get_definitions()` 含全部工具——旧的
   `tool_call_dispatches_to_real_tool` 用了忽略 `params` 的 `DummyHandler`，
   只验证"调用了"，结构上不可能发现这个问题。
2. **browser 动作报真实结果。** `r#type` / `click` 的页面脚本改为 IIFE + `var`
   （裸顶层 `let` 会在 realm 留持久 global-lexical binding，第二次调用必抛
   `Identifier 'el' has already been declared`）；操作数改用
   `serde_json::to_string` 编码而非 `replace('\'')` 拼接（后者是可注入的畸形 JS）；
   `status` 由硬编码 `"typed"` / `"clicked"` 改为按实际求值结果判定；
   preflight 解析失败改为 fail-closed（原来兜底成 `{"ok": true}`）。
3. **`[ref=eN]` 映射产出合法 CSS。** `ax_node_selector` 过去生成
   `[role="X"]:has-text('Y')`——Playwright 引擎专有语法，`document.querySelector`
   直接抛 SyntaxError。改用真正承载 accessible name 的属性级联。
4. **BUG-08**：`normalize_windows_path_slashes` 的 URL 豁免只认字面量 `://`，
   看不见 PowerShell 变量插值，`"$base/"` 被改写成 `"$base\"`。含 `$` 的 token
   一律视为非字面量路径。
5. **BUG-09**：凭据检测器按"字段名 + 任意非空白值"匹配，变量引用 `$env:X` 同样
   被拦。改为要求值的首字符不是 `$`。
6. **BUG-06**：`windows-core` 把 null 出参转成 `Error::empty()`，其 code 恰为
   `HRESULT(0)`，`Display` 跑 `FormatMessageW(0)` 打印"操作成功完成。"——UIA 的
   "没匹配到"信号被渲染成错误。改走 `CuaError::NotFound`。
7. **BUG-07**：`cua_ui_tree` 的 schema 从未声明 `hwnd`，分发器静默忽略未知字段，
   每次都从桌面根遍历。新增 `get_control_view_tree_from` 与 `hwnd` 参数。

## Alternatives considered

**否决一：按报告原样修 BUG-02。** 报告建议把 `arguments` 合并到参数对象顶层。
`ToolCallHandler::execute`（`tool_search.rs:440-443, 461`）本就正确提取
`arguments` 并原样下发；合并只会往目标工具参数里注入 `name` / `arguments`
两个垃圾键，让所有**正常**调用一起失败。根因是工具压根没注册，与参数无关。

**否决二：放宽 nudge 门控。** 「说计划→停」看着像 `nudge_eligible`
（`run_loop.rs:1375-1380`）门控太窄——只认自治模式 + 全程没碰过工具。查证后
发现门控是**有据可查的架构决策**：`empty_response_handler.rs:25-42` 记录了
曾有 5 层「别信 LLM 的停止」的 nudge，其中 4 层被删，理由是每层多花 1+ 次
LLM 调用；只留自治那层，因为它对应用户的显式偏好。放宽等于推翻一次记录在案的
成本取舍。改为在提示词层根治：新增 `USER_LABEL_REPLY_GUIDANCE`（裸标签先复述
完整选项再动手、决策不得与收尾配对、选项必须自描述、无唯一匹配就直问），
并把 `PLAN_BEFORE_EXECUTE_GUIDANCE` 的"同一轮发出第一个工具调用"提到显式条款
（*announcing a step is not doing it*）。

**否决三：修 BUG-12（`Remove-Item -Recurse -Force` 被拒 + 600s 审批超时）。**
查证后认定这是**有意的护栏**，不是缺陷：`approval/engine.rs:49-50` 的
`DENIED_PATTERNS` 有测试钉住，`directions.rs:376-378` 明确指定 `storage_clean`
作为替代路径；600s 超时同样是有意设计，且 scratch 范围
（`local_agent_service.rs:1494-1511`）本来就跳过审批。拆它等于给
递归强删开绿灯。真正想缩短等待的诉求应调
`hermix-config/src/config.rs:793` 的 `APPROVAL_TIMEOUT_SECS`，不碰 deny 列表。

**否决四：把 BUG-03（图像 base64 剥离）当 bug 修。** 报告称「父子 agent 双重
剥离 → 视觉通道完全死锁」。实际 `run_loop.rs:507-519` 写明这是有意的
zero-context-overhead 模型：主上下文**永不**包含图片 base64，剥离后注入 nudge
引导改用 `delegate_task`，由子 agent 的隔离上下文吸收成本——
即 AGENTS.md 的「零上下文膨胀」铁律。剥离逻辑不动。真缺陷在 nudge 靠 LLM 自觉，
属另一议题。

## Consequences

- **ref 选择精度下降。** CSS 没有文本匹配器，名字只来自文本内容的元素不再被
  `[ref=eN]` 命中。这是有意的失败模式切换：可见的 `type_failed` 错误（模型能
  重新 snapshot 脱困）优于静默命中错误节点。依赖文本内容命名的页面需要额外
  适配，未做。
- **常驻工具数增加。** 7 个 browser 工具进入注册表，会影响
  `classify_tools` 的 deferred 划分（`DEFERRED_THRESHOLD = 25`）。这是恢复
  能力的必要代价，但值得观察一轮 token 分布。
- **提示词变长。** 新增一条 always-on direction。`directions.rs` 距 god-file
  红线（2500 行）仍有余量，但方向类 block 的边际收益递减，后续新增前应先度量。
- **报告本身不可作为修复清单。** 12 条中仅约一半的根因指向正确代码位置。
  任何无源码访问环境产出的缺陷报告都应先验证再实施。
- **报告提到的 BUG-01（嵌套 runtime panic）未修。** 机制真实但触发器错误：
  `cua_screenshot` 走 `spawn_blocking` + `capture::screenshot`，不碰 CDP；
  唯一未包 `spawn_blocking` 的三个调用点（`cua_backend.rs:28/42/68`）无调用者，
  是死代码中的潜伏陷阱而非已观测崩溃。属独立议题。
