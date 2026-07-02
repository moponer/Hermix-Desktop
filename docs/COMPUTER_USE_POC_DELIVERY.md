# Computer-Use PoC 交付报告（Phase 1）

> 2026-06-23 落地。
> 实现 v2.0 方案的 **Phase 1 PoC**：5 个自研工具，零三方运行时，仅 Windows 10/11 x64。

---

## 一、目标范围（PoC）

| 工具 | 公开 API | 实现栈 |
|---|---|---|
| `cua_screenshot` | `action: "screenshot", monitor?: number` | `BitBlt` + `GetDC(NULL)` + PNG 编码 |
| `cua_screen_info` | `action: "screen_info"` | `GetSystemMetrics` (`SM_CXSCREEN/SM_CYSCREEN`) |
| `cua_left_click` | `action: "left_click", x, y, button?` | `SetCursorPos` + `SendInput(MOUSEINPUT)` |
| `cua_type_text` | `action: "type_text", text` | `SendInput(KEYBDINPUT, KEYEVENTF_UNICODE)` |
| `cua_press_key` | `action: "press_key", chord` | `SendInput(KEYBDINPUT)` + VK chord 解析 |

**v2.0 方案三约束（已锁定）**：
1. **仅 Windows 10/11 x64** —— hermix-cua 全部模块用 `#[cfg(windows)]` 隔离，非 Windows 平台编译失败但工具注册走 stub 路径
2. **零云服务** —— 无 `browser-use` / `OpenAI CUA` / 任何 SaaS 依赖
3. **自研为主** —— 仅依赖 `windows` crate（Win32 直接绑定）+ workspace 内 `image` crate；DXGI / UIA / OCR / CDP 均为自研路径（Phase 2 落地）

---

## 二、新建/修改文件清单

### 新建：`crates/hermix-cua/`（真项目里）

```
crates/hermix-cua/
├── Cargo.toml                          # windows 0.58 + image 0.25 + 通用 deps
├── src/
│   ├── lib.rs                          # 模块出口 + 类型重导出
│   ├── error.rs                        # CuaError + kind() short string
│   ├── safety.rs                       # 黑名单 + 限速 + RiskLevel + ApprovalMode
│   ├── trace.rs                        # TraceEvent / TraceKind / TraceRecorder (JSONL)
│   ├── capture/
│   │   ├── mod.rs                      # CaptureConfig / ScreenshotResult + BitBlt 实现
│   │   ├── dxgi.rs                     # Phase 2 stub
│   │   └── screen_info.rs              # GetSystemMetrics 实现
│   ├── action/
│   │   ├── mod.rs                      # click / type_text / press_key 公共入口
│   │   └── sendinput.rs                # SetCursorPos + SendInput + VK chord 解析
│   ├── element.rs                      # Phase 2 UIAutomation stub
│   ├── ocr.rs                          # Phase 2 Windows.Media.Ocr stub
│   └── browser.rs                      # Phase 2 自研 CDP stub
```

### 修改（workspace 串联）

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | members 加 `crates/hermix-cua`；`workspace.dependencies` 加 `hermix-cua = { path = ... }` |
| `crates/hermix-tools/Cargo.toml` | 加 `hermix-cua = { workspace = true }` + `once_cell = { workspace = true }` |
| `crates/hermix-tools/src/tools/mod.rs` | 加 `pub mod cua;` |
| `crates/hermix-tools/src/tools/cua.rs` | **新建** —— 5 个 ToolHandler + 共享 SAFETY/TRACE static + map_cua_error + 单元测试 |
| `crates/hermix-tools/src/lib.rs` | 加 5 个 handler 的 re-export |
| `crates/hermix-tools/src/register_builtins.rs` | 注册 5 个工具（toolset=`"cua"`, emoji 分色, RiskLevel::Dangerous） |

---

## 三、关键设计决策

### 3.1 SafetyGuard（hermix-cua）

- **黑名单**：内置 `ctrl+alt+delete`、`ctrl+alt+del`、`win+l`、`meta+l`、`super+l`、`alt+f4`、`ctrl+alt+end`、`win+x`、`win+d`、`ctrl+win+delete`。匹配时先 `lowercase + trim + sort` `+`-分隔的 parts，所以 `"Shift+Ctrl+T"` 和 `"ctrl+shift+t"` 同义匹配。
- **限速**：滑动窗口 `20 actions/sec`（burst）。纯读（screenshot / screen_info）豁免。
- **ApprovalMode**：`TaskLevelOnce`（默认）/ `EveryAction` / `FullAuto` —— 仅作为 flag 暴露，**实际审批在 `PermissionLevel::Default` 的 dispatch gate**。PoC 阶段 `ToolError` 没有 `PermissionDenied` 变体，所以 `Denied` 映射为 `ExecutionFailed("denied by safety: ...")`。
- **共享状态**：所有 5 个 handler 共享 `static SAFETY: Lazy<SafetyGuard>` —— RateLimiter 跨工具累加。

