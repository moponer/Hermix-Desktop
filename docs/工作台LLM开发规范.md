# 工作台 LLM 开发规范

> 版本：v1 · 日期：2026-09-01 · 状态：基于 M0–M6 已落地代码核准（非设计稿）
>
> 依据：`docs/工作台架构设计方案.md` v0.3、`crates/hermix-workspace/src/model.rs`、`crates/hermix-server/src/dashboard/workspaces.rs`、`apps/desktop-tauri/src/components/workspace/*`

---

## 0. 一句话答案

**LLM 开发工作台不写编程语言代码——它产出声明式 JSON（`WorkspaceSchema`），由通用渲染器呈现。**

工作台是「Schema 驱动为主，代码生成为逃生门」的第一性决策（架构方案 §4.1）：

| 路线 | 说明 | 状态 |
|---|---|---|
| **A. Schema 驱动（主路径）** | LLM 产出 `WorkspaceSchema`（表结构/视图/仪表盘/自动化），渲染器呈现，数据走后端 SQLite | ✅ 已落地，覆盖 ≈90% 台账/管理场景 |
| **B. 代码生成（逃生门）** | LLM 写 vanilla JS（Web Component / 脚本） | ⚠️ 设计已定（§16），**实现未启动**，见 §5 |

选择 A 的依据：安全（无代码执行）、可校验、可同步、运行开销小、一次性交付不报错。B 路线的独立工具场景已由既有 custom-app 机制覆盖，工作台不重复造。

---

## 1. 技术栈三层（别混淆）

| 层 | 谁写 | 技术 | 位置 |
|---|---|---|---|
| **① 工作台模块自身（后端）** | 人类开发者 | **Rust**（`hermix-workspace` crate，rusqlite 0.32 bundled） | `crates/hermix-workspace/src/` |
| **② 工作台渲染器（前端）** | 人类开发者 | **React 19 + TypeScript + Tailwind 4 + Vite** | `apps/desktop-tauri/src/components/workspace/` |
| **③ LLM 建设的工作台内容** | **LLM** | **声明式 JSON（`WorkspaceSchema`）** | `~/.hermix/workspaces/<ws_id>/workspace.json` |
| ④ 逃生门扩展代码（规划） | LLM | vanilla JS（Web Component / 脚本） | 未实现，见 §5 |

### ② 渲染器实际依赖（已确认安装）

| 用途 | 选型 | 版本 |
|---|---|---|
| 布局引擎 | react-grid-layout | ^2.2.4 |
| 表格数据层 | @tanstack/react-table | ^8.21.3 |
| 图表 | echarts | ^6.1.0 |
| 看板拖拽 | dnd-kit | ❌ 未安装（规划中） |
| 共享状态 | zustand | ❌ 未安装（React 自带够用） |

### ③ LLM 侧"语言"的唯一形态

LLM 的工作产物是**一份 JSON 文档**，通过 `workspace_build` / `workspace_update_schema` 工具提交。它不写 Rust、不写 TSX、不写 SQL、不写 JS。

---

## 2. WorkspaceSchema 契约（LLM 主产物）

完整结构与字段说明见 `docs/工作台架构设计方案.md` §5.3 与 `crates/hermix-workspace/src/model.rs`。规范化要求：

### 2.1 结构骨架

```json
{
  "schema_version": 1,
  "name": "安全管理工作台",
  "icon": "shield",
  "modules": [ /* Table 或 Dashboard 模块 */ ],
  "layout": { "<dashboard_id>": [{ "i": "<widget_id>", "x": 0, "y": 0, "w": 4, "h": 2 }] },
  "automations": [],
  "permissions": { "mode": "readwrite", "allow_delete": true }
}
```

### 2.2 字段类型白名单（`FieldType`）

| type | 说明 | 约束 |
|---|---|---|
| `text` | 单行文本 | — |
| `number` | 整数/浮点 | — |
| `date` | `YYYY-MM-DD` | — |
| `datetime` | `YYYY-MM-DD HH:MM:SS` | — |
| `boolean` | 布尔 | — |
| `enum` | 枚举 | **必填 `options` 数组** |
| `relation` | 关系外键 | **必填 `target`（目标表 id）** |
| `attachment` | 附件 | 存 `files/<row_id>/` 相对路径 |

### 2.3 视图类型（`ViewType`）与**渲染支持度**

| type | 说明 | 渲染状态 |
|---|---|---|
| `grid` | 表格 | ✅ TableView（含行增删改、虚拟滚动、搜索、CSV 导出） |
| `form` | 表单 | ✅ FormView |
| `kanban` | 看板（`group_by` 分组） | ⚠️ **schema 合法 + 后端校验通过，但前端无渲染器**（见 §5.2） |
| `calendar` | 日历（`group_by` 日期） | ⚠️ 同上 |

### 2.4 仪表盘组件（`WidgetType`）

| type | 说明 | 依赖 |
|---|---|---|
| `stat` | 统计卡（数字 + 文案） | ✅ |
| `chart` | 图表 | ✅ ECharts 6 |
| `list` | 列表 | ✅ |

