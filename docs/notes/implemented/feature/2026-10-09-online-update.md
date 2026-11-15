# Decision Note: 桌面端在线更新（提示 + 用户触发 + 进度 + 重启）

Status: implemented — 已拍板（2026-10-09）；桌面端四段更新 + 手机端 APK 更新，随实施保持同步

## Decision

采用**自管 zip 整包更新**，不用 `tauri-plugin-updater`。分四段：

### 1. 版本清单（发现新版本）

- 清单端点：`GET https://hermix.cssso.com/desktop/manifest.json`（**客户端内置默认**，与手机端 pairing 的 DEFAULT_RELAY 同域；可被 env / app 根 update-url.txt 覆写）。部署形态（2026-10-09 与主人确认）：relay 服务器（宝塔，`/www/wwwroot/hermix-relay/`）已有 Node relay 进程持有一个 HTTPS 域名+端口，**不新开端口/域名**——静态文件放 relay 的 `public/desktop/`（relay 已有 `public/hermix.apk` 静态先例，`server.js:70-123`），relay 加静态路由；zip 下载地址固定写 `.../desktop/latest.zip`（文件名恒定，manifest 的 url 字段永不变更，发版只覆盖两个文件）。relay 仍零业务逻辑——分发只是静态文件，不进路由代码。
- 清单格式（`notes` 为 Markdown 更新日志，前端用现有渲染器展示）：

```json
{
  "version": "0.2.0",
  "notes": "修复…；新增…",
  "pub_date": "2026-10-09T00:00:00Z",
  "url": "https://<relay-host>/desktop/hermix-desktop-0.2.0-win64.zip",
  "sha256": "<zip 摘要>",
  "min_version": "0.1.9"
}
```

- 检查时机：启动后延迟 30s 查一次 + 每 2h 一次；失败静默不重试（便携产品不强打扰）。设置页有「自动检查更新」开关（默认开）与「检查更新」手动按钮。
- 强制/可选双模式，由 manifest 的 `min_version` 驱动：普通新版（version > current）= 可选，菜单区非阻断徽标，用户可忽略；`current < min_version`（发布方调高服务器上的 min_version 即可，无需发新版）= 强制，启动即查、立即弹全屏阻断对话框（只有「立即更新」，无关闭）。便携 zip 用户拦不住，但在线检查通过的用户可拦——发布方始终保有强制手段。
- 更新日志来源：发布脚本从 `CHANGELOG.md` 顶部段落自动提取写入 manifest `notes`（单一真相源）；提取失败回落脚本参数手传。
- 提示 UI：侧栏底部用户菜单区域挂「有新版本 x.y.z」徽标（非阻断），点击进更新面板（版本号 + Markdown 渲染的更新日志 + 「立即更新」）。

### 2. 下载（用户点击后才开始）

- 点击「立即更新」→ 下载 zip 到 `<app根>/update.pending/`。
- 进度：读 `Content-Length`，按已收字节流式回报，前端经 Tauri event 收 `{received, total}` 渲染进度条。
- 校验：下载完算 sha256 与清单比对，不符即删缓存报错——这是信任边界校验，属不可削减项。

### 3. 落地交换（Windows 自替换难题）

进程运行中不能覆盖自己的 exe。方案：**随包带 updater 脚本，由它做交换**：

1. zip 校验后解压到 `update.pending/extracted/`；
2. 启动 helper（优先复用包内已有 `resources/loader/hx-shell.exe` 跑脚本；不行则退到 `powershell -WindowStyle Hidden`）；
3. 当前应用确认 helper 已起（PID 握手）→ 正常退出（走既有 sidecar 收尾）；
4. helper 等 PID 退出 → 旧目录改名 `update.backup/` → 搬入新文件 → 启动新 exe → 新 exe 起来后删 `update.backup/`。

新版本首启自报「更新成功」，失败（新 exe 30s 内没起来）由 helper 回滚 backup 并启动旧版。

### 3.1 卸载器保留（NSIS 安装用户）

zip 整包交换会把 app 根内容整体搬进 `update.backup` 再用新 zip 覆盖。NSIS 安装器生成的
`Uninstall.exe` 住在安装目录、且**不在更新 zip 内**（由安装动作产生，非 app 载荷）。若随旧
目录搬走，更新后 Windows「卸载」入口（`UninstallString` 指向它）即变死链——「装过安装器 +
走过一次自动更新」的用户点卸载会文件找不到。

缓解（`updater.rs` 的 `apply-update.ps1`，2026-10-14）：swap 第 2 步与回滚第 5 步都把
`Uninstall.exe`（及旧自研 `uninstall.exe`）排除在搬移 / 删除之外，使安装目录里的卸载器在每次
更新后原样保留。便携模式安装目录无卸载器，此排除为空操作，行为不变。**用户数据在 `~/.hermix`，
不在安装目录，交换过程触碰不到，不受影响。**

残留：保留的是**初始安装版本**的卸载器，其内部文件清单是首装版本的文件集合。后续 zip 更新若
新增 / 改名文件，卸载时这些文件不会被旧卸载器清掉（孤儿文件），但用户数据不受影响。完整解法
见下方「待评估」。

