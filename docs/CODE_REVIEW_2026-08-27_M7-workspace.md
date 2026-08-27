# M7 工作台模块全栈审查报告（Rust 后端 + React 前端）

**日期**：2026-08-27
**审查对象**：commit `fb136499`（M7.2/M7.3 权限模型）+ `e220fc0f`（M7.4 前端引导），及关联链路
- 后端：`crates/hermix-workspace/` 全部、`crates/hermix-server/src/workspace_backend.rs`、`dashboard/{workspaces,config,custom_providers}.rs`
- 前端：`apps/desktop-tauri/src/components/{ModelSelector,WorkspacesView}.tsx`、`components/workspace/*`、`api/rest.ts`、`types.ts`

**审查原则**：拒绝临时方案、拒绝过度设计、命名自解释、异常精准处理、用类型系统防非法状态。
**状态标记**：✅ 通过 / ⚠️ 需改进 / ❌ 严重问题。P0 = 合入主干前必须修。

---

## Part A · Rust 后端架构审查

### A.0 七维度结论总表

| # | 维度 | 结论 | 一句话 |
|---|---|---|---|
| A1 | 架构合理性 | ⚠️ | 分层清晰，但安全不变量放在调用方而非单一分发点 |
| A2 | 封装与内聚 | ✅ | 字段可见性/API 暴露得当；建议补 `From<ServiceError> for HttpError` |
| A3 | 代码精炼度 DRY & KISS | ⚠️ | 关键逻辑重新发明了已有的轮子 |
| A4 | 彻底性 | ❌ | 3 个 P0 + 一批静默吞错 |
| A5 | 可测试性 | ✅ | tempdir 全链路测试文化好；缺 3 个放走 P0 的测试场景 |
| A6 | 注释与可读性 | ⚠️ | 一条与实现直接矛盾的注释；M-milestone 编号噪音 |
| A7 | 性能与资源 | ⚠️ | 受保护写请求双重解析 schema；merge 导入 N+1 |

### A1-A7 P0 问题（三个）

#### P0-1【fb136499 引入】孤儿过滤误杀环境变量配置的 provider

**位置**：`crates/hermix-server/src/dashboard/config.rs:241-247`

```rust
let is_configured = |pid: &str| -> bool {
    cfg.llm_providers.get(pid)
        .and_then(|p| p.api_key.as_ref())
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
};
```

只检查 config.yaml 里的 `api_key`，而本项目内置 provider 支持纯环境变量配钥
（`OPENAI_API_KEY` 等，映射见 `hermix-config/src/loader.rs:691-708`，
agent_builder 启动时注入）。env-key 用户聊天完全可用，但模型下拉被过滤成空。

**根因**：`hermix-config` 已有权威函数 `provider_has_key(config, provider)`
（`loader.rs:751`，config 或 env 任一命中即 true），此处手写了它的一半。

**修复**：

```rust
use hermix_config::provider_has_key;
let is_configured = |pid: &str| provider_has_key(&cfg, pid);
```

**连带前置债**：`env_var_name_for_provider` 在 `hermix-config/loader.rs:691` 与
`hermix-agent/agent_builder.rs:255` 各一份且已漂移——agent 版多出
baichuan/tencent/spark/yi 四家，config 版没有 → `provider_has_key`
不认识这四家。删除 agent 侧副本，统一从 hermix-config 导入一份完整映射。

#### P0-2 权限防线绕过：import merge 可清空只读工作台

**位置**：`dashboard/workspaces.rs:442-483`（merge 分支）、`:517-551`（rewrite_table）

M7.3 不变量是"数据端点与 LLM 工具在写操作前统一调用 check"，但 merge 导入对
目标表做全量 delete+insert，全程不经 `check_write`/`check_delete`。
readonly / allow_delete=false 的工作台一条 `POST /api/workspaces/import` 即可被覆盖甚至清空。

**根因方案**：不变量收进 service 层，返回能力受限的写句柄，一次消灭 6 处重复守卫
（3 REST + 3 trait impl），顺带消除双份读盘（见 A7-1）：

