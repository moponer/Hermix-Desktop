# Decision Note: Windows ACL restricted-token 沙箱设计

Status: proposed — WRITE_RESTRICTED token + 三层写防线的最小切片定界与对抗式审查已成型；实现（P3）与真机测试钉死清单未做

> 依据：deepseek-harness `packages/sandbox/` 四包组 + 决策文档
> `2026-08-08-windows-acl-restricted-token-sandbox.md`（含 2026-09-19 delete 收口扩展的最终形态）。
> 本笔记是 hermix 移植的最小切片定界 + 对抗式审查记录。
> 实施认领方：批次 F（结构升级，季度级）—— `docs/notes/proposed/process/2026-09-30-doc-cleanup-and-optimization-plan.md`。

## Decision

**走 token 派生路线，不走身份路线。** 机制抄成熟方案，不发明：

`CreateRestrictedToken(hToken, WRITE_RESTRICTED | DISABLE_MAX_PRIVILEGE | LUA_TOKEN, ...)` —— Windows 对 write 类访问做**两次判定**（正常 SID 判定 + restricting SID 判定，两者都过才放行 write），read 走正常判定（调用方本就有的读权限原样保留，**零 read 授权成本**）。

**三层写防线**（一次 `SetNamedSecurityInfoW` 调用内同时落三个编辑）：

1. **能力 SID grant ACE**：workspace/temp 目录各挂一个派生 SID（`sha256(path)` → `S-1-4-x-y`）的 GRANT_MASK ACE（`FILE_GENERIC_WRITE|DELETE|FILE_DELETE_CHILD`，去 `WRITE_DAC|WRITE_OWNER` 防重写 DACL/夺权逃逸）。
2. **Low 强制完整性标签**：目录 + 受限 token 都降到 Low（`SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`）。内核在访问判定内生效——**堵住父目录 `FILE_DELETE_CHILD` 授权路径**（pass-2 交集管不到的那条，`cmd /c del` 借父目录权限删工作区外文件的回归就是靠这层堵死的）。
3. **DENY world `FILE_DELETE_CHILD`**：能力 ACE 的 DELETE 位成为被授权根内唯一删除权威。

**两个 keep-alive / 不变量**：token default DACL 必须并入一个 restricting SID 的 full-access ACE，否则**新建匿名管道（子进程 stdio）在创建时 EPERM，一切带管道的东西全挂**；logon SID + Everyone 必须留在 restricting 列表（缺了 → DLL 初始化 0xC0000142 / CNG 崩 pwsh 0xE0434352），Authenticated Users 剔除（CIM 不可用 → 关死 C:\ 根目录树创建逃逸）——接受，照抄。

**最小切片的 12 个决策点**（逐条定界，全部照抄 deepseek 语义，不换名、不自创）见下文表格。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 身份路线（mxc / AppContainer） | 任意路径读都要全量改写 DACL，OS 门槛还高。token 路线 read 走正常判定，**零 read 授权成本**——这是它胜过 mxc / AppContainer 的根 |
| 外部 runner exe（deepseek 的 Node runner） | deepseek 用外部 runner 是因为 TS 侧无进程内通道；Rust 有进程内 `CreateProcessAsUserW` + token 直传通道，少一个可执行件少一面故障 |
| `CREATE_NO_WINDOW` 隐藏控制台 | 受限创建下 → `STATUS_DLL_INIT_FAILED`，子进程直接死。改用 STARTUPINFO `STARTF_USESHOWWINDOW|SW_HIDE`（deepseek 2026-09-16 决策的坑，直接吸收） |
| workspace grant 随命令撤销 | 树传播代价一次性（大工作区可达数十秒），随命令撤销等于每次重付。改为**常驻** grant + exact-ACE skip 幂等，跨命令摊销 |
| 共享固定 temp 目录 | 每命令随机私有目录（`%TEMP%` 下）+ 路径派生独立 SID；ACE 随目录销毁，免 revoke FFI |
| 自动升级（escalation）编排 | YAGNI。首版 escalation = 用户切 `danger-full-access`；升级编排（审批引擎）已独立存在，不进本切片 |
| 认证层与沙箱一起做 | 超出最小切片：SSH 后端沙箱 / per-session 状态 / mxc / AppContainer 全部列为非目标 |
| 新建独立 crate 承载沙箱 | 六步阶梯：唯一消费者是 LocalBackend，不为拆而拆；落 `hermix-environments/src/sandbox/` 模块 |
| 静默降级为裸 spawn | 拒绝。fail-closed：任何 FFI 失败 → `AgentError`，**绝不回退裸 spawn**（成熟方案的底线：永不静默不受限） |
| 声称 enforcement 完整 | 如实报告 `partial`（hard-link 别名 + AppContainer 树 + 读不受限三个边界），与 deepseek 同 |

