# Decision Note: 独立查看器白屏——CSP 缺 'wasm-unsafe-eval' 与共享预览包的 WASM 顶层 await

Status: implemented — 2026-10-19 定位并修复 `hermix-viewer.exe` 启动即纯白页（`#root` 子节点数 = 0）；viewer 的 CSP 补齐 `'wasm-unsafe-eval'` / `worker-src` / `http://ipc.localhost`

关联：[独立查看器预览引擎懒加载 + 首调冷启延迟的完整表征](2026-10-19-viewer-lazy-preview-engine.md)（同批排查的后一半：冷启 20s 的表征与懒加载改造）

## Decision

`apps/viewer/src-tauri/tauri.conf.json` 的 `app.security.csp` 此前是：

```
default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; img-src 'self' data: blob:; object-src 'self' data:
```

现改为：

```
default-src 'self'; connect-src 'self' ipc: http://ipc.localhost; style-src 'self' 'unsafe-inline'; script-src 'self' 'wasm-unsafe-eval'; img-src 'self' data: blob:; object-src 'self' data:; worker-src 'self' blob:;
```

三处缺口的成因各不相同，逐条记在这里，因为**它们都会表现为"窗口打开了但一片空白"**：

1. **`script-src` 缺 `'wasm-unsafe-eval'`（致命，白屏的直接原因）**。viewer 通过共享包 `@hermix/preview` 的 barrel（`packages/preview/src/index.ts`）间接引入 `satteri`（WASI 版 CommonMark 解析器），而它的浏览器入口 `satteri_napi.wasi-browser.js` 在**模块顶层**做 `await fetch(wasm)` + `await __emnapiInstantiateNapiModule(...)`，即 `WebAssembly.instantiate`。CSP 无 `'wasm-unsafe-eval'` 时该调用被 Chrome 拒绝并抛 `CompileError`；顶层 await 拒绝会沿 import 图向上传播，`App.tsx` → `main.tsx` 整条求值链失败，**`createRoot().render()` 从未执行**，DOM 停在 `<div id="root"></div>` 空壳——纯白页，且**控制台之外没有任何界面线索**。
2. **缺 `worker-src`**。`satteri` 的 WASI 运行时用 `new Worker(new URL('./wasi-worker-browser.mjs', import.meta.url), { type: 'module' })` 拉异步工作线程（`asyncWorkPoolSize: 4`）。没有 `worker-src` 时回落到 `default-src 'self'`，`blob:` 不在允许集内，WASI 线程创建会被拦。
3. **`connect-src` 缺 `http://ipc.localhost`**。Windows 上 Tauri v2 的 IPC 端点是 `http://ipc.localhost`（不是 `ipc:` 这个 scheme token）。缺了它，首次 `invoke` 的 fetch 被 CSP 拦掉，Tauri 自行回退到 postMessage 通道——**功能不坏，但每次调用都先失败一次再重试**，控制台刷 `IPC custom protocol failed, Tauri will now use the postMessage interface instead`。补上后走原生通道。

### 为什么之前没人发现

`@hermix/preview` 是**桌面端与 viewer 共用**的包，桌面端 `tauri.conf.json` 的 CSP 早就带 `'wasm-unsafe-eval'` 和 `worker-src 'self' blob:`，所以同一份预览代码在主程序里正常渲染、在 viewer 里整页死掉。两份 CSP 各写各的，没有交叉校验，于是"主程序能用"掩盖了"viewer 根本起不来"。

**因此确立一条约束**：`apps/viewer/src-tauri/tauri.conf.json` 与 `apps/desktop-tauri/src-tauri/tauri.conf.json` 两份 CSP 属于同一份**共享渲染栈**的配置。凡是给 `@hermix/preview` 引入新的运行期资源（WASM、Worker、wasm 字体等），**必须同时确认两个 app 的 CSP 都放行**——只改一边等于把另一边打成白屏。

