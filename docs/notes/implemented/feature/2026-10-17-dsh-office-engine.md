# Decision Note: Office→PDF 采用 dsh-libreoffice-kit 引擎

Status: implemented — 引擎获取脚本、file_converter 集成、定位契约与测试均已落地（2026-10-17）

## Decision

`file_converter` 的 Office→PDF 主路径改为 **dsh-libreoffice-kit 预裁剪 LibreOffice 引擎**（`deepseek-ai/dsh-libreoffice-kit`，MPL-2.0），首次使用时下载一次、之后运行期完全离线，**不再要求用户安装 LibreOffice**。系统 LibreOffice 探测保留为回退（保真度历史路径）。

- **引擎获取（运行期）**：`crates/hermix-tools/src/tools/dsh_engine_fetch.rs` —— 首次 Office→PDF / PDF→图片时下载 `@deepseek-ai/libreoffice-kit-win32-x64@0.2.0` tgz（70.8MB，npmmirror 优先、npmjs 兜底）→ npm `dist.integrity` sha512 校验 → System32 bsdtar 解压到 `.staging/` → 同卷 rename 到 `<hermix home>/dsh-engine/package`。pin 升级 = 改 `VERSION` + `INTEGRITY` 两行。分发策略见 [2026-10-23 按需下载](2026-10-23-dsh-engine-on-demand-download.md)。
- **引擎获取（开发/验证）**：`apps/desktop-tauri/scripts/fetch-dsh-engine.mjs` 仍在，供本机取一份引擎到 `src-tauri/resources/dsh-engine/package` 做 smoke 验证（`HERMIX_DSH_ENGINE` 指过去），**不再由构建链调用、不进安装包与更新包**。
- **Rust 集成**（`crates/hermix-tools/src/tools/file_converter.rs`）：`locate_dsh_engine()` 按序探测 `HERMIX_DSH_ENGINE` env → `<exe>/resources/dsh-engine/package`（≤0.1.20 旧安装包布局，保留以免老用户重下）→ `<exe>/dsh-engine/package` → `<hermix home>/dsh-engine/package`（按需下载的正式位置）；`convert_with_dsh_engine()` 用纯 CLI 契约驱动 `dsoffice.exe`（`--program-directory` 指内层 `program/program`、私有 profile 作 HOME/USERPROFILE/TMP/TEMP、stdout 末行 JSON 判定、参数对齐 adapter 默认 limits）。
- **availability 如实化**：转换失败不再谎报；dsh 失败回退 LibreOffice，两者皆缺时报错附获取引擎的指引。

## Alternatives considered

- **要求用户安装 LibreOffice**（旧实现）：与便携零安装定位冲突，默认环境必失败——正是"没有一件事能完美做好"的具体来源。
- **pandoc sidecar**：pandoc 不做 docx→PDF（需另配 LaTeX），且 DeepSeek/ZCode 均不用——出局。
- **自建裁剪配方编译 LibreOffice**：数周级工程 + 多平台 CI 矩阵；dsh 已发布现成载荷且明确许可应用构建者捆绑——不重复造轮子。
- **运行时按需下载**（首用下载到 hermix home）：曾在 2026-10-17 保留为第 4 候选，2026-10-23 正式采用，见 [按需下载笔记](2026-10-23-dsh-engine-on-demand-download.md)。

## Consequences

- 安装包与更新包不再携带引擎（压缩后省 ~70MB、安装少解压 800 个文件）；代价是首次文档转换需联网下载一次。
- 引擎版本 pin 在 0.2.0；上游升级需手动改 `VERSION` + `INTEGRITY` 并复验 smoke 测试。
- Windows 引擎需 MS VC++ v14 运行库（与主程序 MSVC 链接依赖一致，无新增要求）。
- `render`（Office→PNG 按页）与 `recalculate`（表格重算）走同一契约，后续作为新能力接入工具面（未实现，仅引擎能力就位）。
- smoke 测试 `dsh_engine_txt_to_pdf_smoke` 引擎缺席时自然跳过（CI 无引擎）；本机验证设 `HERMIX_DSH_ENGINE` 或先跑 fetch 脚本。

## Reintroduction conditions

无（本笔记是能力引入而非移除）。若上游 dsh-libreoffice-kit 改变引擎载荷结构（prebuilds.json schema / dsoffice.exe 契约），按其 packages/entry/src/native.ts 更新 `convert_with_dsh_engine` 的参数表即可。