## Consequences

- **首命令在大工作区上有一次性树传播代价**（可达数十秒），由 standing grant 跨命令摊销；这是接受成熟方案同款取舍的代价。
- **崩溃残留 = 惰性垃圾**：grant 与清理之间崩溃会留下 temp 目录，下次随机新路径不冲突；不写清理器。
- **后台命令 + 沙箱**：temp **不撤销不删除**（子进程还活着），同样按惰性垃圾文档化。
- **三个已知 enforcement 边界**，如实报告为 `partial`：hard-link 别名、AppContainer 树、读不受限。
- **Authenticated Users 被剔除**是接受的残余风险：CIM 不可用 → 关死 C:\ 根目录树创建逃逸。照抄，不加补丁。
- **FAT/exFAT（非 ACL 文件系统）不验证不承诺**，只文档化。
- **read-only 下 PowerShell 落到 ConstrainedLanguage**（AppLocker 探针写不了 temp），接受——这是 PS 启动行为而非 ACL 边界；workspace-write 下私有 temp 能力让探针通过，保留 FullLanguage。
- **workspace 与 temp 根重叠、或 workspace 是盘根时，ACL 变更前直接报错**（deepseek 同款重叠检查）。
- **世界 deny 只挡受限子进程**：人类用户自己的进程删被授根内文件时 ambient DELETE 直接放行（同一 DACL 显式授），这是设计意图不是漏洞。
- **实现与真机测试尚未落地**（P3）：下文的测试钉死清单是准入门槛，未跑通前不得声称 enforcement 成立。

---

## 机制（抄成熟，不发明）

`CreateRestrictedToken(hToken, WRITE_RESTRICTED | DISABLE_MAX_PRIVILEGE | LUA_TOKEN, ...)`：
token 派生路线（非身份路线）。Windows 对 write 类访问做**两次判定**——正常 SID 判定 +
restricting SID 判定，两者都过才放行 write；read 走正常判定（调用方本就有的读权限原样保留，
**零 read 授权成本**）。这是它胜过 mxc / AppContainer 的根（那两条身份路线任意路径读都要
全量改写 DACL，OS 门槛还高）。

三层写防线（一次 `SetNamedSecurityInfoW` 调用内同时落三个编辑）：
1. **能力 SID grant ACE**：workspace/temp 目录各挂一个派生 SID（sha256(path) → `S-1-4-x-y`）
   的 GRANT_MASK ACE（`FILE_GENERIC_WRITE|DELETE|FILE_DELETE_CHILD`，去 `WRITE_DAC|WRITE_OWNER`
   防重写 DACL/夺权逃逸）。
2. **Low 强制完整性标签**：目录 + 受限 token 都降到 Low（`SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`）。
   内核在访问判定内生效——**堵住父目录 `FILE_DELETE_CHILD` 授权路径**（pass-2 交集管不到的那条，
   `cmd /c del` 借父目录权限删工作区外文件的回归就是靠这层堵死的）。
3. **DENY world `FILE_DELETE_CHILD`**：能力 ACE 的 DELETE 位成为被授权根内唯一删除权威。

token default DACL 必须并入一个 restricting SID 的 full-access ACE，否则**新建匿名管道
（子进程 stdio）在创建时 EPERM，一切带管道的东西全挂**。

keep-alive 不变量：logon SID + Everyone 必须留在 restricting 列表（缺了 → DLL 初始化
0xC0000142 / CNG 崩 pwsh 0xE0434352）。Authenticated Users 剔除（CIM 不可用 → 关死
C:\ 根目录树创建逃逸）——接受，照抄。

## hermix 最小切片定界