### 定位方法（可复用）

viewer 是 `windows_subsystem = "windows"`，无控制台、stderr 全丢，界面上也看不出任何错误。实测可行的取证路径：

1. `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"` 启动 viewer（WebView2 自身认这个环境变量）；
2. `curl http://127.0.0.1:9222/json/list` 拿 page target 的 `webSocketDebuggerUrl`；
3. CDP `Runtime.enable` + `Page.reload` 后收 `Runtime.exceptionThrown` / `Log.entryAdded`，并 `Runtime.evaluate` 读 `document.getElementById('root').children.length` 区分"白屏"与"没数据"。

`rootChildren === 0` = JS 求值链断了（模块级错误）；`rootChildren > 0` 但内容空 = 数据层问题。这是本次把"白屏"一步定位到 CSP 的关键。

### 附带坑：改了 `tauri.conf.json` 但配置没生效

修 CSP 时先改配置再 `cargo build -p hermix-viewer`，结果 **cargo 确实重编了 crate（`Compiling hermix-viewer`），编出来的 exe 里却还是旧 CSP**——查二进制字符串确认 `connect-src 'self' ipc:` 仍在、`http://ipc.localhost` 根本没有。

原因链条（两次都白编才定位出来）：

1. `tauri::generate_context!()` 是**宏**，它在**宏展开期**读 `tauri.conf.json`。
2. 本仓库的 `.cargo/config.toml` 设了 `rustc-wrapper = "<USERPROFILE>/.cargo/bin/sccache.exe"`（见 AGENTS.md 的 sccache 运维条目）。**sccache 的缓存键 = 源码 + 编译参数 + build script 输出**，**不含宏展开期读取的外部文件**。`tauri-build` 发的 `rerun-if-changed=tauri.conf.json` 只让 build script 重跑，build script 输出没变 → sccache 判定 **HIT** → 把第一次编译（嵌着旧 CSP）的 rlib 原样吐回来。
3. 所以 **"cargo 确实重编了 crate"（输出 `Compiling hermix-viewer`）并不代表配置生效**——sccache 命中时，rustc 根本没跑。

**坑上加坑：`cargo clean -p` 也没用。** 试过 `cargo clean -p hermix-viewer`（移除 1055 个文件、1.3GiB）后重新编译，**编出来的 exe 里还是旧 CSP**——因为对象还在 sccache 缓存里，源码哈希没变照样 HIT。

可靠做法：改完 `tauri.conf.json`（或任何 `generate_context!` 会读的配置）后，**用 `SCCACHE_RECACHE=true` 强制重编**：

```bash
SCCACHE_RECACHE=true cargo build -p hermix-viewer
```

（等价的笨办法是改一点源码内容，让 sccache 键变化。改 mtime 没用——rustc 增量缓存和 sccache 都按**内容**哈希。）

**验证手段**：改完别信"编译成功"，**直接扫 exe 二进制里的配置字符串**（CSP 这类字符串是明文嵌进去的），确认目标串在、旧的不在。本次就是靠扫二进制才发现"编译成功 ≠ 配置生效"。

```powershell
$ascii = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("target/debug/hermix-viewer.exe"))
([regex]::Matches($ascii, [regex]::Escape("http://ipc.localhost"))).Count
```

**2026-10-19 晚间补充：这不是"改了配置之后才会踩"，普通 `cargo build` 也会踩。** 修完界面层后跑 `cargo check --workspace --all-targets` + `cargo test --workspace --lib`，再去看 `target/debug/hermix-viewer.exe`，它已经变回 **2026-10-15 的旧二进制**（13.3MB，扫 `wasm-unsafe-eval` / `http://ipc.localhost` 都是 0 命中），而 `target/debug/deps/hermix_viewer.exe` 是当晚正确的新构建（19.1MB）。也就是说：**sccache 的缓存键不含"宏展开期读取的文件 + 嵌入的 frontend dist"，任何一次让构建指纹失效的操作（check / test / 换 flags）都可能让 cargo 恢复出一个更早的缓存产物并覆盖掉刚编好的 exe。**

