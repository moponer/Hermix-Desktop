# Decision Note: 本地文件索引 UAC 链修复

Status: proposed — 2026-10-05 现场取证确认自动索引链路被三处缺陷击穿(根因 + 策略 + 静默失败),同批修复;伴随 simplification 性质的增量流治理。修复已随 2026-10-07 笔记落地；**该子系统已于 2026-10-13 整体移除**，见 implemented/simplification/2026-10-13-remove-local-file-search-module.md。

## Decision

修复本地文件搜索"打开搜不到任何文件"的根因链,按因果分三层:

1. **根因修复**:`usn-indexer` 的 `touch()` 用"打开即关、不写字节"实现,Windows NTFS 不更新 LastWriteTime → `helper.alive` mtime 冻结 → 驻留检测(`helper_resident`)与让位检查(`usn_indexer` 启动处)双双失效。改为真写内容(写时间戳字符串),一处修改恢复三个依赖点(免二次 UAC / 旧 helper 让位 / 心跳语义)。
2. **策略修复**:① `HELPER_FAILED` 不再一次性判死——UAC 失败后自动路径当轮放弃,但用户打开查找面板时可显式重置并重试;② `INDEX_TTL=600s` 只约束 jwalk 全量路径——USN helper 路径有 tail 重放保鲜,不需周期性全量重建(每次重建都会再弹 UAC,是用户拒绝授权的直接诱因)。
3. **可观测性修复**:`IndexStatus` 增加 `helper_state`(ok/unauthorized/failed),前端状态条明示"索引需要授权"并提供"重新授权"按钮;helper 消费路径回填 `PROGRESS`(按快照行数),消灭"已发现 0 项"的假死观感;前端查询失败不再静默伪装成空结果。

伴生治理(同一链路,同批实施):`spawn_tail_replay` 加 OnceLock 守卫(现状每次重建泄一线程且重读全量增量);`updates.jsonl` 消费后按大小截断(现场已膨胀至 189MB);`done.json` 延迟到拿到新结果再删(UAC 失败不再误删上次成功凭据);`search_roots()` 用 `GetDriveTypeW == DRIVE_FIXED` 过滤(现场 Z: 失效映射混入报错);双 helper 并发写 `updates.jsonl` 的行交错由截断治理自然缓解。

## 取证记录(2026-10-05,本机)

- `usn-indexer.exe` 存活(PID 2784),`done.json` 显示 2026-10-04 16:27 全量枚举成功(indexed: 2,034,593),snapshot 已被消费删除——链路本身能通。
- `helper.alive` mtime 冻结在 10-03 11:38(helper 每秒 touch 一次)——`touch()` 不更新 mtime 的实锤。
- `helper.pid` mtime 10-04 16:25——之后再无新 helper 拉起(驻留检测失效的另一面:每次启动都重复弹 UAC)。
- `updates.jsonl` 189MB 且持续增长——tail 重放多实例 + 无截断的实锤。
- `done.json` error 字段含 `Z: 0x80070002`——非固定盘混入卷列表。

## 失效故事线(修复前)

启动 → 自动索引触发 UAC → 未授权 → `HELPER_FAILED` 判死 → 索引静默空;打开面板敲字 → helper 已锁死 → jwalk 全盘兜底(分钟级,BELOW_NORMAL)→ 用户等不到结果;即使某次授权成功(10-04 那次),600s TTL 后重建再弹 UAC → 拒绝 → 回到判死循环。索引能力存在,但授权链 + TTL 策略 + 静默失败三层叠加使其表现为"永远坏"。

## 实施(函数级)

1. `apps/desktop-tauri/src-tauri/src/bin/usn_indexer.rs`:
   - `touch()`(L81)改 `std::fs::write(path, now_unix().to_string())`;单测:`touch` 后 mtime 前进。