| 决策点 | 选择 | 依据 |
|---|---|---|
| 落点 | `hermix-environments/src/sandbox/` 模块（非新 crate） | 唯一消费者是 LocalBackend；六步阶梯：不为拆而拆 |
| 策略词汇 | `read-only` / `workspace-write` / `danger-full-access`，与 deepseek 逐字对齐 | 成熟词汇不换名 |
| 默认模式 | `workspace-write`；`HERMIX_SANDBOX_MODE` env 覆盖（含 `danger-full-access`） | 铁律：零 config schema 变更，开关走 env；fail-closed 是成熟方案的默认姿态 |
| danger-full-access | 完全绕过（走现有路径，不碰沙箱） | 与 deepseek 语义一致 |
| Runner 形态 | **进程内** CreateProcessAsUserW + token 直传（无外部 runner exe） | deepseek 用 Node runner 是因为 TS 侧无进程内通道；Rust 有，少一个可执行件少一面故障 |
| temp 目录 | 每命令随机私有目录（`%TEMP%` 下）+ 路径派生独立 SID；子进程 env 的 TMP/TEMP 指向它；退出 `remove_dir_all`（ACE 随目录销毁，免 revoke FFI） | deepseek 的 agentless per-invocation 流程即此形；崩溃残留 = 惰性垃圾，文档化 |
| workspace grant | **常驻**（不随命令撤销），exact-ACE skip 幂等 | 树传播代价一次性（大工作区可达数十秒），跨命令摊销 |
| 子进程 env | 手工构建 env block（父 env + TMP/TEMP 覆盖），UTF-16 双 NUL 结尾 | 子进程里 cargo/rustc 重度依赖 %TEMP% |
| 控制台隐藏 | **禁用 `CREATE_NO_WINDOW`**（受限创建下 → STATUS_DLL_INIT_FAILED 子进程直接死），改 STARTUPINFO `STARTF_USESHOWWINDOW|SW_HIDE` | deepseek 2026-09-16 决策的坑，直接吸收 |
| stdio | 自建匿名管道（子端可继承）+ NUL 设备作 stdin；读端包成 `ChildStdout/ChildStderr` | 与现有 wait_with_output 语义对齐 |
| 失败语义 | fail-closed：任何 FFI 失败 → `AgentError`，**绝不回退裸 spawn** | 成熟方案的底线：永不静默不受限 |
| enforcement | 如实报告 `partial`（hard-link 别名 + AppContainer 树 + 读不受限三个边界） | 诚实，与 deepseek 同 |
| 非目标 | SSH 后端沙箱 / per-session 状态 / mxc / AppContainer / 升级编排（审批引擎已独立存在） | YAGNI；escalation 首版 = 用户切 `danger-full-access` |

## 对抗式审查（什么场景会崩）

| 场景 | 处置 |
|---|---|
| `CREATE_NO_WINDOW` 杀死受限子进程 | STARTUPINFO SW_HIDE 替代（上面已列） |
| 子进程 stdio 匿名管道 EPERM | default DACL 并入 restricting SID full-access ACE |
| `cmd /c del` 借父目录 `FILE_DELETE_CHILD` 删工作区外 | Low 标签 + world deny 三层，采纳 2026-09-19 最终形态 |
| 后台命令（background）+ 沙箱 | 允许；temp **不撤销不删除**（子进程还活着）→ 文档化惰性垃圾，同崩溃残留语义 |
| 两命令并发 grant 同一 workspace | 全局 Mutex 串行化 grant 段；exact-ACE skip 使重复 grant 为 no-op |
| 首命令在大工作区上树传播数十秒 | 一次性代价，standing grant 摊销（成熟方案同款取舍） |
| workspace ==/包含 temp 根、或 workspace 是盘根 | ACL 变更**前**直接报错（deepseek 同款重叠检查） |
| FAT/exFAT（非 ACL 文件系统） | 不验证不承诺，文档化（deepseek 同） |
| 提权调用方 | `LUA_TOKEN` 过滤出受限派生 token，行为正确 |
| read-only 下 PowerShell | AppLocker 探针写不了 temp → ConstrainedLanguage（接受，是 PS 启动行为非 ACL 边界） |
| workspace-write 下 PowerShell | 私有 temp 能力让探针通过 → FullLanguage 保留 |
| cargo build / git commit（工作区内写+执行） | 写走能力 ACE；执行/读非 write 类不受 pass-2 影响 → 正常 |
| 命令写工作区外（如 C 盘清理） | EACCES → 诚实策略拒绝；报错文案指明 `HERMIX_SANDBOX_MODE` 出口 |
| 人类用户自己的进程删被授根内文件 | world deny 只挡受限子进程；用户 ambient DELETE 直接放（同一 DACL 显式授） |
| 进程崩溃在 grant 与清理之间 | temp 目录残留 = 惰性垃圾，下次随机新路径不冲突 |

## 测试钉死清单（真机）

sid 派生确定性 + 域分离 / 策略解析 / **工作区内写成功** / **工作区外写 EACCES** /
**工作区内删成功** / **工作区外删失败（ambient-delete 回归，关键）** / 任意读不受限 /
exit code 镜像 / stdout 管道可用（default DACL 证明）/ fail-closed（不产出裸 spawn 路径）。
