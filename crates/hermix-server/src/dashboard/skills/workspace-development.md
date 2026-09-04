---
name: workspace-development
version: 1
description: >
  工作台（Workspace）开发助手。当用户要求创建/管理一个"工作台、台账、管理面板、数据管理、仪表盘、
  车辆管理/人员管理/安全台账/进销存"等 Schema 驱动的进程内多模块空间时使用。
  通过 workspace_* 工具族（workspace_build / workspace_update_schema / workspace_query 等）建设、查询、迭代工作台。
  工作台 = 用户用自然语言描述需求 → LLM 产出 WorkspaceSchema（数据表 + 仪表盘 + 自动化）→ 后端自动建表建 UI。
  与"自定义应用（独立 exe）"互补：工作台适合台账/管理类，进程内多 Tab，数据在后端 SQLite。
tags:
  - workspace
  - 工作台
  - 台账
  - 管理面板
  - dashboard
  - schema-driven
  - sqlite
  - kanban
  - calendar
  - automation
  - 数据管理
  - 仪表盘
---

# Workspace Development（工作台开发）

## When to Activate

- 用户说"帮我建一个 XX 工作台 / XX 台账 / XX 管理系统 / XX 管理面板"（车辆、人员、安全台账、进销存、项目、客户……）
- 用户丢来 CSV/Excel/粘贴文本，要求"建成表格管理"
- 用户要求在工作台里加模块、改字段、加视图、加仪表盘、加提醒
- 用户问"XX 工作台里的数据帮我查一下/统计一下"
- 对话语义命中台账/管理/仪表盘/看板/日历/统计需求时，**优先走工作台**而不是自定义应用
- 收到 `@work 创建XX工作台 ...` 或 `@work <已建工作台名> ...` 形态文本（快捷入口）

## Core Concepts

- **工作台 = Schema 驱动的进程内多模块空间**。用户对话 → LLM 产出 `WorkspaceSchema`（JSON）→ 后端建目录、写 `workspace.json`、SQLite 建表、注册数据工具、渲染 UI。用户看到的是多 Tab 画布。
- **定义与数据分离**：`workspace.json` 描述"长什么样"；数据在独立 SQLite 库（每工作台一个）。
- **布局即数据**：`layout` 是 react-grid-layout 的可序列化 JSON，后端不解释内部结构，LLM/前端可读写。
- **标识符 vs 展示名**：字段/模块/视图的 `id` 是建列建表的标识符（必须过安全校验：小写字母数字下划线，不合法会被拒），`name` 是任意文本展示名（可中文）。
- **与现有概念的区别**：
  - 「工作区」(useWorkspaces) = 目录沙箱，限制对话读写范围。**不是**工作台。
  - 「自定义应用」(custom-apps) = LLM 写网页工程打包成独立 exe，每个 app 一个 WebView2 进程。适合独立工具类。
  - 「工作台」= 进程内多 Tab，Schema 驱动，数据在后端 SQLite。适合台账/管理类。二者互补。
- **LLM 永远通过工具操作数据，不直接写 SQL**：数据准确性、可审计、可校验。
- **确定性逻辑不浪费 LLM**：条件提醒/统计聚合走 SQL + 调度器；只有语义综合（周报、异常发现、自然语言查询）才用 LLM 定时任务。

## WorkspaceSchema 结构

```json
{
  "schema_version": 1,
  "name": "车辆管理工作台",
  "icon": "car",
  "modules": [
    {
      "type": "table",
      "id": "vehicles",
      "name": "车辆台账",
      "fields": [
        { "id": "plate", "name": "车牌号", "type": "text", "required": true, "unique": true },
        { "id": "inspection_date", "name": "年检日期", "type": "date" },
        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
        { "id": "owner", "name": "负责人", "type": "relation", "target": "personnel" }
      ],
      "views": [
        { "id": "list", "name": "列表", "type": "grid", "fields": ["plate", "inspection_date", "status"] },
        { "id": "board", "name": "看板", "type": "kanban", "group_by": "status" }
      ]
    },
    {
      "type": "dashboard",
      "id": "overview",
      "name": "总览",
      "widgets": [
        { "id": "w1", "type": "stat", "title": "在用车辆", "query": "count where status=在用" }
      ]
    }
  ],
  "layout": { "overview": [{ "i": "w1", "x": 0, "y": 0, "w": 4, "h": 2 }] },
  "automations": [
    { "kind": "condition", "id": "a1", "name": "年检临期提醒", "table": "vehicles",
      "condition": "inspection_date <= date('now','+7 days')", "message": "车辆 {plate} 年检到期", "frequency": "daily" },
    { "kind": "llm_summary", "id": "a2", "name": "每周安全周报", "schedule": "0 9 * * 1",
      "prompt": "汇总上周安全事件", "output": "notify" }
  ],
  "permissions": { "roles": ["owner", "editor", "viewer"] }
}
```

