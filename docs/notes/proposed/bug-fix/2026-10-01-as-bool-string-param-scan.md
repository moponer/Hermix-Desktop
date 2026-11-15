# Decision Note: as_bool() 字符串参数扫描 — 模型传字符串布尔导致静默失败

Status: proposed — 扫描结论已就绪，修复批次待认领

## Decision

`hermix-tools` 全部 23 处 `as_bool()` 调用分三类处置：**B 类（静默放行，方向不安全）优先修**，`voice_mode.enabled` 是安全问题；**A 类（静默不执行）**中会导致模型陷入重试循环的（`move_file.overwrite`）与静默退化但无报错的关键项（`disk_cleanup.dry_run`）第二批修；C 类（读 handler 输出）不动。

实现方式：抽一个只接受**明确真值**的 `bool_param(params, key, default)` helper，替换上述调用点。不在 tool_call_parser 做全局类型强转。

本笔记是**扫描结论**，修复批次待认领后转为 `implemented/`。

## 背景

2026-10-01 用户实测发现 `storage_clean` 的 `confirm` 参数死循环：
模型传 `{"confirm": "true"}`（字符串），`as_bool()` 返回 None，工具报
"必须有 confirm: true"，而参数明明传了。用户点两次批准、两次都失败。

根因是 `Value::as_bool()` **只认 JSON 裸布尔**。模型输出字符串形式布尔
是常见行为，而 `unwrap_or(default)` 把这种"类型不符"伪装成"没传"，
错误信息还指向错误的方向（让模型去查不存在的字段名），于是陷入重试循环。

## 扫描范围

`crates/hermix-tools/src/tools/` 下全部 `as_bool()` 调用，共 23 处。

## 分类结论

### A 类 — 静默不执行（模型意图未达成，但方向安全）

模型传了参数，工具当成没传，行为退化到默认值。**不会造成破坏，但模型
会收到误导性错误并反复重试**——正是 storage_clean 那个死循环的形态。

| 位置 | 参数 | 默认 | 后果 |
|---|---|---|---|
| `disk_cleanup.rs:310` | `dry_run` | `true` | 模型传 `"false"` 想真删 → 退回 dry-run，**只扫描不删**。schema 明说"设 false 实际删除" |
| `file.rs:1030` | `overwrite` | `false` | 模型传 `"true"` → 报"目标已存在，请设置 overwrite: true"，与 storage_clean 同型死循环 |
| `file.rs:461` / `file.rs:506` | `replace_all` | `false` | 只替换首个匹配，多处替换要循环调用 |
| `aliases.rs:135` | `replace_all` | `false` | 同上 |
| `pdf.rs:363` | `layout` | `true` | 布局退化为默认 |
| `web.rs:213` | `include_links` | `true` | 链接退化为默认 |
| `office_cli.rs:109/158/161/185` | `json` / `force` | `false` | CLI flag 丢失；`force` 尤其值得注意（batch 操作的强制覆盖） |
| `skill_utils.rs:235` | `required` | `false` | 技能环境变量被标成非必填 |
| `terminal.rs:575/580` | `background` / `pty` | `false` | 退化为前台非 PTY |

### B 类 — 静默放行（参数方向不安全，需优先处理）

| 位置 | 参数 | 问题 |
|---|---|---|
| `voice_mode.rs:657` | `enabled` | `unwrap_or(!previous)` —— 传字符串时**翻转当前状态**。用户说"关掉语音"→ 状态被翻转成开启 |
| `cronjob.rs:120` | `enabled` | 传字符串 → None → 走"不修改"分支，配置更新静默丢失 |
| `model_providers.rs:174` | `enabled` | 同上，provider 启停状态静默不更新 |

B 类里 **`voice_mode.rs:657` 最严重**：它不是退回默认值，而是**取反**。
模型意图"关闭"、实际"开启"，方向完全相反且无任何提示。

### C 类 — 无害

- `cua.rs:1926`、`pdf.rs:189`：读 handler **输出**的 JSON，不是模型输入
- `cronjob.rs:120` / `model_providers.rs:174` 的 `Option<bool>` 语义本身
  就是"None = 不修改"，缺失与类型错误无法区分，但至少不会误动作

## 建议方案

抽一个共用的宽松取值 helper，放在 `hermix-tools` 的公共位置：

```rust
/// 读布尔参数，兼容模型输出的字符串形式。
/// 只接受明确真值（true / "true" / "1"，trim + 大小写不敏感），
/// 其余一律 false —— 删除、覆盖等操作不得做 truthy-cast。
pub fn bool_param(params: &Value, key: &str, default: bool) -> bool
```

然后替换 A/B 两类中**会误导模型**的调用点。判据：
- 错误信息会指向错误方向（如 overwrite 那个）→ 必须换
- 静默退化但无报错（如 dry_run）→ 换，因为模型无法察觉
- B 类三个（`!previous` / `Option<bool>`）→ 必须换，`voice_mode` 是安全问题

不动的：`cua.rs:1926`、`pdf.rs:189`（读输出非输入）。

## 为什么不一次全换

23 处里有 8 处是"读 handler 输出"或"Option 语义天然容忍缺失"，真正需要
改的是 15 处。一次性全改会让 diff 难以审阅，且这些点多数不影响正确性。
建议按 B 类 → A 类中会误导模型的分两批做，每批配测试。

## Alternatives considered

- **在每个调用点手写 `.and_then(as_bool).or_else(|| s.eq_ignore_ascii_case("true"))`** — 否决：23 处重复同一个宽松逻辑，改一处漏一处，且每个点的"该接受什么"没人复核。抽 helper 让规则只有一份。
- **helper 写成宽松的 truthy-cast（非空非 "false" 即真）** — 否决：这是删除/覆盖类操作的保险栓。"yes" / "1" / 任意非空字符串都被当真，等于给了一个绕过点。helper 只接受明确真值。
- **在 tool_call_parser 层做全局类型强制转换（字符串 "true" → 布尔）** — 否决：范围远大于本问题。参数里的字符串常常是**合法值**（比如 `mode: "trash"`），全局强转会误伤；而且解析层看不到每个参数的语义，无从判断 `"1"` 对 `dry_run` 该当真还是假。
- **不动，只修 `storage_clean`** — 否决：`voice_mode.rs:657` 的 `unwrap_or(!previous)` 是取反而非取默认，模型说"关掉"会变成"开启"，这是安全问题不是体验问题。

## Consequences

- **代价**：新增一个 helper + 约 15 处替换，约 200 行 diff。收益是消除一类
  反复出现的"模型卡在同一个错误上"的失败模式。
- **不做的后果**：`voice_mode.enabled` 的取反语义会继续产生"用户说关、
  实际开"的行为；`move_file.overwrite` 继续死循环。
- **风险**：helper 一旦写成宽松的 truthy-cast，会削弱删除类操作的保险栓。
  因此 helper 只接受明确真值，`"false"` / `"yes"` / `1` / `null` 一律 false，
  并用测试锁死（storage_clean 的三个测试已是这个范式）。
