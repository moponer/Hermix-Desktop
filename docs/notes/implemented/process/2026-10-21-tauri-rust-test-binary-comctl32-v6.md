# Decision Note: Windows 上 Tauri crate 的 Rust 测试二进制起不来 —— comctl32 v6 清单

Status: implemented — 根因已定位并给出可复用解法：`cargo test --no-run` 编译后，用 SDK 的 `mt.exe` 给测试二进制注入 SxS 清单，再直接跑 exe；查看器 85 个单测因此恢复可跑

## Decision

`cargo test -p hermix-viewer --lib` 在本机**进程根本起不来**（不是测试失败）：

```text
process didn't exit successfully: target\debug\deps\hermix_viewer-<hash>.exe
(exit code: 0xc0000139, STATUS_ENTRYPOINT_NOT_FOUND)
```

不改代码也能复现：**改动前的旧测试二进制（2026-10-21 01:20 那个）能跑**（`--list` exit 0、71 个用例），09:50 之后构建的就起不来 —— 说明是依赖图/feature 变化引入的，不是代码 bug。

### 根因链

1. `tauri-runtime-wry` 的 **`common-controls-v6`** feature → `tao` → `windows 0.62` 的
   `Win32_UI_Controls` feature → `Windows::Win32::UI::Controls::TaskDialogIndirect`；
2. 该函数用 `windows_targets::link!("comctl32.dll" "system" …)` 声明 → **静态导入**
   `comctl32.dll!TaskDialogIndirect`；
3. `TaskDialogIndirect` **只在 Common Controls v6（SxS 程序集）里有**，
   `System32\comctl32.dll` 是 v5 → 加载器解析导入表时找不到入口点 → `0xC0000139`。

### 为什么 exe 没事、测试二进制有事

tauri 给 **exe** 嵌了 SxS 清单（`tauri-build` 编译进资源段）：

```xml
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
                        version="6.0.0.0" processorArchitecture="*"
                        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
```

有了它，加载器按 SxS 规则绑定 v6 → `TaskDialogIndirect` 存在。**测试二进制没有清单**
（cargo 不给 test target 嵌资源）→ 绑到 v5 → 起不来。

### 走过的弯路（别再试）

- ❌ **把 `WebView2Loader.dll` 拷到 exe 旁边**：没用 —— 它根本不在导入表里
  （WebView2 是运行时动态加载的）。
- ❌ **把 WinSxS 里的 comctl32 v6 拷到 exe 旁边**：也没用 —— `comctl32.dll` 在
  **KnownDLLs** 列表里，加载器**直接去 System32 取**，压根不看应用目录。
  （这一条是本题的关键：所有"改 DLL 路径"的路子都被 KnownDLLs 堵死。）

### 解法：给测试二进制注入同一份清单

```powershell
$mt = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\mt.exe'
# 1) 从已构建的 exe 里抽出清单（tauri 嵌的那份）
& $mt '-inputresource:target\debug\hermix-viewer.exe;#1' -out:"$env:TEMP\app.manifest"
# 2) 编译测试二进制
cargo test -p hermix-viewer --lib --no-run
# 3) 给测试二进制注入清单（注意：重编会覆盖 exe，需要重新注入）
$exe = (Get-ChildItem target\debug\deps\hermix_viewer-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
& $mt -manifest "$env:TEMP\app.manifest" "-outputresource:$exe;#1"
# 4) 直接跑（不走 cargo，避免重新链接把清单又冲掉）
& $exe
```

## Alternatives considered

- **给测试 target 也嵌清单**（改 `build.rs`，`cargo:rustc-link-arg-tests=…`）：
  本仓库链接器是 `rust-lld`（见 `.cargo/config.toml`），**lld 不支持嵌入 .manifest**
  （那是 MSVC `link.exe` 的功能）→ 这条路在本仓库走不通。
- **降级 `tauri-runtime-wry` 的 `common-controls-v6` feature**：feature 是 tauri 默认带的，
  关掉会失去 v6 控件与清单，代价大于收益。
- **把 `TaskDialogIndirect` 的导入去掉**：它在 `tao` 里，不在本仓库代码里，动不了。
- **改测试逻辑去迁就环境**（比如"有端口文件就跳过"）：治标且掩盖问题 —— 正确做法是
  **让测试不依赖机器全局状态**（见下）。

## Consequences

- 本仓库在 Windows 上跑 tauri crate 的 Rust 测试，需要「编译 → 注清单 → 直接跑 exe」
  三步；`cargo test` 直接跑会一直报 `0xC0000139`，**别再把它当成代码坏了**。
- 顺带修掉三个被这个问题掩盖的测试问题（断言停留在旧实现上，测试跑不起来就一直没人发现）：
  - `pdf_ops::tests::three_annotation_kinds_are_written_as_standard_annots` 的 `/Contents`
    断言还停在「UTF-8 原始字节」，而实现早已按 PDF 规范改成 **UTF-16BE + BOM**；
  - 同一个测试的 `/QuadPoints` 断言还停在**嵌套数组**形态，而实现早已改成
    **扁平 8 个数**（嵌套会被 pdf.js 静默丢弃）→ 断言改成「8 个、每项都是数字」；
  - `webview_cdp::tests::missing_port_file_reports_clearly` 依赖**机器全局状态**
    （真实 profile 目录里只要有 `DevToolsActivePort` 就随机红）→ 改成
    `devtools_endpoint_in(base, timeout)`，测试指向**空的临时目录**。
- 该环境问题**不是本仓库代码引入的**（改动前的旧二进制能跑），但它让整个查看器的
  Rust 测试长期处于「跑不了」状态 —— 于是测试二进制起不来这件事本身值得写下来，
  否则每个新会话都要重新查一遍。

## Reintroduction conditions

- 若链接器换成 MSVC `link.exe`（不再用 `rust-lld`），`cargo:rustc-link-arg-tests` 就能
  直接嵌清单 → 本笔记的「注清单」步骤可以删掉。
- 若 tauri / tao 升级后不再静态导入 `comctl32.dll!TaskDialogIndirect`，本问题自然消失；
  判断方法：`dumpbin /imports <test.exe> | findstr TaskDialog`（有输出 = 还会犯）。