```rust
// service.rs
/// 打开工作台并确认写权限；拒绝 → PermissionDenied。
/// 返回值即能力：只有拿到 Writable 才能写入。
pub fn open_writable(&self, id: &str) -> Result<Writable, ServiceError> {
    let schema = self.read_schema(id)?;              // 只读一次盘
    let perms = schema.permissions_parsed();
    if !perms.can_write() {
        return Err(ServiceError::PermissionDenied(
            format!("工作台[{id}]为只读，拒绝写操作")));
    }
    let store = LocalSqliteStore::open(self.data_path(id), schema)
        .map_err(|e| ServiceError::Store(e.to_string()))?;
    Ok(Writable { store, allow_delete: perms.allow_delete })
}

pub struct Writable { store: LocalSqliteStore, allow_delete: bool }

impl Writable {
    pub fn insert(&self, t: &str, rows: &[Value], by: &str) -> Result<Vec<String>, StoreError> {
        self.store.insert(t, rows, by)
    }
    pub fn update(&self, t: &str, id: &str, p: &Value, by: &str) -> Result<(), StoreError> {
        self.store.update(t, id, p, by)
    }
    pub fn delete(&self, table: &str, row_id: &str) -> Result<(), StoreError> {
        if !self.allow_delete {
            return Err(StoreError::Validation("该工作台禁止删除（allow_delete=false）".into()));
        }
        self.store.delete(table, row_id)
    }
}
```

调用点从「check_write()? + open_store()?」两步变一步，忘带权限即编译不出的表达方式不存在，
但"拿到 Writable 才能写"由类型承载。builder.rs:108 灌种子数据直接构造
`LocalSqliteStore`、不走 service，不受影响（建台期本就该写）。
import merge 改走 `open_writable` 后此洞闭合。

#### P0-3【M5 遗留】merge 导入悄悄重生成所有行主键——注释与实现矛盾

**位置**：`store.rs:336` vs `dashboard/workspaces.rs:534-535`

```rust
// store.rs row_to_params：无条件覆盖 id
out.push(("id".to_string(), rusqlite::types::Value::Text(uuid_v4())));
```

`rewrite_table` 注释宣称"保留包内原始 id/updated_at/updated_by（行主键 UUID 不重生成）"，
但传入的 `"id"` 因不在字段白名单中被静默丢弃。后果：
1. 每次 .hwx merge 后所有行获得新 UUID → **Relation 外键全部悬空**；
2. created_at/updated_at 由 DDL default 重置 → 下次 LWW 比较错误时间戳，
   "最后编辑胜出"退化为"最后导入胜出"；
3. M5.5 唯一的语义文档（该注释）是假的。

**修复**（尊重调用方显式提供的 id，缺省才生成）：

```rust
let id = obj.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
    .map(str::to_owned)
    .unwrap_or_else(uuid_v4);
out.push(("id".to_string(), rusqlite::types::Value::Text(id)));
```

补两条回归测试：(a) 插入携带 `"id"` 的行 → 返回原 id；(b) 含 relation 数据的
工作台做 merge 导入 → relation 引用仍可解析到目标行。

### A 维度详评要点

**A1 架构 ⚠️**
- 依赖方向干净：workspace crate 无 HTTP 依赖；REST 与 LLM 工具共享同一 store 语义真实落地。
- 不变量放置层级错误是 P0-2 的架构根源：跨入口不变量必须由单一分发点强制（open_writable 能力模式）。
- 路径双事实来源：`workspace_backend.rs:310` 用 `hermix_config::paths::hermix_home()`，`dashboard/workspaces.rs:57` 用 `state.hermix_home` —— 应统一由 state 注入 root。
- `workspace_backend.rs:264-266` 手工拼 `ws_dir/schema_path/data_path`，`WorkspaceService::schema_path()/data_path()` 即为此存在，应复用。

**A2 封装 ✅**
- 建议（非必改）：同 crate 内实现 `From<ServiceError> for HttpError`
  （NotFound→404 / PermissionDenied→403 / 其余→500），消灭 workspaces.rs 内联 FORBIDDEN 映射 ×3 及各处 map_err。