### 4. 重启与状态

- 会话/配置全部落 `~/.hermix` 与 app 根数据目录，不在内存——重启天然不丢状态。
- 更新中的保护：流式进行中禁用「立即更新」按钮；sidecar 二进制（hermix/officecli/markitdown/usn-indexer）随 zip 整包走，与现有 `ensure-sidecar` 打包链一致，**不允许单独更新 sidecar**。

### 发布链路

`scripts/release.sh` 一条链（构建 → 自组装 app 根 → A 路出 latest.zip + manifest → 上传 relay 静态目录；B 路由 `cargo tauri bundle -b nsis` 出 NSIS 安装包，见[回到 NSIS：自研安装器退役](../simplification/2026-10-14-retire-custom-installer-return-to-nsis.md)），杜绝 2026-10-06/08 那类「修了但包里没有」。

### 手机端（Hermix M）自动更新（2026-10-09 补，与主人确认）

同一个 manifest 扩展 `android` 段，APK 与桌面 zip 同目录（relay `public/` 已有 APK 下载先例）：

```json
{
  "version": "0.2.0", "...": "...同上...",
  "android": {
    "apk_url": "https://<relay-host>/static/hermix.apk",
    "sha256": "<apk 摘要>",
    "min_version": "0.1.0"
  }
}
```

- 检查与提示：App 启动时拉同一份 manifest，`version` 高于自身即提示（App 内横幅/设置页红点），点击下载 APK（进度条复用桌面端同一套 event 形态）。
- 安装：Android 侧下载完校验 sha256 → 发系统安装 intent（`REQUEST_INSTALL_PACKAGES` 权限，用户首次需授权「允许安装未知应用」——此为 Android 平台约束，绕不开）→ 用户确认安装。**Android 无应用内自替换机制，系统安装器确认这一步是平台强制，不可省**。
- 强制/可选：与桌面端同规则（`min_version` 驱动）；App 版本过旧时桌面端可在 WS 握手时发「版本过旧，请升级」引导帧（协议已有 `proto_version` 协商先例）。
- 发布链路同脚本：`scripts/release.sh` 同时上传 zip 与 APK 两个产物（APK 由现有安卓构建产出）。

## Alternatives considered

| 方案                           | 否决理由                                                                                                                                                                             |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **tauri-plugin-updater（官方）** | Windows 端只支持应用 NSIS/MSI **安装器**，不支持 zip 整包替换；本产品身份是「Zip, copy, run」便携零安装（bundle.longDescription 明写），用户自选目录解压，安装器会把应用装去固定位置并破坏「数据贴着 exe」的模型。另需引入 minisign 签名双钥基建。若未来转安装器分发，此方案复活。 |
| **增量/差分更新（bspatch 类）**       | 18 crate + 4 sidecar 的整包 zip 量级（数十 MB）不值得引入差分算法与多版本 patch 维护面；YAGNI。                                                                                                             |
| **只更新前端资源（webview 资源热更）**    | UI 嵌在 exe 里、Rust 侧占大头，资源热更覆盖不了 sidecar 与 Rust 改动，会出现「前端新逻辑调不通旧 sidecar」的版本分裂，正是 2026-10-06/08 事故的形态。                                                                             |
| **GitHub Releases 做清单/托管**   | 目标用户网络环境访问 GitHub 不稳；relay 主机已在位，复用成本更低。                                                                                                                                         |
| **自动静默更新**                   | 违背「用户点击才启动更新」的需求；便携场景也无安装时机保证。                                                                                                                                                   |

## Consequences

- **得到**：与便携模型完全一致（只动 app 根目录内文件）；UI 可控的四段体验（提示→确认→进度→重启）；sidecar 与主程序永远同版本。
- **放弃**：官方 updater 的签名生态（minisign）——一期只做 sha256 over HTTPS，**放弃防中间人篡改的能力**；若分发面扩大，二期应加 ed25519 签名（可复用 relay_crypto 的 P-256 经验）。
- **放弃**：跨平台——helper 脚本只写 Windows（与项目 Windows-only 边界一致）。
- **风险**：杀软对「脚本替换主程序目录」可能告警；helper 用包内已带、白名单相对友好的 hx-shell 优先验证。
- **回滚保障**：backup 目录机制保证最坏情况回到旧版本；更新永远不触碰 `~/.hermix` 用户数据。
- **卸载器保留**：NSIS 安装的 `Uninstall.exe` 不在更新 zip 内，swap / 回滚两步都把它排除在
  搬移之外，避免更新后卸载入口变死链（见 §3.1）。

### 待评估：NSIS 安装用户的「干净卸载」完整解法

当前保留旧卸载器的方案消除了死链，但卸载时可能留孤儿文件（见 §3.1 残留）。若要每次更新都让
卸载器与当前版本一致，需在更新路径上**改为静默重跑新版 NSIS 安装器**（`Hermix-Setup-<ver>.exe /S`），
由安装器就地覆盖文件并重新生成 `Uninstall.exe` 及其文件清单。这要求 manifest 额外携带 setup 包
地址、`updater.rs` 区分「安装版 vs 便携版」并改走安装器自更新——属更新模型调整，未纳入当前范围。