### 字段类型白名单（`type`）

| type | 说明 | 约束 |
|---|---|---|
| `text` | 单行文本 | — |
| `number` | 整数或浮点 | — |
| `date` | 日期 `YYYY-MM-DD` | — |
| `datetime` | 时间点 `YYYY-MM-DD HH:MM:SS` | — |
| `boolean` | 布尔 | — |
| `enum` | 枚举 | **必填 `options` 数组** |
| `relation` | 关系外键 | **必填 `target`（目标表 id）**，存目标表行 id |
| `attachment` | 附件 | 存 `files/<row_id>/` 相对路径 |

### 视图类型（`type`）

- `grid` 表格 / `form` 表单 / `kanban` 看板（`group_by` 分组字段）/ `calendar` 日历（`group_by` 日期字段）。
- `views[].fields` 必须引用所属表已存在的字段 id。

### 仪表盘组件（`type`）

- `stat` 统计卡（数字+文案）/ `chart` 图表 / `list` 列表。
- `widgets[].query` 是受控查询片段（如 `count where status=在用`），后端白名单校验后执行。

### 自动化（`kind`）

- `condition` 条件提醒：单表 WHERE 片段 + `{field}` 变量插值消息模板，`frequency` = daily / hourly / cron。
- `llm_summary` 定时分析：cron `schedule` + 任务 `prompt`，产出落 `output` = notify / report / chat。语义综合走 sub-agent 隔离。

### 权限（可选）

- `permissions.mode`：`readwrite`（默认）或 `readonly`（拒绝 insert/update）。
- `permissions.allow_delete`：false 则禁止删行（防 LLM 误删独立开关）。非法值会被校验拒绝（fail-closed）。

## 布局设计档位（LLM 唯一的视觉编排权）

**配色不需要你管**：组件视觉全部走前端 CSS token（卡片背景/边框/文字层级/圆角），schema 里没有颜色字段，你也无法污染——照着填结构即可，视觉风格由渲染器保证。

**布局需要你编排**：`layout` 是 react-grid-layout 网格，这是 LLM 唯一能影响视觉效果的入口。

| 规格 | 值 |
|---|---|
| 网格列数 | 12 列 |
| 行高 | 30px（`h=2` ≈ 60px + 间距） |
| 间距 | margin 8px，容器 padding 4px |

**组件尺寸档位（照抄就行）**：

| widget type | 推荐 w × h | 说明 |
|---|---|---|
| `stat` 统计卡 | `3 × 2` | 一行大数字 + 标题，一排可放 4 个 |
| `chart` 图表 | `6 × 4` | 需足够绘图区，一排放 2 个 |
| `list` 列表 | `4 × 4` | 容纳 5+ 行，一排放 3 个 |

**编排规则**：
1. **总览先行**：stat 统计卡放第一排（`y: 0`），图表居中（`y: 2` 起），列表/明细沉底。
2. **同排顶边对齐**：同一排的组件 `y` 必须相同，否则视觉错位。
3. **宽度不越界**：`x + w ≤ 12`，越界会被后端校验拒绝（报错信息会告诉你具体哪项）。
4. **不要重叠**：虽然校验不拦（渲染器会推开），但重叠会打乱你的编排意图。
5. **`i` 必须匹配 widget id**：`layout` 每一项都要对应 `modules[].widgets[]` 里真实存在的 id，且每个 widget 都要出现在 layout 里，否则组件不显示。
6. **能不填就不填**：留空时前端按上面的档位自动流式铺排（stat 一排 4 个、chart 一排 2 个），效果可控；填错反而更丑。

## 开发流程契约

1. **听懂需求**：用户描述管理对象（车辆/人员/台账…），提炼实体字段、状态、关系、需要什么视图。
2. **产出 schema 提案并展示给用户**：用完整 WorkspaceSchema（含模块、字段、视图、仪表盘、自动化）。**必须先获用户明确确认，再调用工具**。
3. **调用 `workspace_build`**：传完整 schema + 可选种子数据（用户丢来的 CSV/Excel/文本整理成行）。后端自动建设（目录/定义/建表/导入/自测）。
4. **完成后叙述**：「建设完成，请测试」——提醒用户去工作台 Tab 验收；用户反馈继续迭代。
5. **迭代走 `workspace_update_schema`**：传**相对当前定义的完整新版本** schema。后端迁移：新增列/表自动建、删除的表/列**数据保留**、枚举存量校验提示。**先向用户展示改动点（新增/删除/修改）并获确认**，再调用，完成后叙述迁移结果。
6. **查询/统计**：`workspace_table_schema`（查结构）→ `workspace_query` / `workspace_count` / `workspace_group_count`（查数据/计数/分组计数）；写数据用 `workspace_insert` / `workspace_update` / `workspace_delete`。

## 工具族（workspace_*）

