# Decision Note: 自研安装器（hermix-installer）+ 每次构建双产物（安装包 + 更新包）

Status: implemented — 已拍板（2026-10-09）；自研安装器 + 每次构建双产物（安装包/更新包），随实施保持同步

## Decision

新增 `hermix-installer` crate（自绘 UI 单文件安装器），NSIS 从分发链完全退场；`release-update.sh` 升级为 `release.sh`，每次构建从同一个「组装好的 app 根」分叉出两个产物。

### 双产物流水线

```
npm run build（前端 + ensure-sidecar）
→ cargo build 主程序
→ 组装 app 根 staging（hermix.exe + 4 sidecar + resources + icons + update-url.txt）
   ├─→ A 路（更新包）：latest.zip + manifest.json → relay /desktop/  （老用户，UI = 应用内 UpdatePanel）
   └─→ B 路（安装包）：zip 以 PE 资源节嵌入 installer → Hermix-Setup-<ver>.exe （新用户）
```

- 更新包 UI 不需要新做——就是已交付的 UpdatePanel（在线更新笔记同日 implemented）。
- **NSIS 在更新链里的历史角色只是"文件搬运工"**（解它拿 app 根组装目录），自组装后这一步删除。

### 安装器本体

- **单文件自解压**：app 根 zip 嵌入安装器 exe 的 PE 资源节——复用 `hx-packager`/`hermix-plugin` 的 `HxShellPackager` 既有能力（PE 资源嵌入 HXAM 归档），不新造嵌入机制。
- **UI 用 Tauri 小窗口**（单窗、定尺寸）：React + Tailwind 设计语言与 UpdatePanel 同族（浅色 + slate-700 语义 token，跟随主题变量）。WebView2 前提：Win11 预装、Win10 绝大多数存在；缺失时安装器降级系统 MessageBox 引导安装 WebView2（主程序同样依赖，绕不开）。
- **流程**：欢迎页（logo+版本）→ 许可（现有 license.rtf）→ 目录选择（**默认 `%LOCALAPPDATA%\Programs\Hermix`**；选 Program Files 时警告"后续静默更新需管理员"）→ 解压进度（流式 + sha256）→ 完成页（运行/桌面快捷方式勾选）。
- **卸载**：同一 exe `--uninstall` 模式；注册 `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Hermix`（per-user 免 UAC），添加删除程序可见。安装器自身随 app 根走，更新交换后卸载入口仍在。
- **默认目录选用户目录的原因**：更新交换 = 整目录替换，Program Files 下每次更新都要提权；`%LOCALAPPDATA%` 保住已实现的免 UAC 静默交换（update-install helper）。

### NSIS 退场步骤

1. 自组装 app 根（按 tauri.conf 的 externalBin/resources 清单复制 target/release 产物）
2. 与 NSIS 产物做一次**文件清单对照**（防漏 WebView2Loader.dll 等打包期注入文件）
3. 对照通过 → `tauri.conf.json` 删 nsis target、发布脚本删解 NSIS 步骤

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| NSIS 自定义页面/皮肤 | NSIS UI 定制能力弱，做不出设计感；且引入 NSIS 脚本维护面，与"自设计"诉求南辕北辙 |
| egui/原生窗口安装器 | 免 WebView2 依赖、exe 更小，但 UI 风格另起炉灶，无法复用 React 设计语言；主人对安装界面设计感要求高，Tauri 窗口开发效率与还原度都占优 |
| Inno Setup / WiX 换壳 | 同为脚本式安装器，UI 定制天花板与 NSIS 同级，只是换了一套模板语法 |
| MSI | 企业级但 UI 几乎不可定制，与需求相反 |

## Consequences

- **得到**：安装体验品牌一致（首个触点即设计语言）；每次构建自动出双产物且同源（安装的新 = 更新的新，无版本分叉）；NSIS 从仓库消失。
- **放弃**：NSIS 自带的 WebView2 自动静默引导（改为检测 + MessageBox 引导，交互多一步）；Per-machine 安装选项（一期只做 per-user，装 Program Files 需自担更新提权）。
- **风险**：PE 资源嵌入大 payload（~200MB）后单文件安装器体积等同 app 根（与 Electron 系安装器同量级，可接受）；杀软对未签名单文件自解压器的告警（分发面扩大后接代码签名，与在线更新笔记的签名演进同一议程）。
- **边界**：安装器不写 `~/.hermix` 用户数据；重装/升级覆盖安装时用户数据天然保留（数据贴着 exe 的便携模型不变）。

## 相关笔记

- [桌面端在线更新](../implemented/feature/2026-10-09-online-update.md)——更新包 A 路的完整决策；本篇只接手 B 路与组装环节。