2. `apps/desktop-tauri/src-tauri/src/commands_local_search.rs`:
   - `FileIndex` 加 `helper_state: HelperState`(枚举 Ok/Unauthorized/Failed,`#[derive]` Serialize);`rebuild_index` 的 helper Err 分支按错误文案分类落状态(UAC 拒绝/超时 = Unauthorized,其余 = Failed);成功路径回 Ok。
   - `usn_index_via_helper` 的 `HELPER_FAILED` 判定改为可重置:新增 `pub fn reset_helper_failure()`(清 AtomicBool + `helper_state` 复位);`ensure_index` 不因 `HELPER_FAILED` 提前 return——失败后仍允许再次尝试(重试成本 = 一次 UAC 弹窗,由用户显式触发)。
   - TTL:`ensure_index` 的 TTL 检查改为"索引由 jwalk 建成时才受 TTL 约束"(`Index` 加 `source: IndexSource` 枚举 Usn/Jwalk;Usn 路径 built 后不做 TTL 过期,tail 重放保鲜)。
   - `spawn_tail_replay` 加 `static TAIL_SPAWNED: OnceLock<()>` 守卫。
   - tail 重放消费侧:`offset` 追平后若文件超过 64MB,`OpenOptions truncate` 截断并复位 offset(消费单线程内安全;helper 侧 append 句柄在 Windows 上截断竞态可容忍——最坏丢一批增量,resnap 兜底)。
   - helper 快照消费路径:按读入行数 `PROGRESS.fetch_add`(与 jwalk 同计数器,前端"已发现 N 项"生效)。
   - `run()`(usn_helper)的 `remove_file(done.json)` 移到拿到新 done 内容之后。
   - `search_roots()`:`GetDriveTypeW(root) != DRIVE_FIXED` 的卷剔除(保留 `is_dir` 判断)。
   - `local_search_status` 返回 `helper_state`;查询命令失败路径保持返回 Err(不吞)。
3. 前端 `apps/desktop-tauri/src/components/LocalSearchDialog.tsx`:
   - `IndexStatus` 类型加 `helper_state`;顶部工具栏左侧状态条:ok = 不显示;building = 现有进度文案;unauthorized = "索引需要授权一次(管理员权限)" + [重新授权] 按钮(invoke `local_search_reauthorize` → 后端 `reset_helper_failure()` + `ensure_index(false)`);failed = "索引构建失败" + 同按钮(文案"重试")。
   - 空态:非 building、hits 空、indexed==0 时,按 helper_state 给原因文案,不再裸"无结果"。
   - `local_search` 查询 catch 分支:status 显示错误而非伪装空结果。

## 验证

- `cargo check --workspace --all-targets`;`cargo test -p hermix-desktop --lib`(touch/alive_fresh/HelperState 分类单测)
- `cd apps/desktop-tauri && pnpm tsc --noEmit`
- 手工冒烟:拒绝 UAC → 面板显示"需要授权";点击重新授权 → UAC → 索引建成可搜;重启应用 → 不再弹 UAC(驻留复用);10 分钟后查询不再触发重建。

## Alternatives considered

- **去掉提权 helper,全走 jwalk**:零 UAC 但全盘遍历分钟级、每 10 分钟一遍,体验更差;MFT/USN 秒级能力是本功能的差异化价值。弃。
- **开机计划任务/服务常驻 helper(免 UAC)**:绕过 UAC 的正确工程解,但引入系统级安装面(计划任务注册/卸载/权限),超出本 bug 修复范围;列为未来 enhancement。
- **只加"重新授权"按钮不动 touch()**:治标——驻留检测依旧失效,每次启动仍弹 UAC、双 helper 并发仍在。弃。
- **TTL 全部取消**:jwalk 路径无增量保鲜,取消 TTL 会让陈旧索引永久存活。保留 jwalk 路径 TTL。

## Consequences

What we give up:
- `HELPER_FAILED` 的"绝不二次弹 UAC"保证消失:用户主动点"重新授权"会再弹一次——这是把选择权还给用户的本意。
- `updates.jsonl` 截断在极端时序下可能丢一批增量事件(最坏:某文件改名未生效),由 resnap 全量校正兜底——可接受的最终一致性。
- helper 状态三态暴露给前端:错误分类依赖错误文案匹配(UAC 拒绝的 `ShellExecuteW` 失败),文案变更需同步分类规则。

## Reintroduction conditions

- `INDEX_TTL` 对 USN 路径的豁免:若 tail 重放被证明不可靠(增量丢失率可观、resnap 频繁),恢复周期性全量重建并配自动授权方案(计划任务)。
- `HELPER_FAILED` 一次性判死:若"重新授权"按钮被滥用为高频重试入口(审计发现),恢复失败冷静期。
