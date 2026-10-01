# Hermix × WorkBuddy 设置能力对标分析

Archived: 2026-09-30

> 2026-09-28 · 基于 7 张 WorkBuddy 设置页截图与 Hermix 现有代码底座。
> 结论先行：**7 项功能里 6 项可落地，其中"模型配置简化"收益最大且有一半底座已存在**；
> "传输加密/安全网关"在 Hermix 的本地 stdio 架构下是伪需求，建议砍掉。

---

## 一、总览

| WorkBuddy 功能 | Hermix 可行性 | 现有底座 | 工作量 | 建议优先级 |
|---|---|---|---|---|
| 模型配置简化（两类模型 + 测试连接） | ✅ 高 | ModelSettingsTab、`/api/model/list`、`test-connection` 端点（已存在） | M | **P0** |
| 记忆与进化（开关 + 管理记忆） | ✅ 高 | hermix-agent memory、FocusStack、archive-index | M | P1 |
| 沙箱安全（文件/命令/网络策略） | ✅ 高 | approval engine、Sandbox seam、permission levels | M-L | P1 |
| 自动备份 + 备份总上限 | ✅ 高 | workbench 已有 `_trash` 软删先例 | S-M | P1 |
| 删除保护 + 批量删除审批 | ✅ 中 | approval engine、`_trash` 模式 | S-M | P2 |
| 内置运行时开关（Node/Python/Git Bash） | ✅ 中 | Shell seam、tool gating（enabledToolNames） | M | P2 |
| 安全网关 / 传输加密 | ⚠️ 伪需求 | 本地 stdio bridge，数据不出机 | - | **砍掉** |

---

## 二、模型配置简化（P0，最优先）

### 现状痛点

`ModelSettingsTab.tsx`（1000+ 行）是**供应商中心制**：先选供应商 → 填 Key → 勾模型 → 设默认，
三层折叠 + 内置/自定义两套 UI + 上下文窗口等高级字段全部平铺。历史 bug（ModelSelector.tsx
注释里的"双权威漂移"：localStorage / config.yaml / modelCache 三处各认一个权威）正是这个
复杂度的产物。

### 目标设计（对齐 WorkBuddy 截图 5-7）

**页面 = 模型列表**（不是供应商列表）：

```
自定义模型                                    [添加模型]
说明文字：模型添加后会写入 config.yaml 持久保存
─────────────────────────────────────────────
🦊 DeepSeek-V4 Pro  内置            ✏️ 🔗 🗑️
◆ agnes-3.0-flash   自定义           ✏️ 🔗 🗑️
◆ MiniMax-M3        自定义 · 默认     ✏️ 🔗 🗑️
```

每行三个操作：**编辑** / **测试连接** / **删除**。模型只有两种标签：

- **内置**：供应商固定（minimax/deepseek/kimi/… 共 17 家 `BUILTIN_PROVIDER_IDS`），
  用户只需填 API Key、勾选要启用的模型
- **自定义**：任意 OpenAI 兼容端点，填接口地址 + API Key + 模型名

**添加模型弹窗**（分两步）：

1. 供应商下拉：内置供应商分组（带 logo，选中后接口地址自动预填、只露 API Key 一个输入框）
   + "自定义" 分组（露出接口地址/Key/模型名/测试连接）
2. 高级配置默认折叠：工具调用 ☑ / 图片输入 ☐ / 思考模式 ☐ 复选框，
   输入/输出价格留空 = 使用供应商默认

**测试连接**：后端已存在 `POST /api/custom-providers/{id}/test-connection`，需要：

- 泛化成 `POST /api/providers/test`，接受**未保存的临时配置**（provider + base_url + api_key + model），
  内置供应商也能测
- 返回结构化结果：`{ ok, latency_ms, error_kind }`，error_kind 复用既有的错误四分类
  （**apikey / 网络 / 配额 / provider 故障**），前端按分类给中文提示
- 实现上就是发一个 max_tokens=1 的 chat 请求（或先 GET /models），10s 超时

### 单一权威治理（顺手还债）

新界面落地时把权威收敛到**后端 config.yaml 一处**：前端 localStorage 只做缓存镜像，
所有写操作走 `PUT /api/model/list` 后广播刷新（`writeModelConfigAndNotify` 已是这个形状，
保留即可）；ModelSelector 的 reconcile 逻辑可以砍掉一半分支。

### 落点

- `apps/desktop-tauri/src/components/settings/ModelSettingsTab.tsx` → 重写为 `ModelListTab.tsx`
- 新增 `ModelAddDialog.tsx`（两步弹窗）
- `crates/hermix-server`：泛化 test-connection 端点 + 内置供应商测试
- `crates/hermix-config`：模型条目增列 `capabilities`（tools/images/thinking）字段

---

## 三、记忆与进化（P1）

WorkBuddy 的三件套与 Hermix 现状对照：