### 3.2 TraceRecorder（hermix-cua）

- **JSONL 文件**：`hermix_config::paths::hermix_home().join("cua-trace.jsonl")`
- **Null sink 兜底**：hermix_home 不可写时（测试 / 极早期启动）降级到 discard sink
- **每条记录**：ISO-8601 UTC 时间戳 + `kind` enum + session_id + summary + ok + elapsed_ms + 可选 error
- **`TraceKind` 用 serde tagged enum**：让前端 JSONL consumer 不用判别 union
- **Phase 2 占位**：`Other { subkind: String }` 给 UIA / OCR 留位

### 3.3 截图路径（BitBlt vs DXGI）

Phase 1 用 `BitBlt` + `GetDC(NULL)` + `CreateCompatibleBitmap` + `BitBlt(SRCCOPY)` + `GetBitmapBits` + `image::codecs::png::PngEncoder`。  
Phase 2 切到 `IDXGIOutputDuplication`，**`dxgi_capture` 签名与 `bitblt_capture` 一致**，handler 不变。Phase 1 性能不如 DXGI（单次全屏 GDI 拷贝）但任何 Windows 10/11 都能跑、零额外依赖。

### 3.4 输入路径（SendInput）

- **绝对坐标 vs 像素坐标**：`SetCursorPos(x, y)` 接受像素坐标，`MOUSEEVENTF_ABSOLUTE` 的 0..65535 归一化空间在多显示器下行为怪异 —— 用像素
- **Unicode 注入**：`KEYEVENTF_UNICODE` + `wScan = codepoint` —— CJK / emoji / combining marks 都能正确发送，**唯一可靠的跨布局方式**
- **VK chord 解析**：`ctrl+shift+t` → `[VK_CONTROL, VK_SHIFT, VK_T]`；按下顺序 mod↓ → tap → mod↑（reverse）
- **黑名单提前拒绝**：`press_key` 入口先 `safety.check(Some(chord), High)`，命中黑名单立即返回 `Denied`，**绝不**进入 SendInput

### 3.5 hermix-tools 集成模式

- **spawn_blocking + timeout**：`tokio::task::spawn_blocking(work)` + `tokio::time::timeout(10s)`，任何超时的 OS 调用自动 drop handle，agent loop 不被卡死
- **map_cua_error → ToolError**：`Denied → ExecutionFailed("denied by safety:")`、`Timeout → Timeout("10s")`、`InvalidParams → InvalidParams`、`NotFound → NotFound`、其他 → `ExecutionFailed("cua::{kind}: {msg}")`
- **共享 SAFETY + TRACE**：`static Lazy<SafetyGuard>` + `static Lazy<TraceRecorder>`，5 handler 共用同一限速桶和同一 trace 文件
- **dispatch gate 不重复**：dispatch layer 在 `PermissionLevel::Default` 已经为 `Dangerous` 工具弹窗，CUA 5 工具都标 `Dangerous`

---

## 四、编译与测试结果

| 命令 | 结果 | 备注 |
|---|---|---|
| `cargo check --workspace` | **0 errors, 0 warnings** | 42.39s（首次完整 build） |
| `cargo test -p hermix-cua` | **12/12 passed** | safety / trace 模块全覆盖 |
| `cargo test -p hermix-tools` | 编译中 | 现有 30+ 工具测试 + 5 个新 handler 测试 |

### hermix-cua 测试用例

```
running 12 tests
test safety::tests::blacklist_matches_case_insensitive ... ok
test safety::tests::normalise_chord_sorts_modifier_order ... ok
test safety::tests::check_rejects_blacklisted_chord ... ok
test safety::tests::blacklist_matches_unordered_modifier_order ... ok
test safety::tests::non_blacklisted_chord_passes ... ok
test safety::tests::empty_chord_is_not_blacklisted ... ok
test safety::tests::rate_limiter_caps_medium_risk ... ok
test safety::tests::rate_limiter_does_not_cap_low_risk ... ok
test trace::tests::null_recorder_swallows_events ... ok
test trace::tests::trace_kind_other_serialises_subkind ... ok
test trace::tests::trace_event_with_error_round_trips ... ok
test trace::tests::recorder_writes_one_json_object_per_line ... ok

test result: ok. 12 passed; 0 failed
```

### 编译错误修复记录（5 轮迭代）