| 工具 | 用途 | 风险 |
|---|---|---|
| `workspace_build` | 创建工作台（完整 schema + 种子数据） | Caution，先确认 |
| `workspace_update_schema` | 增量迁移工作台定义 | Caution，先确认 |
| `workspace_table_schema` | 查询表结构（字段白名单） | Safe |
| `workspace_query` | 查询表数据 | Safe |
| `workspace_count` | 计数 | Safe |
| `workspace_group_count` | 分组计数（仪表盘 chart 用） | Safe |
| `workspace_insert` | 插入行 | Caution |
| `workspace_update` | 更新行 | Caution |
| `workspace_delete` | 删除行（尊重权限 allow_delete） | Caution |

## REST API（调试/核对用，LLM 优先走工具）

- `GET/POST /api/workspaces` — 列表 / 创建（注册表条目 + 目录 + 默认定义）
- `GET/PUT/DELETE /api/workspaces/{id}` — 详情 / 重命名 / 删除
- `PUT /api/workspaces/{id}/schema` — 更新定义（布局/模块变更持久化）
- `PUT /api/workspaces/{id}/layout` — 仅替换布局层
- `GET /api/workspaces/{id}/export` — 导出 .hwx 包；`POST /api/workspaces/import` — 导入（`new` 重生成 id / `merge` 整表 LWW 合并）
- `GET/POST /api/workspaces/{id}/automations`、`GET .../automations/runs` — 自动化定义与运行记录
- `POST /api/workspaces/{id}/files/upload`、`GET .../files/{filename}` — 附件
- `/api/workspaces/{id}/tables/{table}/query|insert|update|delete|count|group_count|changes|undo` — 数据 CRUD + 变更记录/撤销
- `POST /api/workspaces/{id}/change-log/purge` — 清理变更日志

## Gotchas

- **字段 id 是标识符**：必须过安全校验（小写字母数字下划线），非法 id 整个 schema 被拒。宁可 id 用英文蛇形，name 用中文。
- **enum 必须带 options，relation 必须带 target**，缺失校验直接拒。
- **视图 fields / group_by 必须引用已存在字段 id**，否则校验失败。
- **layout 是 react-grid-layout JSON**：`{ "<dashboard_id>": [{ "i": "<widget_id>", "x", "y", "w", "h" }] }`，widget 要出现在 layout 里才可见。
- **删除是"软删"**：`workspace_update_schema` 删除表/列时数据保留（防误删），别跟用户说"已删除数据"。
- **只读工作台**：`mode: readonly` 时 insert/update/delete 一律拒绝（数据端点 + LLM 工具统一校验），查询不受限。
- **零上下文膨胀**：用户丢大 CSV/Excel → 不要全文塞进对话，提炼表头+前几行样例进种子数据，或提示分批导入。
- **多工作台隔离**：每个工作台独立 SQLite 库 + 独立定义，Tab 切换互不影响。
- **不确定结构时先 `workspace_table_schema` 再 `workspace_query`**：别凭记忆猜列名。

## 能力边界（强制约束 — 只承诺已落地的）

**✅ 可安全使用**：数据表（grid/form 视图，含行增删改/搜索/CSV 导出）、仪表盘（stat/chart/list + 布局）、数据 CRUD/计数/分组计数、`undo_last` 撤销、导出导入 .hwx、自动化（condition/llm_summary）、权限、内置模板。

**⚠️ schema 合法但没有渲染器 —— 不要主动承诺**：
- `kanban` 看板视图、`calendar` 日历视图：后端 `validate` 会通过（`group_by` 校验），但**前端没有 KanbanView / CalendarView 组件，用户看不到**。用户要求看板/日历时，先说明"当前渲染为表格、看板视图尚未实现"，由用户决定要不要先建数据结构。

**❌ 未实现 —— 禁止向用户承诺**：
- 自定义组件（`widgets/`）、自定义动作（`actions/`）、自定义 SQL 视图、WorkspaceBridge —— 全部未落地。
- 超出 CRUD 的需求 → 走 **custom-app 机制**（`custom_app_manager`，已成熟）；语义判断类走 `llm_summary` 自动化。

完整规范见 `docs/工作台LLM开发规范.md`（技术栈三层、schema 契约、流程、工具族、边界）。

## `@work` 快捷入口

- 聊天输入 `@work` → 弹出工作台面板：首项"创建工作台"，其余为已建工作台。
- 选"创建工作台" → 输入框预填 `@work 创建工作台 需求：`，用户补全需求后发送。
- 选已建工作台 → 插入 `@work <工作台名> `，发送时自动注入 `【目标工作台】\n- <工作台名>（workspace_id: <id>）` 头，直接对该工作台操作（查询/迭代），无需再解释是哪个。
- 系统提示中已有 `## 已建的工作台` 索引（`@work:<id> — <name>`）：用户用 `@work` 引用已建工作台时，先按索引确认存在，**不要重复创建**。