| WorkBuddy | Hermix 现状 | 差距 |
|---|---|---|
| "生成对话记忆"总开关 | memory 写路径已存在（hermix-agent memory + FocusStack 显著性） | 缺一个**用户可见的开关**（gate 写路径）+ 设置项 |
| 管理记忆（重置/编辑/导入） | 记忆内容落盘（`~/.hermix` 下记忆文件 + archive-index.jsonl 可查） | 缺 REST 端点（GET/PUT 记忆正文）+ 设置页编辑器 |
| "每晚自动整理更新" | 压缩沉淀机制已有（ContextManager 折叠 + 迭代摘要） | 逻辑已等价，只需在 UI 里如实呈现 |

**做法**（M）：

1. `hermix-server` 加 `GET/PUT /api/memory`、`POST /api/memory/reset`、`POST /api/memory/import`
2. 设置页新增"记忆"tab：开关（写 config.yaml）+ 记忆正文编辑器（textarea 即可，别造富文本）
   + 重置（二次确认）/ 导入（选 md 文件）
3. 总开关关闭时 memory 写路径 short-circuit，读路径保留

注意：UI 文案别硬编码"仅本人可见"这类承诺——本地单机版这是事实，但措辞要跟隐私实际一致。

---

## 四、安全中心（P1，对安全员出身的你价值最高）

WorkBuddy 的"沙箱安全"面板与 Hermix 底座对得上：

### 4.1 文件/命令/网络安全策略（M-L）

Hermix 的 approval engine 已经在**每次文件写入 / 命令执行前拦截**（Type-B 审批、
permissionLevel），缺的只是"规则先行"：

- 在审批引擎前加一层**策略表**：路径 glob 白/黑名单、命令前缀放行（如 `git *`）、域名白名单
- 命中"放行"→ 静默通过；命中"拒绝"→ 直接拒绝并写审计（DiagnosticsTab 已有审计事件表！）；
  未命中 → 走现有审批流
- 设置页三个子页（文件/命令/网络），形态照抄截图：入口行 + chevron 进二级编辑

落点：`hermix-tools`（审批引擎旁新增 policy 层）、`hermix-config`（策略持久化）、设置 UI。

### 4.2 自动备份 + 备份总上限（S-M，强烈建议做）

- 在 write_file / edit_file / patch 类工具执行**前**，把目标文件拷进
  `~/.hermix/backups/<date>/`，保留相对路径结构
- 备份总上限（MB）+ LRU 清理；设置页一个数字输入 + "打开备份目录"按钮（openExternalUrl 已有）
- workbench_manager 的 `_trash` 软删已经是同款思路，抽成通用 backup 模块即可

### 4.3 删除保护 + 批量删除审批（S-M）

- 总开关：开启后 delete_file / storage_clean 类操作先进 `~/.hermix/trash/`（照搬 `_trash` 模式）
- 批量删除审批：一次删除 ≥ N 个文件时强制走审批卡（阈值数字输入），approval engine 加一个
  计数型触发条件即可

### 4.4 智能体工具管控（S）

`enabledToolNames` override 已存在（ChatView 提交时就在传），缺的只是设置页：
按工具列出开关 + 按 MCP server 列出开关，写入 config，注入每次会话的 overrides。纯 UI 工作。

### 4.5 安全网关 / 传输加密 —— 建议砍掉

WorkBuddy 是云服务，这两条是给"数据出机"场景背书的。Hermix 是本地 stdio bridge +
本地 HTTP，数据物理上不出机，做"端到端加密通道"是给假问题写真代码。若想要对等的**心安感**，
诚实的替代是 DiagnosticsTab 里加一页"数据流向说明 + 网络出口清单"（哪些请求会出网：LLM
provider、更新检查），零运行时成本。

---

## 五、内置运行时开关（P2）

截图形态：总开关 + Node.js / Python / Git Bash 三个带图标子项。

Hermix 现状：命令执行走 Shell seam，运行时探测散落（hermix-server 环境检测）。做法：

1. 启动时探测三个运行时（`node --version` / `python --version` / `git --version`，走既有
   exec 基建），结果缓存 + 设置页展示"已安装 vX.Y / 未安装"
2. 开关落 config.yaml，gate 对应工具族：关 Node → 限制 `node -e` 类命令前缀（走 4.1 的
   命令策略表，天然复用）
3. **总开关关掉 = 只允许内置工具，禁一切子进程命令**——这个语义对 approval engine 是一行判断

工作量 M，优先级放低是因为：Hermix 的 exec 主要用系统 shell，Node/Python 是脚本场景，
不是高频痛点。

---

## 六、建议落地顺序

1. **第一批（模型配置简化）**：ModelListTab 重写 + 两步添加弹窗 + 测试连接泛化。
   顺带还清"双权威漂移"的技术债，根治你已经踩过两次的模型选择诡异问题
2. **第二批（安全中心）**：自动备份（S，最快见效）→ 工具管控 UI（S）→ 删除保护 + 批量审批
   （S-M）→ 文件/命令/网络策略表（M-L，可再拆三期）
3. **第三批（记忆）**：REST 端点 + 设置 tab
4. **随手（P2）**：运行时开关；数据流向说明页替代"传输加密"

每批独立可交付，批内按"一语义一 commit"。第一批动 ModelSettingsTab 前先出 2×2 设计
方向预览（照 qiaomu-design 流程走），设置页是要天天看的界面，值得先对齐再动手。
