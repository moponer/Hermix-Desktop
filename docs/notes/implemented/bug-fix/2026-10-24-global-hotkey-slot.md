# Decision Note: 全局热键收口到一个槽 —— 规范化比较 / 幂等注册 / 冲突自动兜底

Status: implemented — 截图与文件搜索共用 `hotkey.rs` 的 `HotkeySlot`；派发按「当前生效的键」比对，被占时自动改用备选键并回报前端

## Decision

截图（`screenshot_controller`）与文件搜索（`file_search_controller`）的全局热键
不再各写一遍，统一收口到新模块 `apps/desktop-tauri/src-tauri/src/hotkey.rs`
的 `HotkeySlot`。三条规则由这一个类型保证：

### 1. 比较两端都过同一个规范化函数（这是"按了没反应"的真因）

`global_hotkey::HotKey` 的 `Display` 是 `"alt+KeyQ"` —— 修饰键小写、键名用
`keyboard_types::Code` 的**变体名**，不是 `"alt+Q"`。此前 lib.rs 的派发写死
`name == "alt+q"`，那个条件**永远不成立**：热键注册成功、按下去什么都不发生。
用户看到的现象是"热键没反应 / 被占"。

现在注册与比较都走 `canon_of()`（`HotKey::Display` + 小写），
派发时比的是**槽里登记的当前生效键**，而不是某个写死的字符串。

### 2. 重复注册是幂等的

`tauri_plugin_global-shortcut` 对同一实例重复注册同一个键返回
`AlreadyRegistered`。setup 已经注册了默认热键、前端启动时又回放一次配置，
必然撞上 → 前端弹"热键未生效"并把 UI 回滚，而功能其实好着。

`HotkeySlot::apply` 先比对登记项，命中就直接返回当前状态，不重复注册。

### 3. 冲突不再等于哑掉

请求的热键被别的程序占着时，依次尝试备选键（截图
`Ctrl+Alt+A / Ctrl+Alt+S / Ctrl+Alt+J`；文件搜索 `Ctrl+Alt+F / Ctrl+Alt+G /
Ctrl+Alt+K`），全被占才报错。生效的是哪个键一律回报给前端
（`screenshot_apply_config` / `file_search_apply_config` 的返回值 +
`hermix:screenshot-hotkey` 事件），设置页显示"当前实际生效：X"，
启动时若被换掉则 toast 提示。

### 4. 单一真相源

谁注册谁记账：槽里存 `Shortcut` 本体（注销直接用，不再靠字符串反解 ——
`Code` 的 FromStr 只认变体名，`"printscreen"` 这类小写显示名反解不回去，
注销会静默失败）。setup 注册的默认键也进这张账，关闭功能时解的就是它。

## Alternatives considered

- **继续用写死的 `"alt+q"` 字符串比较** —— 否决。它同时错两次：键名形态对不上
  （`alt+KeyQ`），用户改热键后也对不上。两次都是"注册成功但按了没反应"。

- **引入 `tauri-plugin-context-menu` 之类的官方插件来处理热键** —— 否决。
  本地 cargo registry 里没有该 crate，且热键管理只需要「注册 / 注销 / 查当前值」
  三件事，自己写 200 行可控且不引依赖。

- **热键被占就直接报错，让用户自己去改** —— 否决。这是旧行为，用户看到的只有
  一行 stderr，功能静默失效。现在是自动兜底 + 明说换成哪个键。

- **给每个功能各写一份热键管理（维持两处实现，只修 bug）** —— 否决。
  两个功能的 bug 完全同构（重复注册 / 比较口径 / 记账），修一处漏一处的概率很高；
  收口到一个类型后，`hotkey.rs` 里带单测（规范化、展示名、比较口径）。

## Consequences

- 新增依赖：零。新增文件：`hotkey.rs`（含 3 个单测）。

- 热键"生效值"现在有两个：用户请求的（本地存储）与实际生效的（Rust 侧）。
  设置页以 Rust 侧为准展示，避免"我选了 Alt+Q、实际按的是别的键"。

- 备选键是**兜底**不是**配置**：用户在设置页仍从固定的 4 个选项里选，
  被占时由 Rust 自动换到备选键并告知。备选键与用户选的键冲突时会跳过。

- `file_search_apply_config` 的返回类型从 `Result<(), String>` 变成
  `Result<HotkeyStatus, String>` —— 前端原本不读返回值，向后兼容。

- 派发不再硬编码：新增全局热键功能时，必须在自己的 `HotkeySlot` 里登记，
  lib.rs 的 handler 才会认它。这是刻意的"显式登记"，避免漏分发。