**A3 精炼 ⚠️**
- `is_configured` = `provider_has_key` 残缺复制（DRY 违规直接产生 P0-1）。
- env 映射跨 crate 双份漂移（见上）。
- `model.rs:267-271` `from_value` 手写字面量未复用自己的 `default_workspace()`；
  该方法本身应改为标准 `Default` 实现（PermissionMode 加 `#[default] ReadWrite`）。
- `model.rs:304-316` 同一 WorkspaceSchema 两个相邻 impl 块——合并。
- `"readwrite"/"readonly"` 字符串出现在 as_str/parse/validate 三处无真理源——
  validate 复用 `PermissionMode::parse`（提为 pub(crate)）。

**A4 彻底性 ❌（除 P0 外）**

| 位置 | 行为 | 判定 |
|---|---|---|
| workspace_backend.rs:187 `rows.as_array().cloned().unwrap_or_default()` | 非 JSON 数组 → 插 0 行返回 ok:true | 必修：非法形状返回 InvalidParams |
| workspace_backend.rs:158-159,171-172 filters `unwrap_or_default()` | 过滤失败静默当无过滤，返回错误结果集 | 必修：InvalidParams |
| validate.rs:34-35 `get("mode").and_then(as_str)` | `"mode": true` 跳过校验→fail-open 解析为 ReadWrite，限制静默失效 | 必修：mode 存在必须是合法字符串 |
| service.rs:73-74 读失败一律 NotFound 且丢原始错误 | EPERM/损坏文件误报"不存在" | 建议：按 ErrorKind 区分 |
| workspaces.rs:197-199 rename 定义回写双吞 | 注册表名与定义名静默漂移 | 至少 tracing::warn! |
| workspaces.rs:220-222 remove_dir_all 失败零日志 | 目录残留不可见累积 | 加 warn 日志 |
| custom_providers.rs:240 `let _ = auto_correct_model(&mut config)`（既有） | 修正本地副本后既不落盘也不推送——死变异 | 删除或走完保存+推送 |

做得对的：migrate_schema 入口先 validate_schema（migrate.rs:77）、DDL 事务化、删列不物理删；
provider 删除级联同时落盘+推 live service（custom_providers.rs:292-307），闭环完整。

**A5 可测试性 ✅**
tempdir 全链路集成测试、新旧 schema 兼容矩阵、LLM 六步工具链端到端都有。
三个缺口正好对应三个 P0 为什么存活：readonly 工作台的 import merge 无测试；
外部 id 保持无断言；env-key provider 的 model/list 无测试。随修复一并补回归。

**A6 注释与可读性 ⚠️**
- 硬伤：workspaces.rs:533-535 注释描述了不存在的行为（错误的注释比没有危害大一个数量级）。
- 噪音：`// M7.3: …` 等 milestone 编号注释 6 处承载项目管理信息而非技术根因；
  规范应是陈述不变量，milestone 留给 commit message。

**A7 性能 ⚠️**
1. 受保护写请求两次读+解析 workspace.json（check_write 的 read_schema + open_store 再 read_schema）——open_writable 方案顺带消除。
2. merge 导入全表读两遍（workspaces.rs:468 与 rewrite_table 内部 :523）+ 逐行 delete 循环。
   低成本改法：ids 上游传参；store 增加 `delete_all(table)` 单条 DELETE。
3. 无需动作项：SQLite per-request 连接、Mutex<Option<Arc>> 短临界区 clone 模式均合格。

---

## Part B · 前端审查（desktop-tauri）

### B.0 四面结论总表

| 面 | 结论 | 一句话 |
|---|---|---|
| B1 对接（前后端契约） | ❌ | 3 个后端特性前端零对接；模型选择器与后端 P0-1 叠加放大成数据丢失体验 |
| B2 效果（功能是否真的能用） | ❌ | 仪表盘永远空数据；表 CRUD 无任何 UI 入口；FormView 是孤儿组件 |
| B3 设计（交互与视觉） | ⚠️ | hover 双层 mouseleave 取消误触率高；UUID 手输等不可用控件；权限状态不可见 |
| B4 代码质量 | ⚠️ | 死表达式、渲染期读 DOM、第 4 份 provider 清单、localStorage 权威规则自相矛盾 |

