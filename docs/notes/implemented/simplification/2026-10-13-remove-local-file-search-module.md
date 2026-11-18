# Decision Note: 移除本地文件搜索模块（usn-indexer 全链路）

Status: implemented — 仓库与出包链路中已无 usn-indexer / 本地文件搜索痕迹，侧栏「本地文件」入口整体下线

## Decision

侧栏「本地文件」（Everything 式本地快速查找）模块整体移除。该模块自带的 MFT/USN
提权索引链路反复出现"索引建不起来"现场（resnap 超时、幽灵 helper、增量流膨胀、
UAC 链路故障），修复成本持续高于其价值；后续拟改接开源方案，故自建实现全部拆除。

删除清单（按层）：

- **独立 crate**：`crates/hermix-indexer`（usn-indexer 提权辅助进程，896 行）整体
  删除，根 `Cargo.toml` workspace member 同步移除
- **Tauri 壳**：`src-tauri/src/commands_local_search.rs`（jwalk 兜底遍历、arena
  索引、helper 交接协议、快照落盘、搜索/预览命令，约 2000 行含测试）删除；
  `lib.rs` 的模块声明、HWND 记录、`start_background_index` /
  `shutdown_background_index` 接线、5 个 `local_search*` 命令注册移除
- **打包链**：`build.rs` 的 `copy_usn_indexer_sidecar`、`tauri.conf.json`
  externalBin 条目、`ensure-sidecar.mjs` 的编译与刷新步骤、
  `check-sidecar-freshness.sh` 的 usn-indexer 陈旧性检查、`release.sh` 的
  sidecar 清单条目
- **前端**：`LocalSearchDialog.tsx` 整文件、`Sidebar.tsx` 的
  `LOCAL_FILE_ITEM` 入口 / 弹窗状态 / 渲染块
- **孤儿依赖**：`jwalk`、`nucleo-matcher` 仅被该模块消费，从
  `src-tauri/Cargo.toml` 移除
- **文档**：README 仓库结构行与本模块相关描述更新；4 篇该模块的 implemented
  历史笔记归档（见下）

server 后端（hermix-server 及其下 20 个 crate）与该模块零耦合，无需改动。

### 归档的旧笔记

本笔记取代以下对该模块内部机制的记录（机制随代码一起消失，笔记冻结归档）：

- `docs/notes/implemented/bug-fix/USN helper UAC 链（已删除）.md`
- `docs/notes/implemented/architecture/本地索引 MFT 卡死记录（已删除）.md`
- `docs/notes/implemented/simplification/USN 索引 helper 移除记录（已删除）.md`
- `docs/notes/proposed/bug-fix/2026-10-05-local-index-uac-chain.md`（proposed 件
  不归档，Status 行加指向本笔记的说明）

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 保留 jwalk 兜底路线，只删 USN 提权 helper | jwalk 全盘遍历分钟级且无增量保鲜，是"索引永远建不起来"体验的一半来源；留着它等于留半个模块和一份索引代码 |
| 保留模块、再修一轮索引可靠性 | resnap/幽灵 helper/UAC 链路是提权架构的结构性问题，历次修复（2026-10-05/07/08/09/10）都是补丁；需求本身（本地文件查找）将改接开源方案，自建实现没有第二次机会 |
| 只下线前端入口，代码留着"以后再说" | 死代码留在打包链里会继续参与出包校验（externalBin / freshness / release.sh 三处），与 zero-install 自包含目标对冲，且违背代码简化纪律 |

## Consequences

- **能力损失**：应用内全盘文件名模糊搜索没了；侧栏少一个菜单项。文件级操作
  （读/写/列目录）不受影响——那是 `hermix-tools` 的文件工具，另一条链路。
- **依赖收益**：src-tauri 少 2 个直接依赖；提权 helper 进程、约 170MB 快照落盘、
  `%TEMP%\hermix-usn-index\` 交换目录从产品行为中消失。
- **残留物**：已安装机器上的 `%LOCALAPPDATA%\hermix\local-index.bin`（旧索引
  快照）与 `%TEMP%\hermix-usn-index\`（交接文件）成为惰性残留，新版本不再读写；
  手工删除即可。已安装包里的 `usn-indexer.exe` 不再被拉起，helper 侧 60s 心跳
  陈旧规则会让旧进程自行退出，不阻塞升级覆盖。
- **官网文案**：`apps/relay/public` 的版本文章（articles.js）仍描述 usn-indexer
  机制——那是已发布版本的历史记录，随下个版本的 changelog 一并更新，不在本次
  代码拆除范围内。
- **命名余波**：`check-sidecar-freshness.sh` 中"每 sidecar 只比对自己的源码树"
  的隔离逻辑保留（hermix 仍排除无关目录的旧写法已随索引器删除一并简化）。

## Reintroduction conditions

接入的开源本地搜索方案若需要重新走 sidecar / 提权索引路线，按
`2026-10-09-single-workspace-target-dir.md` 与 `2026-10-10-sidecar-freshness-by-construction.md`
的 sidecar 纪律重新入列：独立 bin crate + ensure-sidecar 编译 + build.rs 编译期
复制 + externalBin 登记，四步缺一不可。
