# Decision Note: 本地索引反复扫描却永远没结果——枚举期心跳缺失导致双 helper 并发

Status: implemented — 真机验证：226s 真实全量枚举期间心跳最大间隔 5s、零断更，131 万条入库

## Decision

提权 helper 在**全量枚举期间也必须持续刷新 `helper.alive`**：预算检查与心跳合并进
同一个看门狗循环，不为心跳新增线程。

配套两条：

1. **非驻留拉起前清空 `snapshot-*.jsonl`**，消费完当前那份后清掉目录里其余
   snapshot——孤儿交接文件不属于"机械细节"，它是"索引明明建好了却搜不到"的
   直接成因（现场那份 170MB / 131 万条的成果就躺在盘上没人读）。
2. **helper 常驻循环每秒重写 `helper.pid`**，身份自愈，消灭"幽灵"helper。

## 现场证据

2026-10-10 用户报「本地文件模块始终在循环重复扫描，没有任何结果」。交换目录
`%TEMP%\hermix-usn-index` 里的原始记录：

- `spawn_debug.log` 两条 `resident_before=false`，相隔 93s（01:06:39 / 01:08:12）——
  主进程第二次启动时**判定没有驻留 helper**，于是再弹一次 UAC、再拉起一个
  提权 helper 全盘枚举。
- 两份无人认领的 snapshot 同时躺在盘上：`snapshot-1791565599.jsonl` 102MB、
  `snapshot-1791565692.jsonl` 170MB，合计 272MB。
- `done.json` = `{"ok":true,"indexed":1310693,...}` —— **扫描是成功的**，只是
  主进程在 01:09:02 就退出了（`main.alive` 停更），没来得及读走它。

## 根因

`helper.alive` 是主进程判断 helper 是否驻留的唯一依据（`helper_resident()`，
60s 新鲜度窗口），helper 自己的让位检查也用同一个判据。而心跳此前只在
**枚举前**写一次、**枚举完成后**的常驻循环里每秒写一次——中间那段最坏
`ENUM_BUDGET` = 240s 完全静默：

1. 60s 一到，主进程 `helper_resident()` 判 false → 走非驻留分支 → 再拉起一个
   提权 helper；
2. 新 helper 的让位检查同样判"没有别人" → 不让位 → **两个提权进程并发枚举
   同一批卷**；
3. 两者抢写同一个 `done.json`，主进程读到的 snapshot 可能属于另一个进程；
4. 新主进程顺手删掉在跑那个 helper 的 `helper.pid` / `helper.alive`，它此后
   再不重写 pid → 变成代码注释里说的"幽灵"。

这就是用户看到的循环：每开一次应用就多一次全盘扫描，索引始终建不起来。

同一场景还有第二条独立的重复扫描来源：主进程挂到一个**正在枚举**的驻留
helper 上时（`resident=true` 且 `done.json` 不新鲜），会立刻发
`resnap.request`，等于叫它刚扫完再重扫一遍。修法是用 snapshot 是否在增长
判断"枚举正在进行"，正在进行时只等 `done.json`，不发 resnap。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 把 `helper_resident()` 窗口从 60s 调大到 300s | 只把阈值推高，没有消除"枚举期静默"这个事实。枚举真要超过 300s 时同样复发，且窗口一大，helper 真死之后要等更久才被复用。 |
| 为心跳单开一个线程 | 能修好，但看门狗线程本来就在跑，预算检查与心跳合并成一个循环是同一件事，不多一个线程、不多一次调度。 |
| 用 PID 存活探测（OpenProcess）替代 mtime | 主进程非提权，对提权进程 `OpenProcess` 会 ACCESS_DENIED——代码里已经因这个原因不能 `TerminateProcess` helper。 |
| 把 F: 从 `search_roots()` 里静态剔掉 | 治不了根：卡死是偶发的（同机 F: 后续又能扫完 1.84M 条），静态剔除把偶发故障变成永久缺失。见 [MFT 卡死那篇](本地索引 MFT 卡死记录（已删除）.md)。 |
| 消费失败时保留 snapshot 供下次复用 | 会引入"读到半截 snapshot"的正确性风险。交接文件一次性消费、读完即删是更小的表面积。 |

## Consequences

- 索引从"每开一次应用就重来一遍、且永远读不到"变成"一次枚举，全进程共享"。
- 枚举期多一个每 5s 的小文件写（`helper.alive`，几十字节）。相对每条 MFT
  记录一次 JSON 序列化，这个开销可以忽略。
- 交换目录不再积压孤儿 snapshot：现场那 272MB 会在下一次拉起 helper 时被清掉。
- 枚举期的 `helper.alive` 现在真的代表"helper 活着"。此前它在最需要它准确的
  那段时间里是冻结的——凡是依赖它做驻留/让位判断的地方（主进程与 helper
  双方）都继承了这个假信号。
- F: 卷的偶发卡死**仍未消除**，只由逐卷预算降级为"少一个卷"。盘侧问题仍在。

## 验证

- 单测 `watchdog_refreshes_alive_during_enumeration`：真实线程 + 50ms 间隔，
  断言预算到点前 `helper.alive` 已被刷新。
- 单测 `enum_heartbeat_interval_stays_well_below_the_resident_window`：钉住
  `HEARTBEAT_INTERVAL * 3 < 60s` 这条不变式。
- 真机跑真实全量枚举（C D E F G H，226s，期间有 `cargo check` 抢占 CPU）：
  **心跳最大间隔 5 秒，全程零断更**；`done.json` = `ok:true` / 1,310,648 条 /
  169MB snapshot，F: 按逐卷预算降级跳过（错误文案写明）。
  关键对比：226s > 60s 驻留窗口，旧版在 t≈60s 处 `helper.alive` 已冻结、
  主进程必然再拉起一个提权 helper。
- `cargo check --workspace --all-targets` 通过（3m39s，唯一 warning 是
  `hermix-focus` 测试代码里未处理的 `Result`，与本次改动无关）；
  `cargo test -p hermix-indexer` 4 passed；
  desktop-tauri 的 `commands_local_search` 单测 5 passed。

## 未覆盖

`ci-gate.sh` / `check-notes.sh` 是 bash 脚本，本机只有 WSL stub、没有 Git Bash，
两者均**未能执行**。已手工复现其检查项：笔记命名/头三行/强制节符合
`check-notes.sh` 规则，编译与测试项用 cargo 直接跑。本篇之外的 note 未经该门禁。