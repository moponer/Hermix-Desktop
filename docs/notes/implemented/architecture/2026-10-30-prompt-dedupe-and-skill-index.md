# Decision Note: 提示词去重与技能索引可观测

Status: implemented — 2026-10-30

## Decision

三条独立的小修，都指向「常驻文本只说一次、说清楚、不静默丢东西」：

| 位置 | 改动 |
|---|---|
| `directions::FILE_LIFECYCLE_GUIDANCE` | 合并三份「文件写到哪里」的规则（详见下），成为唯一出处；补上「计划/笔记落 `docs/notes/proposed/{class}/`」「新 crate / 顶层目录 = 架构决策」「scratch 目录具体路径 + 免审批清理」 |
| `prompt_assembly` stable 层 | 删除 `Temp File Hygiene` 块（~1.1 K 字符），规则归并到上一条 |
| `directions::collect_directions` | 删除 `filewrite.full` / `filewrite.short` 两条 per-turn 方向（~1.3 K / ~0.35 K），同一策略不再每请求付两次 |
| `directions::TOOL_USAGE_GUIDANCE` | 「**Default: prefer `delegate_task` for any file read**」改为按体积/用途判断，并明确「要编辑的文件先自己 `read_file`」 |
| `prompt_assembly::render_skill_index` | 行数上限提为常量 `SKILL_INDEX_MAX_ROWS`，超限 `warn`（带 drop 数）；names-only 降级改为**全量列名**、只在名字本身超预算时才截并显式告知 「另有 N 个」；两种形态各发一条日志 |

## 问题（实测 + 代码）

### 1. 同一条规则三处重复，措辞互相矛盾

| 位置 | 说法 |
|---|---|
| stable `Temp File Hygiene`（~1.1 K） | 临时文件**一律写到** `%TEMP%\hermix-scratch\`，收尾 `storage_clean` 删 |
| directions `FILE_WRITE_DISCIPLINE`（full ~1.3 K / short ~0.35 K） | 「Throwaway scripts → **do not write them to disk at all**」 |
| stable `FILE_LIFECYCLE_GUIDANCE`（~1.9 K） | deliverable vs scaffolding、写不写盘、删不删 |

同一决策两个相反答案，模型只能自己调和。三块合计 ≈4.3 K 字符/请求，合并后 ≈2.7 K。

### 2. 一条绝对化指令与编码回路对撞

- `TOOL_USAGE_GUIDANCE`：`Default: prefer delegate_task for any file read.`
- `CODE_DOMAIN_DIRECTIVE`：`改前先读：任何编辑前用 read_file 看当前内容`
- `read_file` 自己的描述：`patch requires the old text to match EXACTLY`

实测该会话 `read_file` 32 次 vs `delegate_task` 2 次——模型事实上无视了那条默认。
它没有起到约束作用，只是每轮付费的噪声。改为「按体积/用途判断 + 要编辑的文件先自己读」。

### 3. 技能索引静默丢 7 个 + 形态不可观测

本机 `~/.hermix/skills` 有 **87 个** skill，而索引渲染写死 `take(80)`：

- 超出部分**无声消失**（按名字排序，固定丢字母序最后 7 个），`skills_list` 能看到，
  模型不知道它们存在；
- 20 K 预算超限时整块降级成 names-only，**所有描述（也就是激活词）一起消失**，
  而当时没有任何日志能区分「当前是 detailed 还是 names-only」。

现在：超行数上限发 `warn`（带 `dropped` 数）；names-only 保留全量名字（名字约 25 字符/个，
87 个才 ~2.2 K，远低于 20 K 预算），只在名字本身超预算时按预算截并追加
「另有 N 个未列出，用 `skills_list` 查看」；两种形态各发一条 `hermix.skills_index` 日志。

## Alternatives considered

- **把 Code 域指令改写成 skill**：不成立。域模式是「不可漏匹配的常驻约束」（写不写文件、
  改完验不验证），skill 是按需匹配的，漏匹配等于没有约束。技能的定位是「具体方法论」
  （workflow-tdd / debugging-methodology …），模式的定位是「这一轮的工作纪律」。二者不互换。
- **把条件性块（`Custom App Dev Loop`、`git_safety`、`url_safety`、`web_automation`）
  按工具集门控**：方向正确，但 `build_stable_layer` 的缓存键只包含「有没有工具」
  （`compute_prompt_cache_key(..., !tool_schemas.is_empty(), ...)`），不含工具**集合**。
  一旦内容随工具集变化而不进键，就会命中另一组合渲染出来的缓存文本。必须先扩键，
  单独一次改动，本次不动。
- **技能索引直接取消 80 行上限**：87 个全量详细描述会稳定超过 20 K 预算，于是永远走
  names-only —— 等于用「永远没有描述」换「一个不漏」，得不偿失。保留详细优先、
  超限才降级，并且降级时不再丢名字。
- **删掉 `storage_clean` 描述里的 scratch 说明**：它是工具自己的契约（该工具可能不在
  本轮路由里），与系统提示的常驻规则不冲突，保留。

## Consequences

- 每请求少 ≈1.6–2.5 K 字符（合并三块文件规则 + 去掉 per-turn 的两份 filewrite 方向）。
- 提示词里不再存在「写盘」相关的自相矛盾；编码回路拿到的是可执行的判断（先读再改）。
- 技能索引：87 个 skill 在 names-only 形态下全部可见；detailed 形态下仍有 80 上限，
  但越界会留下 warn 日志，可据此调 `SKILL_INDEX_MAX_ROWS` 或按类别配额。
- 用户若曾用 `$HERMIX_HOME/guidance/filewrite.md` 覆盖旧块，该覆盖**失效**（tag 已删）；
  文件规则现在只认 `guidance/file_lifecycle.md`。
- 回归锁：`filter` 类测试不涉及这些字符串；`direction_audit` 的测试自建方向列表，
  不依赖 `collect_directions` 的条目数。
