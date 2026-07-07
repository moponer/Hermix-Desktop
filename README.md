<div align="center">

# Hermix Desktop

**自进化的 AI 助手。零安装，零环境。**

`181,127 行 Rust` · `1,378 个测试` · `18 个 crate` · `33 个内置 Skill` · `~80MB 解压即用`

[下载 zip](#) · [文档](#) · [更新日志](#) · [问题反馈](#)

</div>

---

## 一句话定位

> 下载 zip，解压到任意位置（推荐 D/E/F 盘），双击 `Hermix.exe`，开始用。

---

## 核心能力

- **与 AI 对话** — 支持 10 种大模型（OpenAI、Anthropic、Qwen、DeepSeek、Kimi、MiniMax、NousResearch、Copilot、OpenRouter、本地）
- **帮你干活** — 读文件、写代码、执行命令（白名单内）、搜索网页、操作浏览器、文件格式转换
- **记住你的偏好** — 5 层跨会话记忆架构（FocusStack + 向量召回 + 结构化记忆 + 持久化存储 + 文件记忆）
- **30+ 工具** — 文件、终端、搜索、视觉、语音、网页摘要、任务编排、看屏点屏
- **33 个内置 Skill** — 12 个开发类 + 11 个办公类 + 10 个生活类，LLM 自主判断何时调用
- **电脑操控** — [自研 Windows 原生栈](#自研能力--电脑操控)，AI 能直接看屏幕、点鼠标、敲键盘
- **定时任务** — cron 定时提醒/拉取
- **GroupChat 多智能体** — 内置总管调度 + 多角色协作

---

## 怎么用（3 步）

1. **下载** `Hermix-Desktop-Windows-x64.zip`（约 80MB）
2. **解压** 到任意位置（推荐 `D:\Hermix Desktop\`）
3. **双击** `Hermix.exe`

首次启动会引导你填一个 API key（支持 OpenAI / Anthropic / 国产模型），然后就能开始对话了。

---

## 自研能力 — 电脑操控（Computer-Use）

> 让 AI 像人一样 **看屏幕、点鼠标、敲键盘**。全程 Windows 平台原生自研，不集成任何三方桌面自动化运行时。

### 14 个工具

| Phase | 工具 | 作用 |
|-------|------|------|
| 1 | `cua_screenshot` | 截屏，返回 base64 PNG |
| 1 | `cua_screen_info` | 列出所有显示器 |
| 1 | `cua_left_click` | 鼠标点击 |
| 1 | `cua_type_text` | Unicode 文本输入 |
| 1 | `cua_press_key` | 组合键 / 单键 |
| 2-B | `cua_list_windows` | 枚举顶层窗口 |
| 2-B | `cua_window_at_point` | 返回坐标处窗口 |
| 2-B | `cua_click_window` | 激活并点击窗口 |
| 2-C | `cua_ui_tree` | UIA 元素树 |
| 2-C | `cua_ui_find` | 按属性找元素 |
| 2-C | `cua_ui_click` | 点击 UIA 元素 |
| 2-C | `cua_ui_focus` | 聚焦 UIA 元素 |
| 2-C | `cua_ui_invoke` | 触发 UIA 动作 |
| 2-D | `cua_ocr_recognize` | 离线 OCR |

### 全栈自研，Windows 原生 API

| 能力 | 实现 | 关键点 |
|------|------|--------|
| 截屏（主路径） | DXGI Output Duplication | GPU 直采，比 GDI 快 5-10x |
| 截屏（fallback） | BitBlt + GDI | 工具永不失效 |
| 鼠标/键盘输入 | Win32 SendInput | 系统级事件，Unicode 完整支持 |
| 窗口识别 | EnumWindows + GetWindowRect | hwnd 双向、可见性过滤 |
| UIA 元素交互 | UIAutomation COM | IUIAutomationElement / InvokePattern |
| 离线 OCR | Windows.Media.Ocr | 无 token、Win10 1809+ 内置 |
| 浏览器自动化 | 自研 CDP 客户端 | tokio_tungstenite，零三方 driver |

### 双层安全审批

- **任务级一次** — 同一 session 内同类型操作只批一次
- **destructive 每次** — 不可逆操作每次弹窗
- **硬拦截** — Ctrl+Alt+Del、Win+L、Alt+F4 直接拒绝
- **限速** — 默认 30 ops/s
- **完整审计** — 每次操作写入 `~/.hermix/cua/sessions/<session_id>/trace.jsonl`

---

## Skill 体系

### 33 个内置 Skill

**开发类（12 个）**：代码审查、系统化调试、代码重构、Git 工作流、性能优化、安全审计、API 设计、数据库设计、前端架构、TDD、DevOps、架构设计

**办公类（11 个）**：邮件起草、报告生成、会议纪要、数据分析、翻译、内容摘要、电子表格、日程安排、演示文稿、PDF 转换、OfficeCLI

**生活类（10 个）**：健康指导、财务规划、投资分析、旅游规划、学习教练、写作助手、计划撰写、网络小说、知识管理、生活规划

### Skill 增强机制

- **LLM 自主判断** — 不使用关键词匹配，由 LLM 根据任务自主决定是否调用 Skill
- **触发条件** — 每个 Skill 包含触发条件、执行步骤、输出格式
- **使用指导** — `SKILL_USAGE_GUIDANCE` 常量指导 LLM 何时使用/不使用 Skill
- **索引分组** — Skill 索引按 category 分组显示，便于 LLM 选择

---

## 架构概览

```
用户输入 → 意图识别 → 工具路由 → Agent 循环 → 结果验证 → 输出
                ↓                ↓              ↓
          5 层记忆系统    30+ 工具后端    压缩/恢复
                ↓                ↓              ↓
        FocusStack       安全审批引擎    Gateway 安全网
        向量召回         会话级记忆      五步压缩管道
```

### 核心 crate

| Crate | 职责 |
|-------|------|
| `hermix-agent` | Agent 循环、LLM 提供商、记忆系统、上下文管理 |
| `hermix-tools` | 30+ 工具后端、审批引擎、CUA handler |
| `hermix-server` | HTTP/WebSocket + Dashboard + Skill 管理 |
| `hermix-intelligence` | 智能路由、工具路由器、上下文引擎 |
| `hermix-focus` | FocusStack 5 状态机、记忆显著性评分 |
| `hermix-config` | 配置加载、路径管理、便携式部署 |
| `hermix-cua` | 自研电脑操控栈（DXGI / SendInput / UIA / OCR / CDP） |
| `hermix-core` | 核心类型、路径安全、工具 schema |
| `hermix-skills` | 技能管理、专家注册 |
| `hermix-gateway` | 多平台适配、GroupChat 管理 |

### 关键特性

- **5 层记忆架构**：L5 MEMORY.md/USER.md → L4 Session 持久化 → L3 FocusStack → L2 结构化记忆 → L1 向量嵌入
- **Gateway 85% 安全网**：上下文超 85% 自动截断，防 token 爆炸
- **五步压缩管道**：Prune → Snip Ancient → Boundary → Context Collapse → Assemble
- **结果验证机制**：工具执行后自动验证（文件修改、退出码、错误检测）
- **环境感知**：Git 状态、项目类型自动检测
- **工具调用上限**：单轮最多 20 次工具调用，防止死循环
- **会话级审批记忆**：同一操作只批一次，跨会话保留
- **写竞争抖动**：并发写入随机延迟，防数据库锁冲突
- **任务焦点指导**：长对话自动注入焦点恢复指令
- **4 层权限防护**：PermissionLevel + Tirith + RiskLevel + ApprovalManager

---

## 数据存放在哪

| 类型 | 内容 | 路径 |
|------|------|------|
| **用户配置和数据** | config.yaml、.env、auth.json、sessions.db、memory_store.db | `~/.hermix/` |
| **应用瞬态** | WebView2 缓存、日志 | `<安装目录>/webview-cache`、`<安装目录>/logs` |

```
安装目录/                    ← 解压位置（任意盘）
├── Hermix.exe              # 主程序（Tauri v2, ~25MB）
├── hermix-server.exe       # 后端服务（Rust, ~16MB）
├── hermix.exe              # CLI 备用（无 GUI 场景）
├── webview-cache/          # WebView2 缓存（可清）
└── logs/                   # 日志（滚动 7 天）

~/.hermix/                  ← 你的数据
├── config.yaml             # API Key 配置
├── .env                    # 环境变量密钥
├── auth.json               # 认证令牌
├── sessions.db             # 聊天历史（SQLite + FTS5）
├── memory_store.db         # 长期记忆
├── enabled_toolsets.json   # 工具集开关
├── approval_allowlist.json # 审批允许列表
├── skills/                 # 技能
└── dashboard-plugins/      # 面板插件
```

**备份建议**：整个 `~/.hermix/` 目录就是你的全部"记忆 + 技能 + 配置 + 历史"。安装目录删了重下一个就行，数据不丢。

---

## 构建（开发者向）

```bash
# 全 workspace 编译检查
cargo check --workspace --all-targets

# 全 workspace 测试（1,378 个）
cargo test --workspace --lib

# 前端类型检查
cd apps/desktop-tauri && pnpm tsc --noEmit
```

**Windows MSVC 构建**：源码编译需要 MSVC 环境，`.cargo/config.toml` 已硬编码链接器路径。

---

## 常见问题

**Q: 数据会越来越大?**
A: 正常用半年约 500MB。如果上 GB，检查是不是装了太多"技能"和"插件"。

**Q: 跑在 macOS / Linux?**
A: 暂不支持。只承诺 Windows 桌面端。

**Q: 数据会上传?**
A: 不会。Hermix Desktop 不收集任何遥测。你说的话/你传的文件，只发给你配置的 LLM 服务商。

**Q: 怎么升级?**
A: 重新下载 zip → 解压 → 双击 exe。数据保留在 `~/.hermix/`，自动读取。

**Q: 换电脑怎么办?**
A: 把 `~/.hermix/` 拷到新电脑相同位置即可。

---

## 技术栈

- **后端**: Rust 18 个 crate，~181K 行
- **前端**: React 19 + Tauri v2 + Tailwind 4
- **运行时**: 静态二进制，无 Python/Node 依赖
- **数据**: SQLite + FTS5 + JSON + 文件树
- **测试**: 1,378 个单元测试

---

## 许可证

MIT — 见 [LICENSE](LICENSE)。