### B1 对接问题

#### B1-1【最严重】ModelSelector reconcile × 后端 P0-1 = 用户勾选每次 mount 被清空

`ModelSelector.tsx:113-137`：mount 时向后端拉列表，凡 localStorage 中不在后端集合里的
勾选项一律剔除并**写回 localStorage**。叠加 P0-1（env-key provider 被后端误判为孤儿）：
用户勾选 deepseek(env) + openai(config) → 每次 mount deepseek 消失并被持久化 →
重新勾选 → 重启又消失。「取消勾选白做」的历史教训（:81-86 注释记载）以镜像形态复活：「勾选也白做」。
修复 P0-1 即消解；但前端规则本身还有独立缺陷（见 B4-3）。

#### B1-2 M7.2/M7.3 权限模型前端契约缺失

- 后端：readonly 工作台 REST 写操作返回 403（中文 message）。
- 前端：`types.ts:588` `permissions: unknown`——没有任何代码消费权限；UI 无法显示
  「只读」徽标、无法禁用写入按钮（未来一旦接入 CRUD 表单）。
- **方案**：types.ts 收紧为 `{ mode?: "readwrite" | "readonly"; allow_delete?: boolean } & Record<string, unknown>`；
  WorkspacesView 拉详情时计算 `canWrite/canDelete` 并下传 WorkspaceRenderer → TableView/FormView，
  锁定视觉态（徽标 + 禁用），错误兜底 403 toast 兜不住的体验提前拦截。

#### B1-3 数据写路径整条断线：API 有、UI 无

`rest.ts:316-373` 的 `workspaceInsertRows/updateRow/deleteRow/countRows` 生产代码**零调用**；
`FormView.tsx` 仅被 FormView.test.tsx 引用（孤儿组件）；TableView 只有只读表格，无新增/
编辑/删除按钮、无行点击事件。即：**用户通过界面只能看表，不能写一行数据**。
写数据的唯一途径是对话让 LLM 操作。这与"Schema 驱动工作台"的产品主张直接冲突。

#### B1-4 仪表盘数据管道断线

`WorkspaceRenderer.tsx:130-135` 渲染 DashboardView 时从不传 `widgetData`
（DashboardView.tsx:33 prop 定义齐全）→ StatWidget 永远 "—"、ChartWidget 永远空图、
ListWidget 永远"暂无数据"。widget.query 字段宣称"受控查询片段，后端白名单校验执行"
（DashboardWidgets.tsx:5-6），实际从无任何执行者。

#### B1-5 M5 .hwx 导出/导入零前端入口

后端 `GET /{id}/export`、`POST /import` 完整落地（fb136499 前的 62944f17），
前端源码中 `.hwx` 出现次数为 0。协作分发功能对外不可达。

#### B1-6 布局保存的丢失更新竞争

`WorkspacesView.tsx:129-146` handleLayoutChange 把内存中的**整个 schema 快照** PUT 回
`update_workspace_schema`。用户拖拽布局的同时若 LLM 正在增删字段，两者交织时
先完成者的字段变更会被后完成的陈旧快照覆盖（LWW 整体替换，无版本号）。
**方案**：新增 `PUT /api/workspaces/{id}/layout`，服务端读改写仅合并 layout 键
（schema 其他键不动），前端只传 layout。

#### B1-7 错误文案直出原始 HTTP 体

`rest.ts:179-181` throw `new Error(text)`，text 是后端原始响应体；上层
`toastError(\`…失败: ${String(e)}\`)` 会拼出 JSON 串或 "[object Object]"。
建议 fetchApi 统一提取 `message` 字段（HttpError 响应）再抛。

### B2 效果问题（跑起来才发现的）

