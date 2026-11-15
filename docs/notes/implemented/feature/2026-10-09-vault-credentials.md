# Decision Note: 本地凭据保险库（vault）+ 手机远程取凭据

Status: implemented — 已拍板开工（2026-10-09）；本地加密凭据库 + 手机 E2EE 取凭据，随实施保持同步

## Decision

新增本地加密凭据库（`hermix-vault` crate）+ 桌面 UI + agent 工具 + 手机端经 relay 的 E2EE 查询通道。

### 数据与加密

- 存储：`~/.hermix/vault.db`（SQLite）。字段：名称、URL、用户名、密码、说明文本。
- **落盘必须加密**（不可削减项，信任边界）：密码字段 AES-256-GCM 加密存储；主密钥派生自主人口令，主密钥存 Windows Credential Manager（不落 `~/.hermix` 明文文件）。
- vault 不上云、不同步、不进任何备份通道——单机数据。

### 桌面 UI

- 新面板/页面：列表 + 搜索；密码列默认 `••••` 掩码，**点击才显示**（需求明确），另有复制按钮。
- 新增凭据 / 编辑 / 删除走表单；删除高危（已有资源门 delete 类拦截可复用）。

### agent 工具（手机路径）

- 工具：`vault_list`（只返回名称 + URL 摘要，永不含密码）、`vault_get`（用户明确点名条目时取，含密码）。
- 手机对话链路：手机 → relay → 桌面 agent 调 vault 工具 → 回传走 **E2EE 帧**（复用 `relay_crypto` P-256 通道）→ relay 全程看不到明文密码。
- 手机端展示后自动隐藏（约 30s 后恢复掩码）。
- 权限：`vault_get` 含密码，限**已配对设备**（`PairedDevice` 能力位）；未配对来源一律拒绝。

### 交付拆分

1. `hermix-vault` crate（加密存取 + 测试）
2. 桌面 UI 面板
3. agent 工具注册（`hermix-tools`）
4. 手机端查询帧类型 + E2EE 回传

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 明文 SQLite + 文件权限保护 | 违反信任边界不可削减项；`~/.hermix` 在用户家目录，被任意进程/误备份读走即全裸 |
| 复用现成密码管理器（KeePass 数据库格式 KDBX） | 引入 KDBX 格式兼容面（argon2/aes 变体多），自研字段少（只 5 字段），YAGNI；将来若要导出 KeePass 可加导出功能而非采用其格式 |
| 密码存云端（relay 服务器） | 与「vault 数据单机、relay 永不接触明文」的信任模型冲突；relay 是路由组件不是数据托管方 |
| 集成到 config 的纯文本段 | 同明文方案，否决 |

## Consequences

- **得到**：外勤场景手机取凭据闭环；密码 E2EE 到手机，relay/BBS 服务器永不接触明文。
- **放弃**：多设备同步、浏览器自动填充集成（一期单机；将来有需求再评估）。
- **风险**：主人口令忘记 = 数据不可恢复（无后门）——UI 上明确提示，不做找回机制；Windows Credential Manager 在极少数精简系统缺失，需检测并降级为口令派生。
- **边界**：`vault_get` 是高风险工具，进审批引擎视野；IM 通道（QQ/企微）不得调用（只允许配对手机）。
