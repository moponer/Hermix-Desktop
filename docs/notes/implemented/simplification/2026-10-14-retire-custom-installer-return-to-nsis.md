# Decision Note: 回到 NSIS —— 自研安装器退役

Status: implemented — 已拍板（2026-10-14）；NSIS 为唯一安装包路径，
`apps/installer`（Tauri 安装器 app + vite 前端 + `hermix-embed` 载荷嵌入 CLI）与
`hermix-installer` crate 退役

## Decision

**NSIS（Tauri 内置打包，`bundle.targets: ["nsis"]`）是本产品唯一的安装包路径。**
自研安装器整条线退役：

- 删除 `apps/installer/`（Tauri 向导 app + React 前端 + `src-tauri/src/bin/hermix-embed.rs`
  载荷嵌入 CLI）；从 workspace `members` 摘除 `apps/installer/src-tauri`
- `release.sh`：删除安装器构建步骤（`npm run build:vite` + `HERMIX_PRODUCT_VERSION=… cargo
  build -p hermix-installer`）；**B 路由「zip 以 PE 资源节嵌入安装器」改为
  `cargo tauri bundle -b nsis`**，产物 `bundle/nsis/Hermix Desktop_<ver>_x64-setup.exe`
  拷为 `dist/Hermix-Setup-<ver>.exe`（沿用旧产物名，分发/冒烟话术不变）；删除
  `--nsis-baseline`「退场对照」（该门当初为 NSIS→自研的清单对账而设，方向已反）
- 许可文本 `LICENSE.txt` 从 `apps/installer/src-tauri/` 迁到
  `apps/desktop-tauri/src-tauri/`（`release.sh` 仍拷进 app 根，随包分发同一份）
- 品牌化随 NSIS 走：`installer/header.bmp` + `installer/sidebar.bmp` +
  `installer/hooks.nsh`（`installerHooks`）+ `bundle.copyright`（渲染进 `BrandingText`）

**在线更新（A 路 `latest.zip`）不受影响**：客户端 `updater.rs` 全程零引用安装器——
拉 `manifest.json` → 下载 zip → sha256 校验 → 内联 PowerShell（`apply-update.ps1`）
自替换；更新包内容（主 exe + 2 sidecar + markitdown + resources + LICENSE.txt +
update-url.txt）本就不含安装器。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| **自研为准**（文档现状）：删 nsis target，出包只认 `release.sh` 的 B 路 | 与最近实际分发不符——自研安装器最后出包停在 0.1.17（2026-10-12 10:52），此后 0.1.18/0.1.19 的实际产物是 NSIS；且 NSIS 品牌化已交付并验证 |
| **维持两条路径并存** | 这正是要消除的状态：同一仓库两套「怎么出包」的说法（README 说出自研、`AGENTS.md` 写 `cargo tauri build`），照哪份文档做就出不同的包 |
| 把自研安装器改造成 zip 更新链的一环 | 更新链用「PowerShell 自替换 + backup 回滚」已工作，安装器没有可插入的位置；PE 资源嵌入大 payload 的形态只增体积 |
| 只删构建步骤、源码原地保留 | 留下一个不再编译的 Tauri app，长期腐化且持续误导读者；simplification 的默认动作是删代码 |

## Consequences

- **得到**：单一安装路径与单一「怎么出包」的说法；少维护一个完整 Tauri app（含其 vite
  前端与 PE 嵌入工具）；`release.sh` 少一段退场对账逻辑。
- **放弃**：自研安装器的品牌化安装 UI（Tauri React 向导、与 UpdatePanel 同族的设计语言）
  与 per-user HKCU 卸载登记——改由 NSIS 的向导页与卸载条目承担。
- **风险（既有，已缓解）**：zip 整包更新会把 app 根内容整体搬进 `update.backup`
  再用 zip 覆盖；**落在安装目录内的 `Uninstall.exe` 会在更新后丢失，卸载入口对
  「装过安装器 + 走过一次自动更新」的存量用户可能变死链**。NSIS 同构（卸载器同样住
  安装目录）。已通过 `updater.rs` 的 `apply-update.ps1` 在 swap / 回滚两步把卸载器
  排除在搬移之外消除死链（残留=旧卸载器文件清单可能留孤儿文件，见
  [在线更新 §3.1](../feature/2026-10-09-online-update.md)）。
- **不动**：`apps/hx-packager` 与 `crates/hermix-plugin`（Custom App 打包仍在使用；自研
  安装器只是**曾**复用其 PE 资源嵌入能力，两者不是同一功能）。
- 仓库与出包链中「自研安装器」的位置全部清理：`Cargo.toml` members、`release.sh`、
  README、`AGENTS.md`、`AGENT-RUNBOOK.md`、官网 `docs.js`。

## Reintroduction conditions

出现以下任一证据时，应重新评估引入自研安装器：

1. **安装体验成为硬约束**：需要 zip 更新链与 NSIS 都给不出的能力（per-machine 安装、
   企业 MSI 分发、安装期自定义动作），且 NSIS 脚本定制确认覆盖不了。
2. **NSIS 的维护成本反超**：Tauri 每次升级都改 NSIS 模板契约、导致 `installerHooks`/
   品牌字段频繁失效并需手工 merge，使其长期成本高于自研。
3. **信任链要求升级**：分发面扩大到需要代码签名与安装器级信任链，而 NSIS 生态不满足。

重新引入时**不得**回到「每次构建双产物 + 把整包 zip 以 PE 资源节嵌入安装器 exe」的旧形态，
除非该形态的收益（单文件自解压、设计语言同族）被重新证明——历史上它把安装包体积推到与
app 根同量级，且引入了一条与在线更新平行、需各自维护的分发支线。

## 相关笔记

- [桌面端在线更新](../feature/2026-10-09-online-update.md)——A 路（更新包）的完整决策；
  本次只动 B 路（安装包）与组装环节，A 路不受影响。
- [自研安装器（已归档）](../../archived/feature/2026-10-09-custom-installer.md)——被本篇取代。
