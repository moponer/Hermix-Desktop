# Decision Note: viewer 顶部路径栏改「万能输入框」+ 接入本地全盘索引搜索（类 Everything）

Status: implemented — 路径栏一个框吃下三类输入（网址 / 绝对路径 / 搜索词）；搜索默认走本地文件索引（跨卷毫秒级），索引不可用时退回当前目录搜索并把「搜的范围」明示在界面上。

关联：[hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md)、[整合 FilePulse 文件搜索（hermix-filesearch）](../feature/2026-10-14-integrate-filepulse-file-search.md)

## Decision

**1. 顶部路径栏是万能输入框，按输入形态分派**（`apps/viewer/src/App.tsx::onPathSubmit`）：

| 输入形态 | 判别 | 行为 |
|---------|------|------|
| 网址 | `^(https?://|www\.)` | 系统浏览器打开（`www.` 自动补 `https://`） |
| 绝对路径 | `^[A-Za-z]:[\\/]` 或 `\\\\` 前缀 | 目录 → 进资源管理器；文件 → 开预览标签并**跟着定位**（反向同步，见下）；路径不存在 → 当搜索词 |
| 其余 | — | 搜索词（转交搜索链路） |

以前这个框只认「目录」一种形态：传文件路径会报「不是目录」，用户只能自己在中间栏里翻。一个框能吃下三类输入，是因为**这三种是用户真正会往地址栏里打的东西**（浏览器习惯把网址往地址栏打，Explorer 习惯把路径往地址栏打）。

**2. 搜索默认走本地全盘索引，不走目录遍历**。索引服务是既有的 `hermix-filesearch.exe`（提权守护进程，MFT 建索引），跨提权边界继承不了 stdio → 走 `127.0.0.1` 回环 TCP + token 握手，端口与 token 由它写进 `%LOCALAPPDATA%\hermix\filesearch\endpoint.json`。viewer 新增 `apps/viewer/src-tauri/src/filesearch.rs` 作为客户端（协议与 `crates/hermix-server/src/filesearch.rs` 完全一致，同一个 helper、同一份 JSONL 帧），命令 `viewer_search_global`。

**3. 索引不可用时退回当前目录搜索，并且把范围说清楚**。搜索框右侧常驻一枚范围标签：`全盘索引命中 N 条` / `仅在当前目录内搜索（原因）`。用户必须一眼看出「搜到的是不是全盘」——两者结果范围差几个数量级，混为一谈会让人以为「搜不到就是没有」。

**4. 反向路径同步**：文件从**别处**被打开（拖入 / 主程序「在查看器中打开」/ 命令行 / 单实例投递）时，中间栏切到它的父目录，左侧树逐级展开到该目录。树的 key 必须用后端 `listDir` 返回的 `e.path`（盘符根带尾反斜杠、UNC 有前缀，自己拼会命中失败）。纯净模式不渲染树与中间栏，只记当前目录（切重量模式时目录已经是对的）。

**5. 驱动器列表显示用户自己的卷标**（`GetVolumeInformationW`），只对**本地盘**（`DRIVE_FIXED` / `DRIVE_REMOVABLE` / `DRIVE_RAMDISK`）调用；网络盘与光驱保留类型名。

**6. helper 随桌面端一起打包**：`externalBin` 加 `binaries/hermix-filesearch`，`ensure-sidecar.mjs` 构建并逐字节校验。此前它没进 `externalBin` 也没进打包脚本——release 版的「索引」功能其实是死的。

## Alternatives considered