| # | 位置 | 问题 | 方案 |
|---|---|---|---|
| B2-1 | WorkspacesView.tsx:100-110 | 多表加载失败逐表弹 toast，N 表故障弹 N 个 | 聚合为一枚汇总 toast（失败表名列出） |
| B2-2 | TableView.tsx:55 | `viewportH` 在 render 中读 DOM，首帧拿 fallback=400 且无 ResizeObserver：窗口初始比 400 高则底部行缺渲染，直到滚动才恢复 | 改 state + ResizeObserver / useLayoutExpression；至少 onScroll 外监听 resize |
| B2-3 | WorkspacesView.tsx:38 | TABLE_QUERY_LIMIT=500 静默截断：大表显示前 500 行且无任何提示，用户以为看全了 | 行数计数（count API 就有！）vs rows.length 不一致时显示"已截断，共 N 行" |
| B2-4 | DashboardWidgets.tsx:107-110 | ListWidget（接通后）将 `Object.values(row).join(" · ")` 连系统列 id(UUID)/created_at/updated_at 一起渲染 | 白名单列渲染（schema fields）或过滤系统列 |
| B2-5 | FormView.tsx:231-240 | attachment 字段 input[type=file] 选完文件后值从未转换，onSubmit 提交 null——用户选了文件被静默丢弃 | 短期禁用该字段并标注"即将支持"；长期接 files/<row_id>/ 上传 |
| B2-6 | WorkspacesView.tsx:417 | 空态引导「从模板开始」调用的 openTemplates 是 toggle：面板开着再点会关闭 | 空态按钮应强制打开 `setShowTemplates(true)` |

### B3 设计问题

1. **hover 即消失的两段式确认**（TabItem confirm-delete，WorkspacesView.tsx:512
   `onMouseLeave={onCancel}`，外层 ：248 还有一层 resetMode）：鼠标稍滑出确认框即静默撤销；
   纯键盘不可达（无 focus 支持）。模板浮层同样 onMouseLeave 关闭（:339）。
   删除高风险操作应改为 ESC/点外关闭或保留 300ms 延迟，模板面板加 Escape 关闭。
2. **Relation 字段要求手输 UUID**（FormView.tsx:218-230 placeholder"关联 xx 表行 id"）：
   对人类不可用；叠加 P0-3（merge 重生成 id）后引用链彻底断裂。
   方案：下拉目标表已有行（rows 已在 tableRows 里缓存）供选择；短期至少展示目标表名候选列表。
3. **权限不可见**：readonly/allow_delete 在 UI 无任何呈现（见 B1-2），用户只能靠失败感知。
4. **ModuleTab tooltip 暴露内部 id**（WorkspaceRenderer.tsx:104 `title={module.id}`）
   而 TabItem 用展示名（WorkspacesView.tsx:539）——信息气味不一致。
5. **空态文案承诺无处兑现**：'AI 会自动为你建设'（WorkspacesView.tsx:412）但没有跳转对话输入框的入口/高亮，
   引导停留在口号。低成本闭环：按钮点击后聚焦全局输入框。

### B4 代码问题

1. **死表达式** `WorkspacesView.tsx:183`：`${smoke.length ? "" : ""}` 是无操作残迹
   （原意显然想展示 smoke 摘要）；`createWorkspaceFromTemplate` 返回的 smoke 数组被整体丢弃。
   清理：要么展示"3 张表、11 条数据可查"式的摘要，要么接口不再返回。
2. **provider 注册表第 4 份副本** `ModelSelector.tsx:6-11` `BUILTIN_PROVIDER_IDS`：
   前后端已有 3 份（loader.rs:691、agent_builder.rs:255、custom_providers custom- 约定），
   这份还漏了 `nous` → nous 分组被误标为自定义 provider。
   根治：后端暴露 `GET /api/providers/meta`（id/is_builtin/env_var_name）,
   前端启动拉取一次缓存，四处清单收敛为一份。
3. **reconcile 权威规则自相矛盾** `ModelSelector.tsx:121-123`：
   `rawBackend.length ? filtered : baseEnabled`——后端返回空列表（全部 provider 未配钥的新装场景）
   时跳过过滤，旧 orphan 条目原样保留，「避免选中无法发消息」的目标恰在最需要的场景失效。
   规则应统一为：请求成功 → 后端集合即权威（空也是权威）。
4. **localStorage 吞错**：`catch {}` 贯穿 ModelSelector/存储兼容层。桌面应用可接受降级，
   但至少 console.debug 留痕，否则"配置莫名重置"类工单无从排查。
