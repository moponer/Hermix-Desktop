# Decision Note: 浏览器/CU 授权面审计报告

Status: implemented — 2026-10-04 审计 + 修正实施。审计结论：**分级机制本身正确，但存在真实不一致**。实施阶段复核源码后发现审计的「发现 3」有一处误判（`cua_ui_click` 其实已有限速），已按更正后的结论落地：观察类注册档降到 `Caution`，UIA 动作路径按副作用分档接入 `SafetyGuard`。

## 背景事实（读源码所得）

### 一、先纠正上一轮的一处误判

上一轮开工清单写「`cua.rs` 14 个 Dangerous 疑为懒惰一刀切，观察类标 Dangerous 极可能不合理」。**审计后这个判断需要修正**：

`CoreRisk` 并不是独立枚举，而是 `hermix_core::RiskLevel` 的别名——

```rust
// cua.rs:40
use hermix_core::{..., traits::RiskLevel as CoreRisk};
```

即 `cua.rs` 与 `browser.rs` **用的是同一套 `Safe/Caution/Dangerous`**，不是两套词汇。上一轮说的"两套词汇"不成立。

### 二、真实存在的三套词汇（这才是真问题）

| 枚举 | 定义位置 | 语义层 |
|---|---|---|
| `RiskLevel { Safe, Caution, Dangerous }` | `hermix-core/src/traits.rs:314` | **工具注册级**——这个工具 Generally 有多危险 |
| `RiskLevel { Low, Medium, High }` | `hermix-cua/src/safety.rs:60` | **单次调用级**——这一次调用有多危险 |
| `ApprovalMode { TaskLevelOnce, EveryAction, FullAuto }` | `hermix-cua/src/safety.rs:31` | 审批粒度 |

`hermix-cua` 的文档注释自己承认这个分层（「this one describes a *specific invocation*」），**分层是设计意图，不是混乱**。但两套 `RiskLevel` **同名**，且 `hermix-cua` 在 `lib.rs:65` 对外 `pub use safety::{..., RiskLevel, ...}`——**任何 crate 同时 import 两个 `RiskLevel` 就会撞名**。`cua.rs:40` 的别名写法正是这个问题的证据。

### 三、审批门在何处生效

`hermix-core` 的 `RiskLevel` 文档明确了语义：

- `Safe` — read-only, no observable side effect. **Always dispatched**（不弹卡）
- `Caution` — 可写文件/出网，**默认问**
- `Dangerous` — 破坏性动作，**Default 模式下总是问**

所以「`cua_screenshot` 标 Dangerous」的**实际效果**是：每次截图弹一次审批卡。而它是纯读操作（DXGI 抓屏，无副作用）。

## 审计结果

### 发现 1：`cua_move_cursor` 的分级是唯一精确的（正面）

`cua.rs:101` 标 `CoreRisk::Safe`，与 `action/mod.rs:277` 的 `check(None, RiskLevel::Low)` 一致。移动光标确实无副作用。

**这条证明分级机制在 CU 写作者手里是能写对的**——问题不是机制，是**没被完整用起来**。

### 发现 2：三层风险标注的一致性表（逐项核实）

`cua.rs` 注册档 × `action/mod.rs` 调用档：

| 工具 | 注册档 | 调用档（`action/mod.rs`） | 一致？ |
|---|---|---|---|
| `cua_left_click` | Dangerous | Medium（:104） | ⚠️ 注册更严 |
| `cua_double_click` | Dangerous | Medium（:247） | ⚠️ 注册更严 |
| `cua_type_text` | Dangerous | Medium（:146） | ⚠️ 注册更严 |
| `cua_scroll` | Dangerous | Medium（:217） | ⚠️ 注册更严 |
| `cua_press_key` | Dangerous | **High**（:180） | ✅ |
| `cua_move_cursor` | **Safe** | Low（:277） | ✅ 唯一精确 |
| `cua_click_window` | Dangerous | Medium（:924，修正前在副作用之后） | ⚠️ 已修 |
| `cua_ui_click` | Dangerous | Medium（经 `action::click`，修正前在查找之后） | ⚠️ 已修 |
| `cua_ui_focus` / `cua_ui_invoke` | Dangerous | 修正前**无**；现 Medium / High | ❌ **已修，见发现 3** |
| `cua_screenshot` / `screen_info` / `list_windows` / `window_at_point` / `ui_tree` / `ui_find` | ~~Dangerous~~ → **Caution** | Low（净效果为零，见发现 5） | ✅ **已修，见发现 4** |

