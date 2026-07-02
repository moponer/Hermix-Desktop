# hermix-cua

**Hermix Computer-Use Agent 自研栈** — Windows 10/11 x64 桌面自动化底层能力。

## 三个硬约束

| # | 约束 | 体现 |
|---|------|------|
| 1 | **仅 Windows 10/11 x64** | `#[cfg(windows)]` 隔离 capture / action;其他平台为空 stub |
| 2 | **零云服务** | 截屏 / OCR / 输入全部本地完成,无需任何 token / 账号 |
| 3 | **自研为主** | 不集成任何三方 desktop automation runtime,所有 Rust 代码自研 |

## 模块结构

```
crates/hermix-cua/
├── Cargo.toml         ← windows 0.58 + WinRT (Windows_Media_Ocr / Imaging / Storage_Streams) + image + serde + base64 + once_cell
├── README.md          ← 本文档
└── src/
    ├── lib.rs         ← 模块出口
    ├── error.rs       ← CuaError (Dxgi / SendInput / Uia / Ocr / Cdp / Png / Denied / Timeout / InvalidParams / NotFound / Gdi)
    ├── safety.rs      ← ApprovalMode + RiskLevel + 黑名单 + RateLimiter + SafetyGuard
    ├── trace.rs       ← TraceRecorder 写 ~/.hermix/cua/sessions/<id>/trace.jsonl
    ├── capture/       ← DXGI Output Duplication + BitBlt fallback + screen_info
    ├── action/        ← SendInput 鼠标 / 键盘 (Unicode + chord)
    ├── element.rs     ← Phase 2-B Win32 顶层窗口识别(EnumWindows + GetWindowRect + hwnd <-> isize)
    ├── uia.rs         ← Phase 2-C UIAutomation COM 控件树(UiaAutomation / UiaCondition / UiElement / find_all / find_first_raw / click_by_name / focus_by_name / invoke_by_name)
    ├── ocr.rs         ← Phase 2-D Windows.Media.Ocr 离线 OCR(recognize_from_png / list_supported_languages,同步包装异步)
    └── browser.rs     ← Phase 2-E 占位(自研最小化 CDP,P0 待启动)
```

## 公共 API

```rust
use hermix_cua::{
    capture::{screenshot, CaptureConfig, ScreenshotResult, screen_info, MonitorInfo},
    action::{click, type_text, press_key, MouseButton},
    safety::{SafetyGuard, ApprovalMode, RiskLevel},
    trace::TraceRecorder,
    error::CuaError,
};

// 1. 截屏(主显示器)
let config = CaptureConfig { monitor: 0 };
let result: ScreenshotResult = screenshot(config)?;
println!("{}x{} ({} KB)", result.width, result.height, result.bytes / 1024);
// result.png_base64 → 前端可内联渲染 <img src="data:image/png;base64,...">

// 2. 列出所有显示器
let monitors: Vec<MonitorInfo> = screen_info::enumerate()?;
for m in monitors {
    println!("{}: {}x{}", m.name, m.width, m.height);
}

// 3. 共享 safety guard + trace recorder(整个 session 复用)
let safety = SafetyGuard::new();
let trace  = TraceRecorder::open("~/.hermix/cua/sessions/abc/trace.jsonl")?;

// 4. 鼠标点击
click(&safety, &trace, "session-abc", 1920, 540, MouseButton::Left)?;

// 5. 文本输入(Unicode 完整)
type_text(&safety, &trace, "session-abc", "hello hermix 你好")?;

// 6. 组合键
press_key(&safety, &trace, "session-abc", "ctrl+shift+delete")?;
```

## 自研实现要点

### 截屏主路径 — DXGI Output Duplication

GPU 直采,比传统 GDI BitBlt 快 5–10×。完整链路:

```
D3D11CreateDevice
  → IDXGIDevice::GetAdapter (cast → IDXGIAdapter1)
  → IDXGIAdapter1::EnumOutputs(0)
  → IDXGIOutput::QueryInterface → IDXGIOutput1
  → IDXGIOutput1::DuplicateOutput(device)
  → loop:
      AcquireNextFrame(100ms, frame_info, desktop_resource)
      → IDXGIResource::QueryInterface → ID3D11Texture2D
      → CreateTexture2D(staging_desc)  ← CPU-readable
      → CopyResource(staging, src)
      → Map(staging, READ)
      → copy pixels (BGRA → RGBA, 处理 row_pitch stride)
      → Unmap + ReleaseFrame (RAII: FrameReleaser)
      → image::PngEncoder::write_image(Rgba8)
      → base64::STANDARD.encode
```

### 截屏 fallback — BitBlt

```rust
GetDC(NULL) → CreateCompatibleDC → CreateCompatibleBitmap → BitBlt(SRCCOPY) → GetBitmapBits → image::PngEncoder
```

`capture::screenshot()` 优先走 DXGI,任何错误 (`DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` / `DXGI_ERROR_ACCESS_LOST` / 虚拟 GPU / 多 adapter laptop 切换) 自动 `tracing::warn!` 后回退 BitBlt,工具永不失效。

### 输入注入 — Win32 SendInput

```rust
SetCursorPos(x, y);
SendInput(&[INPUT {
    r#type: INPUT_MOUSE,
    Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: MOUSEEVENTF_LEFTDOWN, ... } },
}]);
SendInput(&[INPUT {
    r#type: INPUT_KEYBOARD,
    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk, wScan, dwFlags: KEYEVENTF_UNICODE, ... } },
}]);
```

