# Decision Note: dsh 转换引擎改为运行时按需下载

Status: implemented — 安装包与更新包不再携带引擎；首次文档转换时下载到 hermix home（2026-10-23）

## Decision

dsh-libreoffice-kit 引擎（190MB / 800 文件）从**安装包与更新包中移出**，改为运行期首次使用时下载：

- **构建链摘除**：`tauri.conf.json` 的 `resources/dsh-engine/**` 移除；`ensure-sidecar.mjs` 不再调用 `fetch-dsh-engine.mjs`；`release.sh` 的 resources 复制对 `dsh-engine` 显式 `/XD` 排除（本地调试残留不得静默入包）。
- **运行期获取**（`crates/hermix-tools/src/tools/dsh_engine_fetch.rs`）：首次 Office→PDF 或 PDF→图片时下载 `@deepseek-ai/libreoffice-kit-win32-x64@0.2.0` tgz（70.8MB，npmmirror 优先、npmjs 兜底）→ npm `dist.integrity` sha512 逐字节比对 → `%SystemRoot%\System32\tar.exe` 解压到 `.staging/` → 结构断言（`bin/dsoffice.exe` + `program/program/uno.ini`）→ 同卷 rename 到 `<hermix home>/dsh-engine/package`。半截下载落 `.part`，不会冒充成品。
- **串行化**：进程内 `tokio::Mutex` + 双重检查，多会话同时首调只下载一次。
- **老用户兼容**：`locate_dsh_engine()` 的候选顺序保留 `<exe>/resources/dsh-engine/package` 在 hermix home 之前 —— ≤0.1.20 装过的机器上那份引擎继续生效，升级不重下。
- **预算**：`per_tool_budget("file_converter")` 300s → 600s，把「下载 71MB + 解压 190MB」算进首次调用。
- **关闭开关**：`HERMIX_DISABLE_ENGINE_FETCH`（离线/受控环境；置非空且非 `0` 即拒绝下载，报错文案带变量名）。
- **执行中可见**：`ToolCallContext` 新增通用 `progress_cb` + `active_tool`（`crates/hermix-tools/src/context.rs`），`LocalAgentService` 在 `send_message_stream` 里接上 WS 通道、在每次 `tool_start` 写入活跃工具；`file_converter` 由此 opt-in `ContextAware`，下载/校验/解压各阶段推一行进度，前端那张卡片实时显示（`tool_stdout` 事件）。进度是**瞬时**通道：只走 WS、不写 `execution_timeline`（回放里的"下载 48%"是噪音）。

## Alternatives considered

- **继续捆绑在安装包里**（2026-10-17 的原始做法）：安装日志刷出 800 行解压明细、安装变慢、每次自动更新都要多背 190MB（更新包是全量 zip）、大体积单 exe 更容易被 Defender 拦。
- **打成单个 zip 资源、首次启动解压**：只把解压动作从安装器挪到首启，安装包体积、更新带宽、磁盘占用三个问题都没解决。
- **要求用户安装 LibreOffice**：与便携零安装定位冲突，默认环境必失败（旧实现，2026-10-17 已否决）。
- **改成 `codex exec` 类外部后端**：与引擎无关，本笔记不涉及。

## Consequences

- 安装包压缩后省 ~70MB，安装时少解压 800 个文件；更新 zip 同步瘦身。
- 首次文档转换需联网一次，本机实测下载+校验+解压共 64.7s；之后完全离线。首调变慢属预期，不是卡死（工具超时已放宽到 600s）。
- 失败路径分档可见：镜像逐个重试，最终报错带每个镜像的失败原因（HTTP 码 / sha512 不匹配）；不再出现"未找到引擎，请运行某个构建脚本"这种面向开发者的提示。
- 老用户机器上旧的 `resources/dsh-engine`（190MB）不会被自动清理，仍按候选 2 生效；要回收磁盘需手工删该目录。
- CI 的 dsh smoke 测试在引擎缺席时照旧跳过；验证真实下载链用 `cargo test -p hermix-tools --lib engine_download_end_to_end -- --ignored --nocapture`（会联网取 71MB）。
- 进度事件的 `tool` / `tool_call_id` 取"当前活跃工具"这一共享槽位：多工具并行时，后启动者会覆盖先启动者，进度可能记到错误的卡片上。只影响显示、不影响结果；要精确需把 `ToolCallContext` 拆成 per-call 副本，本轮不做。
- 引擎版本 pin 现在是两处（`dsh_engine_fetch.rs` 与 `fetch-dsh-engine.mjs`），升级需同步改。
