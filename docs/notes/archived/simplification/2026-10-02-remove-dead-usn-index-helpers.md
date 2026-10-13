# Decision Note: 删除 usn 索引三处死代码（add_entry / frn_dirs_get / RawLine.op）

Status: implemented — 三处 dead_code warning 清零，索引行为零变化
Archived: 2026-10-13 — 所属子系统（本地文件搜索 / usn-indexer）已整体移除，见 implemented/simplification/2026-10-13-remove-local-file-search-module.md

## Decision

删除 `src-tauri/src/commands_local_search.rs` 中三处编译器判定为死的代码，
并把 `frn_dirs` 的值域约定写死在注释里：

1. `usn_helper::add_entry`（原 427 行）——向 index 注册一条记录。
2. `usn_helper::frn_dirs_get`（原 475 行）——仅被 `add_entry` 调用。
3. `usn_helper::RawLine.op` 字段（原 365 行）——从未被读取。

判定依据不是 lint，是**调用链 + 值域一致性**双重核对（批次 G4 明确
"无 import 不构成死代码证据"）：

- 更新流 `spawn_tail_replay` → `replay_update` 是活的，其 add 分支
  （588-607 行）**自己内联了** arena 追加 + frn 映射，全程未调用
  `add_entry`；父目录解析走的是 `resolve_parent_dir`（613 行），不是
  `frn_dirs_get`。两者是并行实现，后者在重构中被前者取代。
- 决定性证据：`frn_dirs` 的值域在活代码里是 **dirs 下标**——816 行注释
  写明"frn → 自己路径的 dirs 下标"，`resolve_parent_dir` 也按此读取。
  而被删的 `add_entry` 往**同一个 map** 里塞的是 `eid`（entries 下标）。
  两者语义冲突：复活 `add_entry` 会让 `resolve_parent_dir` 把条目下标
  当成目录 id 去索引 `dirs`，产出错路径且不报错。
  **所以这不是"备用的参考实现"，是错的**，留着比删掉更危险。
- `RawLine` 本身在用（745 行解析 snapshot 流），只有 `op` 无人读：
  helper 写 snapshot 时本就不带 op，带 op 的是 updates 流，由独立的
  `UpdLine` 承载。原注释" snapshot/updates 共用"已与实现不符。

## Alternatives considered

- **加 `#[allow(dead_code)]` 压制 warning——否决**。这会把"值域约定矛盾"
  这个真问题永久藏起来：下一个人看到 `add_entry` 还在，会理所当然地
  以为增量重放可以复用它。warning 是症状，矛盾才是病因。
- **只删 `RawLine.op`、保留两个函数——否决**。`frn_dirs_get` 唯一调用方是
  `add_entry`，删掉调用方后它必然变成新的 dead code，等于把同一笔债
  推迟到下个迭代再付一次。
- **把 `add_entry` 改造成 `replay_update` 的实现以消除重复——否决**。
  两者时机不同：`add_entry` 处理 snapshot 灌入（dir_ids 兜底建目录），
  `replay_update` 处理增量（复用既有 dir_id）。强行合并会引入 snapshot
  专属的兜底分支污染热路径，且当前重复量只有 20 行、已由 816/618 两处
  注释各自说明。收益不抵风险。

## Consequences

- 三个 warning 清零。`commands_local_search.rs` 相关的剩余 warning
  （`usn_indexer.rs` 的 `HashMap` / `FSCTL_ENUM_USN_DATA` / `READ_USN_JOURNAL_DATA_V0`
  未用导入）属另一文件、另一成因，本批不动。
- 索引行为**零变化**：删的三处均不在任何活路径上，`cargo check` 前后
  无行为差异。
- `RawLine` 仍保留 `#[derive(serde::Deserialize)]` 且未加
  `deny_unknown_fields`，故 helper 将来在 snapshot 行里多写字段也不会
  解析失败——删 `op` 不改变对未知字段的宽容度。
- build.rs 那条 `sidecar source not found: target/debug/hermix.exe` 是
  **既有环境告警**，与本批无关：它只是提示"要先在仓库根跑
  `cargo build -p hermix-server` 产出 sidecar"，不影响编译通过。

## Reintroduction conditions

- **`add_entry` / `frn_dirs_get`**：仅当 snapshot 灌入与增量重放被重新
  合并为同一条代码路径时，才可重新引入。届时必须先解决 `frn_dirs` 的
  值域歧义（`dirs` 下标 vs `entries` 下标），并在 `Index` 定义处把约定
  写死为类型层面的约束（如 `frn_dirs: HashMap<u64, DirId>`），
  而不是再靠注释维持。
- **`RawLine.op`**：仅当 snapshot 与 updates 重新共用同一解析结构时。
  当前两者字段集已分叉（updates 另有 `UpdLine.op: String`，无 `Option`），
  合并前需先对齐类型。