`widgets[].query` 是受控查询片段（如 `count where status=在用`），后端白名单校验后执行。

### 2.5 自动化（`Automation`）

| kind | 说明 | 关键字段 |
|---|---|---|
| `condition` | 条件提醒：确定性 SQL WHERE + `{field}` 插值消息 | `table` / `condition` / `message` / `frequency`（daily\|hourly\|cron） |
| `llm_summary` | LLM 定时分析（语义综合，走 sub-agent 隔离） | `schedule`（cron）/ `prompt` / `output`（notify\|report\|chat） |

### 2.6 权限（可选）

- `permissions.mode`：`readwrite`（默认）/ `readonly`（拒绝 insert/update，查询不受限）
- `permissions.allow_delete`：`false` 则禁止删行（防 LLM 误删独立开关）
- **非法值被校验拒绝（fail-closed）**，不会静默降级

### 2.7 布局设计档位（LLM 唯一的视觉编排权）

设计质量分三层，责任归属必须分清：

| 层 | 控制方 | 机制 |
|---|---|---|
| 配色 / 视觉语言 | **前端 CSS token** | 组件样式全部 `var(--color-*)`，schema 无颜色字段，LLM 无法污染 |
| 组件内部排版 | **前端组件** | 标题栏、28px 数字、间距、圆角由组件实现 |
| **画布布局编排** | **LLM（`layout` 字段）** | react-grid-layout 网格坐标，唯一需要 LLM 设计的地方 |

**网格规格**：12 列 / 行高 30px / margin 8px / containerPadding 4px。

**组件尺寸档位**（与前端 `DashboardView.DEFAULT_SIZE` 一致）：

| widget type | 推荐 w × h | 说明 |
|---|---|---|
| `stat` 统计卡 | `3 × 2` | 一排 4 个 |
| `chart` 图表 | `6 × 4` | 一排 2 个 |
| `list` 列表 | `4 × 4` | 一排 3 个 |

**编排规则**：总览先行（stat 第一排）→ 图表居中 → 明细沉底；同排 `y` 相同（顶边对齐）；`x + w ≤ 12`；`i` 必须匹配真实 widget id 且每个 widget 都要出现；留空则由前端按档位自动流式铺排。

**后端校验**（`validate.rs::validate_layout`，2026-09-01 新增）：
- 键必须指向已存在的模块；指向 dashboard 时校验 `i` 在 widgets 内；指向 table 只校几何（内置模板有空数组占位存量）
- 拦截：`w < 1`、`h < 1`、`x/y` 为负、`x + w > 12`、缺字段、悬空模块
- **不拦重叠**（react-grid-layout 会推开，且存量数据可能已重叠）

---

## 3. 开发流程契约（LLM 必须遵守）

```
① 听懂需求 → 提炼实体/字段/状态/关系/视图
② 产出 schema 提案 → 展示给用户 → 获明确确认 ★
③ workspace_build（完整 schema + 可选种子数据）→ 后端自动建设（目录/定义/建表/导入/自测）
④ 叙述「建设完成，请测试」，提醒用户去工作台 Tab 验收
⑤ 迭代：workspace_update_schema（传完整新版 schema，非 diff）
   - 先展示改动点（新增/删除/修改）→ 获确认 → 调用 → 叙述迁移结果
⑥ 查询/统计：workspace_table_schema → workspace_query / workspace_count / workspace_group_count
⑦ 写数据：workspace_insert / workspace_update / workspace_delete
```

★ **未获确认不得调用 build/update**——两个工具均为 `RiskLevel::Caution`。

---

## 4. 工具族（`workspace_*`，分类 `workspace`）

| 工具 | 用途 | 风险 |
|---|---|---|
| `workspace_build` | 创建工作台（完整 schema + 种子数据） | Caution（先确认） |
| `workspace_update_schema` | 增量迁移定义 | Caution（先确认） |
| `workspace_table_schema` | 查询表结构（字段白名单） | Safe |
| `workspace_query` | 查询表数据 | Safe |
| `workspace_count` | 计数 | Safe |
| `workspace_group_count` | 分组计数（仪表盘 chart 用） | Safe |
| `workspace_insert` | 插入行 | Caution |
| `workspace_update` | 更新行 | Caution |
| `workspace_delete` | 删除行（尊重 `allow_delete`） | Caution |

---

## 5. 能力边界（**已实现 vs 未实现**）

> 规范只承诺已落地的能力。允许 LLM 产出 schema 但用户看不到效果，等同于欺骗——故本节为强制约束。

### 5.1 ✅ 已实现（LLM 可安全使用）