5. **React key 稳定性**：TableView.tsx:127 `key={row.id}`——若某行缺 id 则 undefined 重复 key。
   数据源自后端必有 id，风险低，但 `key={String(row.id ?? idx)}` 一行防御即可。
6. 值得肯定：乐观 UI 克制（创建后 refresh 再 setActiveId 无竞态）、模块导航懒注册、
   Tagged union 类型镜像（types.ts WorkspaceModule 与 serde tag 一致）、Query limit 上限意识都好。

---

## Part C · 修复执行方案

### PR-1【阻塞合流，后端】数据正确性三连
1. P0-1 `is_configured` → `provider_has_key`；合并 env 映射副本（删 agent_builder 版）。
2. P0-2 service 增加 `open_writable` 能力句柄；REST insert/update/delete、workspace_backend
   insert/update/delete、**import merge** 全部改走它；删除 6 处散装 check。
3. P0-3 row_to_params 尊重外部 id；修正 rewrite_table 注释与 LWW 文档一致性。
4. 回归测试：readonly import merge 被拒 / 外部 id 保持 / env-key provider 出现在 model list。
验收：`cargo test -p hermix-workspace -p hermix-server --lib` 全绿；手工验证 .hwx merge 后 relation 可解析。

### PR-2【后端小项】彻底性清扫
filters/rows 非法形状转 InvalidParams；validate mode 存在必须合法字符串；
ServiceError→HttpError From 化；吞错点补 warn；布局专用 PUT layout 端点（B1-6）。

### PR-3【前端打通】让已建的变成可见可用的
1. 权限契约：types.ts permissions 类型化 + readonly 徽标 + canWrite/canDelete 贯穿 renderer。
2. TableView 行操作（新增弹 FormView、行编辑、删除复用两段确认但去掉 mouseleave 取消）。
3. 仪表盘 widgetData 接线（stat→countRows，list→queryTable，chart 先 stat 占位或明确砍掉 chart）。
4. ListWidget 白名单列渲染。
5. `.hwx` 导出/导入入口（Tab 栏溢出菜单即可，最小可用）。

### PR-4【前端打磨】
smoke 死表达式清理或利用；BUILTIN_PROVIDER_IDS 收敛到 /api/providers/meta；
reconcile 空=权威规则修正；ResizeObserver 接入 TableView；attachment 字段显式禁用态;
relation 选择器（消费 tableRows 缓存）；providers meta 端点前后端一对。

### 明确不做（YAGNI 记录）
- 权限不做角色 ACL（维持工作台级读写保护定位，§17.4 原方案照旧预留）。
- 不引入 mock 框架/tempfile 以外的测试设施；不为 SQLite 加连接池。
- Chart 不做通用 SQL 图表 DSL——沿 widget.query 受控片段或推迟到真需求出现。

---

## 附：本次审查方法备注
- 后端结论可复现于 HEAD（改动已合入 fb136499 / e220fc0f），非过期快照。
- 前端效果类问题（B2 系列）基于静态链路追踪判定，建议修前各花 ≤10 分钟手测复核一遍现象
  （特别是仪表盘空数据与模型勾选回滚两条，预期与静态分析一致）。

---

## 附2 · 修复落地记录（2026-08-27 同日回写）

本报告发现的问题已于当日修复并验证。逐项状态：