1. **viewer 直接读索引快照**（`%LOCALAPPDATA%\hermix\filesearch\indexes\<VOL>.bin`）——拒绝：快照 150–250MB，读一次尖峰就把查看器的内存吃满；索引还有 30s 滞后，且拿到的只是构建期过滤后的集合，没有实时排序。走 helper 才有「精确名 > 前缀 > 其他子串，目录优先」的三档排序。
2. **viewer 自己建索引（目录遍历）** ——拒绝：`crates/hermix-filesearch/src/core/scanner.rs` 的 `scan_directory` 在上游**没有调用点**（死代码），MFT 路径又必须管理员。复用现成的提权 helper 是唯一不重复造轮子的路。
3. **让 viewer 走 hermix-server 的 HTTP `/v1/filesearch/search`** ——拒绝：桌面端 server 跑 `--stdio` 模式，**不绑任何 TCP**，这条路在桌面场景下根本不通；而且 viewer 已是独立进程，不该依赖主程序是否在跑。
4. **让 viewer 自己拉起一个不提权的 helper** ——拒绝（列为后续）：MFT 建索引必须管理员，不提权就只能目录遍历（慢一到两个数量级）。当前做法是**首次搜索时弹一次 UAC**，之后 helper 常驻、搜索毫秒级——这正是 Everything 的安装体验。
5. **搜索只在当前目录内做** ——拒绝：用户找的文件多半不在当前层，「在这一层里翻一遍」等于没有搜索。
6. **路径栏加第二个独立搜索框** ——拒绝：顶部已经有搜索框，再加一个等于两个入口、两套心智模型。合成一个「万能输入框」，靠形态判别分派。
7. **绝对路径解析交给 Rust 新增一条命令** ——拒绝（六步阶梯 YAGNI）：前端一行正则就能判形态，Rust 侧已有 `viewer_stat` 判存在性，不值得为「判断是不是路径」新增一个 IPC 命令。
8. **支持 `~` / 相对路径 / 环境变量展开** ——拒绝（YAGNI）：用户没要求，且 `viewer_stat` 对这些形态本来就不成立。
9. **网络盘也查卷标** ——拒绝：`GetVolumeInformationW` 对网络盘要走网络，断开的映射盘会卡到 SMB 超时（可达数十秒）——这正是 `viewer_drives` 当初被改成 `GetDriveTypeW` 的原因。卷标只对本地盘取。
10. **反向同步时把树整个重建** ——拒绝：树是懒加载结构（只存已加载 children），逐级 `listDir` 展开是它的固有代价；整树重建会把大树的白屏时间从毫秒推到秒。

## Consequences

- **新增 Rust 依赖：0**。`filesearch.rs` 用 `windows-sys`（已在依赖树，只加 `Win32_UI_Shell` / `Win32_System_Registry` 两个 feature 用于 `ShellExecuteExW` 提权拉起 helper）+ `serde_json`（已有）。
- **新增前端依赖：0**。共享包 `packages/preview` 只加了一个**可选方法** `FileSource.searchGlobal?`，desktop 端零影响。
- **首次全盘搜索会弹一次 UAC**（helper 需管理员建 MFT 索引）。用户拒绝 → 搜索自动退回当前目录并在界面上说明原因，不会静默失败。
- **全盘搜索不覆盖未索引的卷**：helper 只索引 NTFS 卷；网络盘 / 非 NTFS 卷不在索引内，这类文件仍需靠目录搜索或绝对路径直达。
- **路径不存在时按搜索词处理**：用户敲 `C:\Users\foo\bar.txt` 但文件不存在，会去搜「这个字符串」而不是报错——多数时候用户就是想找它。绝对路径格式但文件不存在时会显示搜索结果，不会误导成「打开失败」。
- **索引 helper 成为 release 包的新成员**：桌面端 `externalBin` 多一个 `hermix-filesearch-x86_64-pc-windows-msvc.exe`（约 1–2MB）；`ensure-sidecar.mjs` 会多构建一步，打包时间相应增加。
- **搜索范围标签常驻面包屑栏右侧**：它是界面元素而非装饰，所以用 `--bg-tertiary` 底色 + 全盘态用 `--bg-accent-soft`，不描边（沿用「层次靠底色差」纪律）。
- **搜索进行中必须给反馈**：首次搜索可能要经历「弹 UAC → helper 启动 → 装载索引快照」，实测从几秒到几十秒不等（helper 还在后台建卷索引时更慢）。所以范围标签的显示条件是 `searchNote`（一发起搜索就有）而不是 `searchScope`（要等结果回来才定）——否则用户在这段时间里看到的是「列表没变、也没提示」，会以为搜索没生效。
- **0 命中不能回落成目录内容**：列表显示什么由「有没有搜索结果集」决定（`searchScope`），而不是「请求在不在飞」（`searching`）——后者在搜索**完成**后就变 false，于是 0 命中的结果集会被丢掉，用户看到的是「搜索没生效」。
- **「路径不存在就当搜索词」有个细节**：路径形态的查询词（如 `C:\a\b.txt`）拿去匹配索引必然 0 命中（索引匹配的是**文件名**），所以这类输入按整串搜索意义不大；当前按整串搜，用户想找的是文件名时用不带路径的词更有效。

## Reintroduction conditions

- 若索引 helper 的提权授权在目标用户群里被普遍拒绝（全盘搜索长期不可用），则把目录遍历回退（`core/scanner.rs` 的 `scan_directory`）接回 helper 的无权限路径，做「无索引时的全盘遍历搜索」降级。
- 若索引快照体积继续增长到「helper 常驻内存不可接受」，则评估 helper 改成分片索引或流式查询。
- 若用户要求搜索支持通配符 / 多关键词 / 路径匹配（当前 helper 只做**文件名子串**匹配），则在 `crates/hermix-filesearch/src/core/search.rs` 扩展匹配器，而不是在前端加过滤。
