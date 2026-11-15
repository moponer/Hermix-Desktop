# Decision Note: 独立查看器预览引擎懒加载 + 首调冷启延迟的完整表征

Status: implemented — 2026-10-19 `hermix-viewer.exe` 的预览引擎（`@hermix/preview` barrel）改为懒加载，首屏 JS 从 1.17MB 降到 242KB；同批定位了「首批 IPC 请求并行挂起 ~20s」的现象，根因未收敛

关联：[独立查看器白屏——CSP 缺 'wasm-unsafe-eval'](2026-10-19-viewer-csp-wasm-blank-screen.md)（同批排查的前一半：白屏的直接成因）

## Decision

### 1. 预览引擎改为懒加载

`apps/viewer/src/App.tsx` 里 `PreviewHost` 由静态 import 改为 `React.lazy` + `Suspense`：

```tsx
const PreviewHost = lazy(() => import("./PreviewHost").then((m) => ({ default: m.PreviewHost })));
```

`PreviewHost` 是启动路径上**唯一**的 `@hermix/preview` **运行时**引用（`App.tsx` / `fileSource.ts` 对该包都是 `import type`，编译期擦除）。而这个 barrel 拖进来的是 satteri 的 3MB WASM + ExcelJS 940KB + pdf.js 437KB + pptx + docx-preview。

结果（`apps/viewer/dist`）：

| | 改前 | 改后 |
|---|---|---|
| 首屏入口 chunk | `index-*.js` 1.17MB | `index-*.js` 242KB |
| 首屏 modulepreload | 6 个（含 exceljs / PdfView / docx-preview） | 4 个小文件（合计 ~13KB） |
| 预览引擎 | 首屏加载 | 拆成 `PreviewHost-*.js` 929KB 按需 chunk |
| satteri 3MB WASM | 启动即 fetch + 实例化 | 打开第一个预览时才 fetch |
| 首屏渲染（实测） | +1.46s | +0.94s |

与主程序 `main.tsx` 的分窗小图是同款思路：「浏览文件」不该等「预览引擎」。

配套改动 `apps/viewer/vite.config.ts`：补 `optimizeDeps.include`（docx-preview / exceljs / pdfjs-dist / pptx-browser / xlsx / papaparse）与 `optimizeDeps.exclude`（satteri 及其 wasm 链）。**这一步是必须的**：预览链改成纯懒加载后，这些包在 dev 下只能被运行时发现 → 中途重新优化 → 旧 hash 失效 → webview 拿过期地址 → 白屏，正是桌面端 2026-10-15 踩过的坑（见桌面端 vite.config.ts 同名注释）。`exclude` satteri 那组则是因为预打包会改写 `import.meta.url`，把 wasm 路径指到 `.vite/deps/` 下不存在的相对位置。

### 2. 冷启延迟：现象已定死，根因未收敛

懒加载做完后，冷启时间**没有改善**（21.5s → 26.4s），反而暴露了这个更基础的问题。用 `Page.addScriptToEvaluateOnNewDocument` 在页面脚本执行前注入探针、拦截 `window.fetch` 与 `__TAURI_INTERNALS__.invoke`，实测启动期的真实时序：

```
fetch http://ipc.localhost/viewer_take_pending   20117ms  ok
fetch http://ipc.localhost/viewer_drives          20115ms  ok
fetch http://ipc.localhost/viewer_drives          20114ms  ok
fetch http://ipc.localhost/plugin:event|listen    20113ms  ok
fetch http://ipc.localhost/viewer_list_dir        14ms  ok
```

**首批 4 个 IPC 请求全部并行挂起 ~20.1 秒、然后在同一刻同时释放；第 5 个起只要 14ms。** 也就是说卡的不是某条命令，而是「第一次 IPC 通道打通」这件事本身。

已排除的解释（每条都有实测支撑）：

- **不是 bundle 体积**：懒加载把首屏砍到 242KB，冷启反而更慢（26.4s vs 21.5s）。
- **不是 CPU 忙**：从 +4s 到 +22s 采样，全部相关进程只消耗 0.1s CPU——是**等**，不是算。
- **不是残留进程占锁**：清空全部 `hermix-viewer` / `msedgewebview2` 进程并等 35s 后冷启，仍是 26~31s。
- **不是系统 HTTP 代理**：本机代理为 `127.0.0.1:7897`，绕过表缺 `ipc.localhost`（`ProxyOverride` 里有 `localhost;127.*;…;<local>`）。但把 `ipc.localhost` 加进 `ProxyOverride` 后冷启无改善（26.4s），且 `curl` 直连 / 走代理到 `ipc.localhost` 的耗时差只有 0.9s 量级。（该注册表改动已还原。）
- **不是 WebView2 侧首帧未就绪**：外壳在 +0.94s 就渲染出来了，此时首批 IPC 才刚发出。

