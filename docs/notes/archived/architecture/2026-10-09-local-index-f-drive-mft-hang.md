# Decision Note: 本机文件索引建不起来——F: 卷 MFT 枚举卡死拖垮整次扫描

Status: implemented — 真机验证通过（192 万条 / 121 秒 / ok:true）
Archived: 2026-10-13 — 所属子系统（本地文件搜索 / usn-indexer）已整体移除，见 implemented/simplification/2026-10-13-remove-local-file-search-module.md

## Decision

索引助手（`usn-indexer.exe`）的全量枚举改为**逐卷隔离 + 成果可取走的看门狗**：
初扫与 resnap 共用同一个 `enumerate_with_watchdog()`，任何单卷卡死都不再拖垮整次扫描。

三条具体规则：

1. **看门狗必须能取走成果，而不是丢弃。** 期限检查放在枚举循环里是**无效**的
   —— 卡死发生在单次 `DeviceIoControl(FSCTL_ENUM_USN_DATA)` 调用内部，循环边界
   永远跑不到。实测证据：第一版按批次边界检查，snapshot 已写到 165.7MB
   （约 184 万条），到点后仍只能报 `ok:false / indexed:0`，184 万条全丢。
   正确做法是把写入器与计数用 `Arc` 共享给看门狗线程；到点时看门狗自己 lock
   写入器 flush（卡死线程阻塞在 DeviceIoControl 上、不持写锁，必然拿得到），
   然后**按成功上报这部分索引**再退出。
2. **逐卷预算 60s**（`PER_VOLUME_BUDGET`）抓"慢但在推进"的卷；与看门狗是两层，
   前者抓慢卷，后者抓卡死卷。
3. **初扫与 resnap 必须走同一个枚举器。** 两条路径面对同一批卷、同一个卡死
   风险；只给初扫加保护等于把 bug 留在 resnap 里。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 循环内按批次检查期限 | 实测无效。卡死在单次 DeviceIoControl 内部，循环边界跑不到；且首版因此把已写的 184 万条全丢了。 |
| 把卡死的卷从 `search_roots()` 里剔掉 | 治不了根。卡死是**偶发**的（外置盘休眠 / 链路瞬断），同一台机器上 F: 后续又能在 20s 内扫完 1.84M 条。静态剔除会把一个偶发故障变成永久缺失。 |
| 每卷一个独立 helper 进程 | 进程数与提权弹窗次数随盘数线性上升；同一个 exe 用线程 + 看门狗就够了。 |
| 看门狗到点直接 `exit` 不管成果 | 就是第一版的错误——把已经扫完的 5 个卷（131 万条）连同 F: 的前半截一起丢掉。降级到"少一个卷"远好过"一条都没有"。 |

## Consequences

- **索引从"完全没有"变成"少一个卷"**：C/D/E/G/H 的 131 万条照常入库，
  卡死的那一卷缺席并在意图里写明。真机验证 `C D E F G H` 得 1,922,593 条 /
  121 秒 / `ok:true` / `error:null`。
- 坏卷现在是**可诊断的**：界面过去只显示"未找到可用的索引助手或非 NTFS 卷"
  这句与真实原因无关的兜底文案；现在失败原因进 `done.json` 的 `error`。
- `done.json` 在成功时写 `error: null`。此前用
  `errs.first().unwrap_or_else(|| "no volumes")` 兜底，导致**每一次成功扫描**
  都写出 `"ok":true, "error":"no volumes"` —— 一句凭空捏造的失败文案，本次排查
  一开始就被它带偏。"没有卷"是另一个条件（volumes 为空），不该复用错误字段。
- 主进程侧 `RESNAP_TIMEOUT_SECS` 定为 240s（此前 25s，而一次真实枚举约 90-120s，
  复用路径因此必超时）。仍小于 300s 全局截止线，让全局 deadline 兜底。
- F: 的偶发卡死**没有被消除**，只是不再有破坏性。若要根治需查该盘为何会返回
  不结束的 DeviceIoControl（外置盒 / 休眠唤醒 / USB-SAT 桥接芯片），属盘侧问题。

## 验证

- 对卷 `C D E F G H` 跑真实枚举：121 秒，`indexed=1922593`，`ok:true`，`error:null`
- 逐卷单测确认故障面：C 6.6s / D 40.1s / E 3.4s / G 6.3s / H 33.3s 正常，
  I `enum start` 失败，Z `open` 找不到（失效盘符），F 曾卡死 >60s
- `cargo build --bin usn-indexer` 通过

## 附注：提交归属

本次改动因暂存区碰撞被并入了同一时刻另一个工作项的提交（标题与本修复无关）。
代码内容完整无误，但提交信息未涵盖它 —— 以本篇为准。