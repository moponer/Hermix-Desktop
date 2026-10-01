# Decision Note: Windows ACL 沙箱（批次 F-2 立项）

Status: proposed — 待评审；切片落地，先读通道后写通道

## Decision

为 terminal / file 工具引入 **OS 级写隔离**，对标 dsh
`2026-08-08-windows-acl-restricted-token-sandbox` +
`2026-09-19-mandatory-integrity-confinement` 两篇（含其补漏笔记）。
现有 SANDBOX_WINDOWS_ACL_DESIGN.md（已迁 notes）是最小切片，本立项扩展为
完整方案。核心机制：

- **`CreateRestrictedToken(WRITE_RESTRICTED)` + 低完整性标签**：只约束写、
  读保持环境权限——AppContainer 被否的原因是需要全量 DACL 改写才能支持
  任意读，桌面场景代价过高。
- **已知旁路必须同批堵上**：父目录删除权不受 restricting SID 约束
  （`cmd /c del` 逃逸），需补强制完整性 + `FILE_DELETE_CHILD` ambient-delete
  deny ACE（dsh `2026-09-19` 补漏笔记的教训：首发方案被 `del` 穿透）。
- **沙箱后端链**：每工具路径配置后端序列（受限令牌优先 → 内置 fallback）；
  被拒后**单次批准升级重试**，不做永久 allowlist（dsh `2026-07-06-sandbox`：
  拒绝无升级路径会逼用户全局放开权限）。
- **fs 与 bash 共享同一策略根**（dsh `2026-07-14-cross-family-fs-sandbox`）：
  两套根会漂移，模型会借 bash heredoc 绕过 fs 限制——hermix 的 file 工具与
  terminal 现在正是两套独立判断，必须收敛到同一 workspace 策略面。
- **与 2026-09-26 审批定策的关系**：沙箱是**能力隔离**（进程根本写不出去），
  不是审批弹卡——不与"写文件不用审批"定策冲突，二者正交。

## Alternatives considered

- **AppContainer** —— 否决：任意读需要全量 DACL 改写，破坏"读保持环境权限"。
- **仅靠现有 credential_guard + allowlist 正则** —— 否决：正则可绕
  （heredoc、重定向、中间命令），形态匹配不等于能力隔离；dsh 原则
  "schema 隐藏/字符串过滤不是强制，拒绝必须发生在执行层"。
- **Docker/WSL 隔离** —— 否决：便携零安装定位（PROPOSAL_PORTABLE_DESKTOP）
  不允许引入容器依赖。
- **一步到位全覆盖** —— 否决：先覆盖 terminal 写命令 + file 写工具到
  workspace 外，workspace 内不降速；后续再扩展网络/electron 类。

## Consequences

- 放弃：非 Windows 平台（本项目就是 Windows-only，无损）。
- 代价：受限令牌创建有进程启动开销（~10ms 级），terminal 冷启动可感知但
  可接受；Job Object 与 restricted token 组合需要测试覆盖
  （hermix-cua 已有 Job 用法可参考）。
- 风险：子进程链中 spawn 的孙进程继承完整性标签——需验证 npm/cargo 等
  包管理器在受限令牌下可用，不可用则走单次批准升级路径。
- 收益：模型失控/注入场景下 workspace 外文件系统真正不可写（现在只靠
  提示词 + 正则 allowlist），安全等级从"纪律"升到"物理"。