剩下的主要嫌疑是 Tauri v2 在 Windows 上的 IPC 自定义协议（`http://ipc.localhost`）**首次请求**的冷启开销——所有首批请求被同一个「协议尚未热起来」的门卡住，一放行就全放。但这条**尚未证实**（未能在 Tauri 源码里定位到对应的 20s 常量，也未能让桌面端复现同样量级：桌面端自己用 `std::env::set_var` 覆写 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`，调试端口进不去，探针接不上）。

### 3. 顺带发现：主程序有同款隐患，但被超时竞速掩盖

主程序 `apps/desktop-tauri/src/main.tsx` 的健康探测早就写着：

```ts
// 即使 `invoke("backend_ready")` 永久 pending，本次探测也会在 timeoutMs 后判定为 false
```

即**主程序早就遇到过 invoke 挂起**（用 2s 超时竞速兜住，代价是启动期可能白等 20s）。同一个 20s 首调冷启若也发生在主程序，启动屏会卡在「正在启动本地服务…」。本次未能对主程序做同量级实测（见上）。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 把 `satteri` 换成纯 JS 的 Markdown 解析器，根除 WASM | 牵动共享包与主程序的渲染一致性；`satteri` 是主程序 Markdown 预览的既有实现。换 parser 是独立架构决策，不该夹带在一次白屏/冷启修复里 |
| 把 barrel 拆成深路径导入（`@hermix/preview/viewers/MarkdownView`），避免 barrel 拖入 satteri | 只把问题推后：只要渲染 Markdown，WASM 迟早要跑；且要改共享包的导出面 + 桌面端两处导入。懒加载是同等效果下更小的改动面 |
| 在 `PreviewHost` 外层加 `ErrorBoundary`，把 WASM 崩溃降级成局部错误 | 崩溃发生在**模块求值期**（`import` 语句），ErrorBoundary 挂载前就炸了，救不了 |
| 首屏用兜底数据（硬编码盘符列表）先把树画出来，IPC 通了再刷新 | 盘符是 `GetLogicalDrives()` 的运行时结果，硬编码会撒谎（网络盘/移动盘会错）；且仍要等 IPC 才能列目录，只能糊弄"看得见"不能糊弄"用得了" |
| 给首个 IPC 调用加超时竞速，超时后重试 | 超时只会让用户看到空树然后再等一轮；根因在协议首调，不在应用层。桌面程序已有此模式（`checkOnce`），但那是兜底不是解法 |
| 删掉 `optimizeDeps` 相关的 `exclude`（satteri 那组） | 会让 dev 模式的 wasm 路径指向不存在的相对位置，`WebAssembly.instantiate` 报 "expected magic word"。必须保留 |

## Consequences

- **得到**：查看器首屏负载降 5 倍，首屏渲染提前约 0.5s；预览引擎（3MB WASM + 2.6MB JS）只在真正打开文件时才加载；预览功能回归验证通过（AGENTS.md 完整渲染，零异常）。
- **代价**：多一层懒加载 chunk；dev 模式必须正确配置 `optimizeDeps`（本次已配）；`chunkSizeWarningLimit` 放宽到 2600（预览 chunk 合法地超过默认 500KB 警告线）。
- **仍然遗留**（本次未解决，需后续单独立项）：
  - 首批 IPC 并行挂起 ~20s 的根因未定位。本机上打开查看器要等 ~26s 才出目录树；**很可能主程序同样受影响**（主程序代码里有"invoke 可能永久 pending"的既有注记），但未实测确认。
  - 没有 CI 门禁能发现「一个 app 的 CSP 漏了共享栈需要的指令」或「懒加载链缺 optimizeDeps.include」。
  - 主程序 CSP 的 `connect-src` 同样缺 `http://ipc.localhost`（Windows 上 Tauri IPC 的真实端点），主程序也一直靠 Tauri 的 postMessage 回退跑 IPC。