| 错误 | 根因 | 修复 |
|---|---|---|
| `KEY_EVENT_FLAGS` not found | windows 0.58 改名为 `KEYBD_EVENT_FLAGS` | 改名 |
| `SetCursorPos().as_bool()` no method | 返回 `Result<(), Error>` 不是 `BOOL` | 用 `.is_err()` |
| `*input.Anonymous.mi()` no method | union field 不是 method | 改为 `input.Anonymous.mi = mi`（unsafe 块内） |
| `DeleteObject(bitmap.into())` type annotation needed | windows_core `Param<HGDIOBJ>` 多 impls，推断不出 | 去掉 `.into()`，直接传 `bitmap` |
| `SelectObject(mem_dc, bitmap.into())` 同上 | 同上 | 同上 |
| `ToolError::PermissionDenied` not found | ToolError enum 没这变体 | 改 `ExecutionFailed("denied by safety: ...")` |
| `once_cell` unresolved | hermix-tools Cargo.toml 没声明 | 加 `once_cell = { workspace = true }` |
| `tempfile` unresolved in test | hermix-cua dev-dependencies 缺 | 加 `tempfile = "3"` |
| `ApprovalMode` unused import | `SafetyGuard::with_mode` 临时变量借用悬空 | 改用 `static SAFETY` 复用 |
| `normalise_chord` 没 lowercase | test 暴露 | 加 `.to_ascii_lowercase()` |

---

## 五、未完成项（待用户确认）

### 5.1 前端集成

| 文件 | 改动 |
|---|---|
| `apps/desktop-tauri/src/components/InlineApprovalCard.tsx` | **不需要改动** —— 已支持 `tool` / `args` / `riskLevel` / `requestId` props，CUA 5 工具直接复用 |
| `apps/desktop-tauri/src/components/MessageBubble.tsx` | 加 `image_url` 渲染分支 —— `cua_screenshot` 返回 `png_base64` 时显示为内联 `<img>` |
| `apps/desktop-tauri/src/components/ChatView.tsx` | 已经在 `WorkspaceLocked` 包装下接入 `PolishButton` —— `CuaButton` 接入模式一致 |

### 5.2 Tauri 集成（可选）

> hermix-cua 是 Rust crate，可被 hermix-server 通过 HTTP/WebSocket 调到。如果需要前端**直接**触发（不通过 LLM），需要：
>
> - `apps/desktop-tauri/src-tauri/src/commands.rs` 加 5 个 `cuA_*` invoke handler（直接调 hermix-cua）
> - `apps/desktop-tauri/src-tauri/src/lib.rs` invoke_handler 注册
> - `apps/desktop-tauri/src-tauri/Cargo.toml` 加 hermix-cua 依赖
>
> 但 **`src-tauri/Cargo.toml` 是独立 `[workspace]`**（不依赖主 workspace），所以这条路要重新评估是否值得 —— 多数场景走 hermix-server 的 WebSocket 更轻。

### 5.3 端到端 demo

记事本自动化（cua_screenshot → cua_left_click → cua_type_text → cua_press_key enter）：
1. `cua_screenshot` 拿到主屏图像
2. LLM 视觉理解，定位记事本窗口 + 文本区坐标
3. `cua_left_click(x, y)` 点击文本区
4. `cua_type_text("Hello from Hermix")` 输入文本
5. `cua_press_key("ctrl+s")` 保存
6. 文件对话框 → `cua_press_key("Return")` 确认

PoC 阶段没有 LLM 视觉，需要预定义坐标 —— 实际跑需要接入 multimodal 模型。

---

## 六、Phase 2 路线图

| 模块 | Phase 2 任务 | 预计工时 |
|---|---|---|
| `capture::dxgi` | `IDXGIOutputDuplication` 实现，dirty-rect delta tracking，多显示器枚举 | 1 周 |
| `element` | UIAutomation COM 接口封装（`IUIAutomation` → `IUIAutomationElement`） | 1 周 |
| `ocr` | Windows.Media.Ocr WinRT 集成，async 桥接到 sync API | 3 天 |
| `browser` | 自研最小 CDP 客户端（tokio_tungstenite + serde_json），spawn chrome sidecar | 2 周 |
| 端到端 demo | 实际 LLM 视觉接入（多模态模型 + 截图循环） | 2 周 |

---

## 七、附录：注册表状态

`register_builtins.rs` 新增 5 行：

```rust
reg_risk(&enabled_toolsets, registry, "cua",
    Arc::new(crate::tools::cua::CuaScreenshotHandler::new()), "📸", vec![],
    RiskLevel::Dangerous);
reg_risk(&enabled_toolsets, registry, "cua",
    Arc::new(crate::tools::cua::CuaScreenInfoHandler::new()), "🖥️", vec![],
    RiskLevel::Dangerous);
reg_risk(&enabled_toolsets, registry, "cua",
    Arc::new(crate::tools::cua::CuaLeftClickHandler::new()), "🖱️", vec![],
    RiskLevel::Dangerous);
reg_risk(&enabled_toolsets, registry, "cua",
    Arc::new(crate::tools::cua::CuaTypeTextHandler::new()), "⌨️", vec![],
    RiskLevel::Dangerous);
reg_risk(&enabled_toolsets, registry, "cua",
    Arc::new(crate::tools::cua::CuaPressKeyHandler::new()), "🔘", vec![],
    RiskLevel::Dangerous);
```

`computer_use` handler（enigo/xcap/arboard 版本）**保留**，与 CUA 并存 —— `computer_use` 提供 10 个完整动作（clipboard、scroll、multi-display），CUA 提供 PoC 5 工具的轻量替代。