实践含义两条：

1. **凡是要拿 `target/debug/*.exe` 去验证行为，必须先扫串**，不能假设"我刚编过"。
2. **交付/打包前一律 `SCCACHE_RECACHE=true cargo build`**，且打完立刻扫串。这一条对桌面端同样成立：`apps/desktop-tauri/src-tauri/tauri.conf.json` 改了 CSP / 外部二进制 / 窗口配置后，也要 `SCCACHE_RECACHE=true` 才生效。

**这条坑对桌面端同样成立**：`apps/desktop-tauri/src-tauri/tauri.conf.json` 改了 CSP / 外部二进制 / 窗口配置后，也要 `SCCACHE_RECACHE=true` 才生效。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 改 `packages/preview` 的 barrel，改成深路径导入（`@hermix/preview/viewers/MarkdownView`）以避开 satteri | 治标不治本：只要 viewer 渲染任何 Markdown，WASM 迟早要跑；且要动共享包 + 桌面端两处导入面，风险远大于加一条 CSP 指令。真正该做的是"barrel 按需拆包"（见 Consequences），那是独立的性能议题 |
| 在 `apps/viewer/src/App.tsx` 里给 `PreviewHost` 加 `ErrorBoundary`，把 WASM 崩溃降级成局部错误 | 崩溃发生在**模块求值期**（`import` 语句），不是渲染期，ErrorBoundary 根本来不及挂载——救不了 |
| 用 `try/catch` 包住 `import("satteri")` 做动态导入降级 | 同上：要让 satteri 变成动态导入，得改 `MarkdownView.tsx` 的静态 `import`，等于动共享包；且 Markdown 预览是核心功能，不该默认失效 |
| 放宽 CSP 到 `script-src 'self' 'unsafe-eval'` | `'unsafe-eval'` 会把整个 JS 字符串求值一并放开，攻击面远大于 `'wasm-unsafe-eval'`（后者只放行 WebAssembly 编译）。主程序已选后者，viewer 应对齐 |
| 删掉 viewer 里的 WASM 依赖（换纯 JS 的 Markdown 解析器） | 牵动共享包和主程序的渲染一致性；`satteri` 是主程序 Markdown 预览的既有实现，换 parser 属于独立的架构决策，不该夹带在一次白屏修复里 |

## Consequences

- **得到**：独立文件管理器（`hermix-viewer.exe`）恢复可用，共享预览栈的 WASM/Worker 资源在两个 app 里都放行。
- **代价**：viewer 的 CSP 多了 `'wasm-unsafe-eval'`（仅放行 WebAssembly 编译，不放行 JS 字符串求值）与 `worker-src 'self' blob:`；两者都是主程序早已在用的指令，不是新增风险面。
- **仍然遗留**（不在本次范围）：
  - 桌面端 CSP 的 `connect-src` 同样缺 `http://ipc.localhost`，主程序在 Windows 上同样靠 postMessage 回退（每次 invoke 多一次失败重试）。修法是同一个 token，本次未动主程序配置。
  - viewer 启动时经 barrel 把 `satteri` 的 3MB WASM + ExcelJS + pdf.js 一起拉进首屏（`@hermix/preview` 的 barrel 无按需拆分），文件管理器外壳要等 WASM 实例化完才首绘。正确解法是 `PreviewHost` 改懒加载，让"浏览文件"不等"预览引擎"——这是独立的性能议题。
- **门禁缺口**：目前没有任何检查能发现"一个 app 的 CSP 漏了共享栈需要的指令"。CI 里加一条比对两个 `tauri.conf.json` CSP 的脚本（或对 `@hermix/preview` 依赖做静态扫描）可根治这类复发，本次未做。