### Part A 后端
| 项 | 状态 | 落点 |
|---|---|---|
| P0-1 is_configured 半份副本 | ✅ 已修 | `dashboard/config.rs` 改用 `provider_has_key` |
| P0-1 前置债 env 映射双份漂移 | ✅ 已修 | `hermix-config/loader.rs` 新增 `BUILTIN_PROVIDER_ENV_VARS` 唯一真理源（含国内四厂商），`agent_builder.rs` 两函数改 re-export；新增 `GET /api/providers/meta` 派生端点 |
| P0-2 import merge 绕过权限 | ✅ 已修 | service 新增 `open_writable()`/`Writable` 能力句柄（单次读盘）；REST ×3、LLM 工具 ×3、**import merge** 全部迁移；散装 `check_write/check_delete` 删除；`errors.rs` 增加 `From<ServiceError>`（404/403/500 单点映射） |
| P0-3 merge 重生成行主键 | ✅ 已修 | `row_to_params` 尊重外部 id；新增 `replace_table_rows()`（事务整表替换、保留 created_at/updated_at/updated_by）；回归测试锁定（id 不重生成 + 时间线不重置 + relation 引用稳定） |
| rows/filters 静默吞错 | ✅ 已修 | workspace_backend query/count/insert 非法形状 → `InvalidParams` |
| mode 非 string fail-open | ✅ 已修 | validate 复用 `PermissionMode::parse`，存在即必须合法；测试覆盖 `true`/数字拒绝 |
| model.rs 双 impl 块/default_workspace/字面量分叉 | ✅ 已修 | Default 派生、合并 impl、validate 复用 parse |
| rename/删目录吞错、dead auto-correct | ✅ 已修 | 补 `tracing::warn!`；create 的 auto-correct 移到持久化前并真正落盘推送 |
| update_schema 手工拼路径 | ✅ 已修 | 复用 `service.schema_path()/data_path()` |

### Part B 前端
| 项 | 状态 | 落点 |
|---|---|---|
| B1-1 reconcile×P0-1 勾选被清 | ✅ 根因消解 | P0-1 修复 + 前端规则改为「请求成功即权威（空也是权威），失败保留本地」 |
| B1-2 权限契约缺失 | ✅ 已接通 | `types.ts` permissions 类型化；`workspacePermissions()` 解析贯穿 Renderer→TableView/FormView；Tab 栏 🔒 只读徽标 |
| B1-3 写路径断线 | ✅ 已接通 | TableView 行操作（新增/编辑 FormView 面板、删除两段确认无 hover 取消）；WorkspacesView CRUD 回调→rest.ts 写 API；关系字段下拉候选；attachment 显式禁用说明 |
| B1-4 仪表盘数据管道断线 | ⚠️ 部分接通 | widgetData 全链路已打通；stat 卡实现受控查询最小执行器（`count[from T][where f=v]`，附解析器单测）；list/chart 无后端解释器保持空态——属 M7 未规划功能，见"明确不做" |
| B1-5 .hwx 前端入口缺失 | ❌ 未做（后续项） | 需 Tauri 文件对话框 + blob 下载接线，超出本次修复范围 |
| B1-6 布局保存丢失更新竞争 | ✅ 已修 | 新增 `PUT /{id}/layout` 仅替换布局层；前端防抖保存改走该端点 |
| B1-7 toast 直出原始响应体 | ❌ 未做（低优） | fetchApi 错误提取 message 属全局改造 |
| B2 系列（ResizeObserver/截断提示/ListWidget 系统列/smoke 死表达式/模板 toggle） | ✅ 已修 | ResizeObserver 视口测量（jsdom stub 补在 vitest.setup）；截断提示用真实 count；ListWidget 排除系统列且 key 用行 id；toast 展示自测通过数；空态按钮强制开面板 |
| B1-8 ModuleTab tooltip / ModelSelector nous 缺失 | ✅ 已修 | tooltip 改展示名；静态清单补 nous 且以后端 meta 为权威合并 |

### 验证基线
- `cargo test -p hermix-workspace --lib`：74/74 ✅（含 P0-3 回归 + 能力句柄测试）
- `cargo test -p hermix-config --lib`：138/138 ✅
- `cargo test -p hermix-server --lib`：139/139 ✅
- `cargo check --workspace --all-targets`：0 error ✅
- `pnpm tsc --noEmit`：EXIT=0 ✅
- `pnpm vitest run`：15 files / 131 tests 全绿 ✅（含新增 parser 5 测试）
- `hermix-agent --lib`：1053 通过、1 失败（`status_callback_receives_empty_response_retry_notice`，
  经 stash 干净树复核为预存失败，属 e220fc0f 记录的"已知遗留 23 失败"范畴）

### 明确不做（YAGNI 回执）
角色 ACL、mock 测试设施、SQLite 连池、通用图表 DSL —— 维持原判。
另记：Chart/List 组件的后端白名单查询解释器是下一个真需求出现时再立项的功能缺口。