`RiskLevel::Medium/High` 会触发 `safety.rs:195` 的**限速**（`try_record`），`Low` 不触发。所以调用档的实际功能是「要不要计入限速」。

### 发现 3：UIA 动作路径的限速接线不完整（安全缺口，已实施修正）

初版审计写「`cua_ui_click` / `ui_focus` / `ui_invoke` 三个都既无限速也无黑名单」。**实施前复核源码，这个判断有一处误判**：

`cua_ui_click` 走的是 `uia::click_by_name`（取坐标）→ `action::click`（`cua.rs` 闭包内），而 `action/mod.rs:104` **有** `check(None, RiskLevel::Medium)`。**它本就有限速**。

复核后的准确结论：

| 工具 | 副作用路径 | 修正前 | 定档依据 |
|---|---|---|---|
| `cua_ui_click` | `uia::click_by_name` → `action::click` | 有限速，但**UIA 树遍历在 check 之前**，闸门形同虚设 | `Medium`（提到查找之前） |
| `cua_ui_focus` | `uia::focus_by_name` → `IUIAutomationElement::SetFocus` | **完全无限速** | `Medium`（UIA 遍历 50–500 ms/次） |
| `cua_ui_invoke` | `uia::invoke_by_name` → `InvokePattern.Invoke` | **完全无限速，且语义等价于"按某个按钮"** | `High`（与 `press_key` 同档） |
| `cua_click_window` | `find_window` + `focus_window`（改前台焦点）→ `action::click` | 有限速，但**窗口状态变更在 check 之前** | `Medium`（提到闭包首行） |

`uia.rs` 全文只有两处写操作（`Invoke` :734、`SetFocus` :760），其余 `get_control_view_tree` / `find_all` / `element_from_point` 均为 TreeWalker + 属性读取，**纯读**——这是「观察类可降档」的直接证据，不是推测。

`cua_ui_invoke` 是三者中最危险的：元素名由 LLM 提供，**从不与屏幕上真实存在的控件核对**，等价于一个不受坐标约束的"确认按钮机器"。

### 发现 4：观察类工具的注册档偏高（体验债，非安全债）

6 个纯观察工具（screenshot / screen_info / list_windows / window_at_point / ui_tree / ui_find）注册档是 `Dangerous`，调用档是 `Low`。按 `hermix-core` 语义，`Safe` = "always dispatched"、`Dangerous` = "Default 模式下总是问"——**即每次 `cua_ui_tree` 都要用户点一次批准**。

这与用户 2026-09-26 的定策（「写文件和命令执行不用审批，只有删除等高危才审批」）方向一致地错了：**读操作比写操作更严格**。

### 发现 5：观察类的 4 处 `.check(None, Low)` 净效果为零

`safety.rs:195` 的限速分支是 `matches!(risk, Medium | High)`，`Low` 直接跳过；黑名单分支需要 `chord: Some`，而观察类都传 `None`。**这 4 处 check 不做任何事**——不是 bug（读操作确实不该限速），但它是"看起来有两层防护、实际只有一层"的假象。已保留：它标记了"这个 handler 经过了 guard"这一意图，且未来若 `Low` 语义变化，行为自动跟上。

## Decision

### 不做统一重构，做三处最小修正

审计的结论是**机制正确、覆盖不全**。把三套词汇合并成一套是过度工程（六步阶梯第 1 条 YAGNI）——分层本身有明确文档辩护，且 `Medium/High` 触发限速、`Low` 不触发是**实际有用**的区分。

**已实施（`crates/hermix-tools/src/tools/cua.rs`）：**

1. **观察类注册档 `Dangerous` → `Caution`**（6 个工具）
   纯读、无副作用、调用档已是 `Low`。**没有降到 `Safe`**——`cua_screenshot` 会把屏幕上的一切（含密码、私人文档名）带进对话记录，`Caution` 的"会碰你的数据"这层语义要留着；`Safe` 语义是"no observable side effect"，截图读到用户隐私不算"无可见影响"。`Caution` 与 `Safe` 在派发上等价（`local_agent_service.rs:1299-1304` 两者都直接 `Allow`），但工具列表上呈现的风险标签不同。