| 能力 | 说明 |
|---|---|
| 数据表模块（grid/form 视图） | 行增删改、虚拟滚动、搜索、CSV 导出 |
| 仪表盘模块（stat/chart/list） | react-grid-layout 布局 + ECharts |
| 数据 CRUD + 计数 + 分组计数 | 后端白名单校验、变更日志、`undo_last` 撤销 |
| 导出/导入 `.hwx` | `new`（重生成 id）/ `merge`（整表 LWW）/ `upsert` |
| 自动化定义与运行记录 | condition / llm_summary 两类 |
| 权限（读只读 / 禁止删除） | fail-closed 校验 |
| 内置模板一键建台 | `POST /api/workspaces/from-template` |
| `@work` 快捷入口 + 系统提示索引 | 见 §7 |

### 5.2 ⚠️ 定义合法但未渲染

| 能力 | 状态 | LLM 应对 |
|---|---|---|
| **kanban 视图** | `model.rs` 定义 + `validate.rs` 校验 group_by，**前端无 KanbanView 组件** | **不要主动承诺看板**；用户要求时先说清"当前渲染为表格/暂不可见"，再决定产出 |
| **calendar 视图** | 同上，无 CalendarView 组件 | 同上 |

### 5.3 ❌ 未实现（不要向用户承诺）

| 扩展点（§16.2） | 内容 | 状态 |
|---|---|---|
| ① 自定义组件 | `widgets/<id>/` + vanilla JS Web Component | 未实现（`hermix-workspace` 无 widgets 目录机制） |
| ② 自定义动作 | `actions/<id>/` + 脚本沙箱 / agent 任务 | 未实现（无脚本运行时，项目无 quick-js/rquickjs/boa 依赖） |
| ③ 自定义 SQL 视图 | `workspace.json` 内只读 SQL 视图 | 未实现（`View` 结构体无 SQL 字段） |
| ④ 自定义自动化 | 多步编排 | 部分：仅 condition / llm_summary 两类，无多步编排 |
| ⑤ 嵌入独立应用 | vanilla HTML/JS → 独立 exe | **走既有 custom-app 机制**（`custom_app_manager`），不由工作台承担 |
| WorkspaceBridge | 扩展代码数据桥 JS API | 未实现 |

**超出 CRUD 的需求**：优先用 ⑤ custom-app（已成熟）；语义判断类用 `llm_summary` 自动化（现有 sub-agent 能力）。

---

## 6. 铁律（LLM 与人类开发者共同遵守）

1. **标识符 vs 展示名**：`id` 是建列/建表标识符（必须过安全校验，小写字母数字下划线），`name` 可中文。宁可 id 用英文蛇形。
2. **`enum` 必填 `options`，`relation` 必填 `target`**——缺失校验直接拒，不会静默补默认。
3. **视图 `fields` / `group_by` 必须引用所属表已存在的字段 id**。
4. **`layout` 必须列出 dashboard 的每个 widget**（`{i,x,y,w,h}`），否则组件不可见。
5. **删除是软删**：`workspace_update_schema` 删除表/列时**数据保留**，别跟用户说"数据已删除"。
6. **LLM 永远通过工具操作数据，不直接写 SQL**——数据准确性、可审计、可校验。
7. **确定性逻辑不浪费 LLM**：统计/条件提醒走 SQL + 调度器；只有语义综合（周报、异常发现）才用 `llm_summary`。
8. **零上下文膨胀**：用户丢大 CSV/Excel → 提炼表头 + 前几行样例做种子数据，不要全文塞对话。
9. **不猜列名**：不确定结构先 `workspace_table_schema` 再 `workspace_query`。
10. **多工作台隔离**：每个工作台独立 SQLite 库 + 独立定义，Tab 切换互不影响。

---

## 7. `@work` 快捷入口

- 输入框打 `@work` → 面板首项"创建工作台"，其余为已建工作台
- 选"创建工作台" → 预填 `@work 创建工作台 需求：`；选已建 → 插入 `@work <工作台名> `
- 发送时自动注入 `【目标工作台】\n- <工作台名>（workspace_id: <id>）` 头
- 系统提示含 `## 已建的工作台` 索引（`@work:<id> — <name>`）——**已存在的工作台不要重复创建**
- agent 完成 `workspace_build` / `workspace_update_schema` / `workspace_delete` 后，前端自动刷新侧栏（App.tsx `onToolComplete` 钩子）

---

## 8. 前端刷新与数据流要点（人类开发者）

- 注册表单例：`apps/desktop-tauri/src/hooks/useWorkspaceRegistry.ts`（模块级 store，`booted` 首次挂载加载一次，`refreshWorkspaces()` 手动刷新）
- **注意区分**：`useWorkspaces`（目录沙箱，限制对话读写范围）≠ `useWorkspaceRegistry`（工作台注册表），二者不是一回事
- REST 端点：`/api/workspaces`（CRUD + schema/layout + export/import + automations + files + `tables/*` 数据端点 + change-log/undo）
- 数据目录：`~/.hermix/workspaces/<ws_id>/workspace.json + data.sqlite + files/ + audit.sqlite`，注册表 `registry.sqlite`

---

## 9. 规范变更

本规范随实施进度更新。新增能力落地时，必须同步 §5 能力边界（把 ⚠️/❌ 升为 ✅），避免"schema 合法但用户看不到"的能力声明漂移。