- **鼠标**: `MOUSEINPUT` 完整支持(left/right/middle/x1/x2 + down/up/wheel)
- **键盘**: `KEYBDINPUT` Unicode 路径(`KEYEVENTF_UNICODE`)支持任意语言
- **chord 解析**: `ctrl+shift+delete` → `[VK_CONTROL, VK_SHIFT, VK_DELETE]` → 序列 SendInput

### 安全审批 — `SafetyGuard`

```rust
pub enum ApprovalMode { TaskLevelOnce, EveryAction, FullAuto }
pub enum RiskLevel { Low, Medium, High }

pub struct SafetyGuard {
    mode: ApprovalMode,
    blacklist: HashSet<String>,  // 硬拦截 chord
    rate_limiter: RateLimiter,   // 30 ops/s 默认
}

impl SafetyGuard {
    pub fn check(&self, chord: Option<&str>, risk: RiskLevel) -> Result<(), CuaError>;
    pub fn wait_approval(&self, action: &str) -> Result<(), CuaError>;
}
```

**硬拦截 chord**:
- `Ctrl+Alt+Del` — 系统级,SendInput 也无法拦截,客户端必须拒绝
- `Win+L` — 锁屏
- `Alt+F4` — 关闭窗口

### 审计追踪 — `TraceRecorder`

```json
{"ts":"2026-06-23T10:30:15.123Z","kind":"Action","action":"click","target":[1920,540],"button":"left","elapsed_ms":12}
{"ts":"2026-06-23T10:30:15.456Z","kind":"Action","action":"type_text","text":"hello hermix","elapsed_ms":8}
{"ts":"2026-06-23T10:30:16.789Z","kind":"Screen","width":1920,"height":1080,"monitor":"output0","backend":"dxgi"}
```

JSONL 追加写,事后可回放。

## 集成路径

**作为 hermix-tools handler** (LLM 调用):

```rust
// crates/hermix-tools/src/tools/cua.rs
pub struct CuaScreenshotHandler;
impl ToolHandler for CuaScreenshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let config = CaptureConfig::from_params(&params)?;
        let result = hermix_cua::capture::screenshot(config)
            .map_err(|e| ToolError::ExecutionFailed(format!("cua::{}: {}", e.kind(), e)))?;
        Ok(serde_json::to_string_pretty(&result)?)
    }
}
```

**作为 Tauri command** (前端直接调用,绕过 agent loop):

```rust
// apps/desktop-tauri/src-tauri/src/commands_cua.rs
#[tauri::command]
pub async fn cua_screenshot(monitor: Option<usize>) -> Result<CuaCommandResponse, String> {
    let started = Instant::now();
    let result: Result<ScreenshotResult, CuaError> =
        run_blocking(move || hermix_cua::capture::screenshot(CaptureConfig {
            monitor: monitor.unwrap_or(0),
        })).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let result = result.map_err(|e| e.to_string())?;
    Ok(CuaCommandResponse::ok("cua_screenshot", elapsed_ms, serde_json::to_value(&result)?))
}
```

## 编译

```toml
[target.'cfg(windows)'.dependencies]
hermix-cua = { path = "../../../crates/hermix-cua" }
```

仅 Windows 目标。其他平台编译时 `capture` / `action` 模块为空 stub,API 仍存在但调用返回 `CuaError::NotFound`。

## 测试

```bash
cargo test -p hermix-cua     # 30/30 通过
```

覆盖范围:
- ✅ DXGI 路径存在性 pin (`screenshot_dispatches_to_dxgi_path` — 防止静默回退)
- ✅ SafetyGuard 黑名单 (Ctrl+Alt+Del / Win+L / Alt+F4)
- ✅ SafetyGuard 限速 (31 ops/s 应被拒)
- ✅ Chord 解析大小写不敏感 (`CTRL+C` ≡ `ctrl+c`)
- ✅ TraceRecorder JSONL 序列化
- ✅ `CuaError` 11 种变体 `Display` + `kind()` 分类
- ✅ `mouse_button` parse (left/right/middle/x1/x2)
- ✅ 配置 schema 校验
- ✅ `screenshot()` 调度策略 (DXGI → BitBlt 顺序)
- ✅ `ElementRect` 默认值 / center() / contains() / 退化矩形处理
- ✅ UIA 条件解析(Name / AutomationId / ClassName + And 组合)
- ✅ OCR WinRT 路径存在性 pin (`ocr_engine_initializes_when_available`)

## Phase 2 路线图

| Phase | 内容 | 状态 |
|-------|------|------|
| **Phase 1** | 5 工具 (screenshot / left_click / type_text / press_key / screen_info) + BitBlt + SendInput | ✅ 已完成 |
| **Phase 2-A** | DXGI Output Duplication (主路径) + BitBlt fallback | ✅ 已完成 |
| **Phase 2-B** | Win32 顶层窗口识别(EnumWindows + 标题/可见性过滤 + hwnd↔isize) + ClickWindow | ✅ 已完成 |
| **Phase 2-C** | UIAutomation COM 控件树(ui_tree / ui_find / ui_click / ui_focus / ui_invoke,共 5 工具 + bounding rect) | ✅ 已完成 |
| **Phase 2-D** | Windows.Media.Ocr 离线 OCR(ocr_recognize + 语言枚举) | ✅ 已完成 |
| **Phase 2-E** | 自研最小化 CDP 客户端 (connect / navigate / eval / screenshot / click / type) | 📋 待启动 (P0,1-2 周) |