2. **UIA 动作路径按副作用分档接入 `SafetyGuard`**
   不加新机制，只把 UIA 路径接进已有的 `check`：
   - `cua_ui_invoke` → `High`（`InvokePattern.Invoke` 等价于按按钮，元素名不与屏幕核对）
   - `cua_ui_focus` → `Medium`（UIA 遍历 50–500 ms/次）
   - `cua_ui_click` → `Medium`，且**提到 UIA 查找之前**（原位置在 `action::click` 内，挡不住昂贵的树遍历）
   - `cua_click_window` → `Medium`，且**提到闭包首行**（`focus_window` 改前台焦点，原位置在点击之后）

   定档原则写进了代码注释：**副作用之前付限速费，不是之后**。

3. **两条回归测试**
   - `registration_tiers_match_side_effects`：钉死全部 16 个工具的注册档。注册档决定是否弹审批卡，是安全相关值不是标签。已做变异测试验证（故意把 `cua_ui_invoke` 降档 → 断言变红 → 恢复）。
   - `low_risk_is_not_rate_limited`：钉住 `Low` 不进限速窗口这个前提——handler 的分档选择依赖它，若将来 `Low` 语义变化，这条会先响。

### 明确不做

- **不合并三套词汇**——分层有文档辩护，`Low/Medium/High` 实际控制限速，有用。
- **不改 `cua_press_key` 的 `High`**——它带黑名单检查，是唯一注册档与调用档精确对应的高危工具。
- **不给 `cua_ui_find` 的 `auto_id` 加签发校验**——那是第 3 项（注入防护）的范围，本审计只记录不处理。
- **不删观察类的 4 处 `.check(None, Low)`**——当前净效果为零，但删掉会丢失"该 handler 经过 guard"的意图标注，且未来 `Low` 语义变化时行为自动跟上。

## Alternatives considered

- **把浏览器与 CU 收敛到一套审批面（上一轮的建议）** — 否决：审计证明两者**已经用同一套** `Safe/Caution/Dangerous`，不存在需要收敛的两套词汇。上一轮基于「`CoreRisk` 是独立枚举」的误判，此路不通。
- **把 CU 全部标 `Dangerous` 保持现状** — 否决：观察类弹卡是每天都会遇到的体验债，且与用户已定策方向相反。
- **为 CU 引入独立进程边界（差距 A）** — 否决：那是 `hermix-cua` 进程隔离，属架构决策。本审计发现 3 的限速缺口用几行代码就能补，不必等进程拆分。
- **观察类降到 `Safe` 而非 `Caution`** — 否决：派发行为上两者等价（都直接 `Allow`），但 `Safe` 的语义是"no observable side effect"，而截图会把屏幕上的密码与私人文件名带进对话记录。`Caution` 保留了这层语义，代价为零。
- **给 `cua_ui_invoke` 用 `Medium` 与其他三个对齐** — 否决：`InvokePattern.Invoke()` 不需要坐标，元素名由 LLM 提供且从不与屏幕核对，是三者中唯一"盲按按钮"的路径，与 `press_key` 同为 `High` 更诚实。
- **把 `hermix-cua::RiskLevel` 改名以消除同名冲突** — 否决（但记录为技术债）：`lib.rs:65` 对外导出 `RiskLevel` 确实有撞名风险，但当前所有调用点已用别名（`cua.rs:40` 的 `as CoreRisk`）规避。改名是 API 破坏性变更，收益是消除一个尚未发生的编译错误。**记入技术债，等真有第二个 crate 同时 import 时再动。**

## Consequences

- **获得**：观察类不再无谓弹卡；UIA 动作路径获得与鼠标路径同等的限速保护，且**限速闸门位于副作用之前**；注册档与调用档的对应关系有测试钉住，后续任何 CU 改动都会在测试里撞出来。
- **放弃**：风险词汇维持三套（分层保留）。
- **不变**：黑名单仍只覆盖键 chords（`BLACKLIST` 是 `&[&str]` 键名表）。UIA 动作族没有 chord 概念，本次不扩这个机制——`cua_ui_invoke` 的等价风险靠 `High` 限速档覆盖，不是靠黑名单。
- **风险**：观察类降档后，若真机走查发现 UIA 查询确实能拖垮目标应用 UI 线程，需要回滚这一条——**所以这一条要真机验证，不能只靠 cargo test**。注意 UIA 查询触发目标应用 UI 线程是所有自动化工具的共性风险，不构成本次降档的反证理由。
- **教训（已写进项目记忆）**：本审计初版把 `cua_ui_click` 判为"无限速"，实际它走 `action::click` 已有 Medium 限速。**误判源于只看了 handler 入口没追进被调用的 `action/mod.rs`**。审计跨 crate 的调用链时，`grep` 到的 `.check()` 数量不是证据，被调函数的函数体才是。
