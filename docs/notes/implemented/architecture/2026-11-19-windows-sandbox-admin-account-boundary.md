# Windows 沙箱：账户沙箱（内置管理员正解）

**日期**: 2026-11-19　**类目**: architecture
**影响**: `crates/hermix-environments/src/account_sandbox.rs`（新）、`sandbox.rs`、`local.rs`、`crates/hermix-server/src/main.rs`

## Decision

产品必须支持内置 Administrator（RID 500）。实测（见《2026-11-19-windows-sandbox-admin-account-boundary》）
证明受限令牌与 AppContainer 两条路在该账户上恒失败，因此按 Codex CLI `windows-sandbox-rs` 的核心模型实现**账户沙箱**：

1. **setup（提权一次）**：`ShellExecuteExW("runas")` 提权运行 sidecar 自身
   （`hermix.exe --hermix-sandbox-setup-exec <base64 payload>`）。
   `NetUserAdd` 创建 `HermixSandboxOffline` / `HermixSandboxOnline`（`UF_SCRIPT |
   UF_DONT_EXPIRE_PASSWD`，密码 24 字符四类保证）；WFP 四条持久过滤器按
   offline 账户 SID 拦全部出/入站（provider/sublayer GUID 为本项目永久命名空间，
   不可重新生成）；密码 DPAPI **机器域**加密写 `<hermix_home>/sandbox/secrets.json`；
   登录界面隐藏（Winlogon SpecialAccounts\\UserList）。
2. **运行时（非提权）**：`CreateProcessWithLogonW(账户, 密码)` 起进程——它自己完成
   登录，不需要 SeAssignPrimaryToken。**必须显式传 lpApplicationName**（NULL 时
   路径解析发生在新用户环境里，PATH 为空 → 失败，实测 err 3/5）；**不能传 NUL
   stdin 句柄**（继承句柄对新用户做访问重检 → error 5）。
3. **联网按需**：offline 账户（被 WFP 拦）/ online 账户（无过滤器）按命令选择。
   开关 = `HERMIX_SANDBOX_NETWORK=on`（审批层接缝；默认 offline）。
4. **路由**（`local.rs::execute_command_sandboxed`）：受限令牌路径优先（普通用户
   零 setup）；失败 → 账户沙箱；也失败 → 降级非受限执行 + stderr 显式告知（沿用
   既有语义），并按工作区记忆化。
5. **文件隔离**：独立账户默认 ACL（读不到其他用户 profile、全局只读）+ 工作区
   Modify ACE（用户对自有目录有 WRITE_DAC，免提权，幂等标记防 DACL 膨胀）。
6. **测试**：`sandboxed_command_can_write_workspace_but_not_outside` 钉住隔离契约
   （区内可写、区外拒绝）。测试工作区必须在 **Medium 完整性链**上——DSH 的
   tempfile 根（`F:\TMP-RU~1`）带 Low 标签，Medium 账户不可写（no-write-up），
   那是测试环境的属性。

## Alternatives considered

- **AppContainer**：内置管理员不能建 profile（E_ACCESSDENIED）。普通用户可用，
  将来若产品面向普通用户可重新评估。
- **Codex 的 runner 二进制 + 能力 SID + 防火墙 COM**：runner 与具名管道 IPC 为 CLI
  形态服务；桌面应用按「账户即隔离边界」直起命令，代码量减半，保证等价
  （账户默认 ACL 比「全读 + deny 列表」更强地限制读面）。
- **静默提权假设**：本机 `ConsentPromptBehaviorAdmin=0`（UAC 自动批准）使 setup
  无感；其他机器会弹一次 UAC——这正是「需要时授权」的产品语义。

## Consequences

- 内置管理员机器：一次性提权 setup 后，命令在独立低权账户下运行：文件写隔离
  （账户 ACL）、断网（WFP）按需放行（online 账户）、进程树由 JobGuard 收容。
- DSH 沙箱测试环境注意：构建产物（rustc Low 运行）继承 **Low 强制标签**，其进程
  不能用账户沙箱（Low 调用者 + WithLogonW → 5）。跑账户沙箱测试前需对测试二进制
  `icacls /setintegritylevel M`（`scripts/fix-build-integrity.ps1` 修目录级标签）。
  产品正常构建无此问题。
- setup 的 UAC 由命令执行路径自动触发（每进程至多一次），被拒绝则按既有降级语义
  处理，不静默。