# Decision Note: 批次 J2（resource_guard 落盘路径拦截）不实施

Status: rejected — 2026-10-01，与 2026-09-26 的既定策略冲突

## Decision

**不做 J2**，保留 J1（系统提示文件写入纪律）与 J3（check-hygiene.sh
门禁）。J2 原方案要求在 `resource_guard` 里对 `write_file` / `patch` /
`terminal` 重定向写的目标路径做形态检查（仓库根散件模式命中即弹审批卡）。

## Alternatives considered

- **按 J2 原文在 `resource_guard` 实现** —— 否决。它与一条 5 天前的明确定策
  直接冲突：`local_agent_service.rs:1120-1129` 写着「文件内容写入
  （write_file / patch / move_file）不做资源门拦截。2026-09-26 定策：
  "写文件不用审批，只有删除等高危才审批"」，且有测试钉死
  （`resource_guard_gates_destructive_file_tools_outside_grant` 断言
  `write_file`/`patch` 即使写未授权路径也返回 `None`）。实施 J2 等于
  正式推翻该定策 + 改其测试，代价与收益不匹配。
- **改在 `credential_guard` 层做路径形态检查** —— 未采纳（技术上可行，
  比弹卡更合适），但与 A 方案重复：J1 已在提示词层约束、J3 已在 CI 层
  拦截，规范类问题不需要再加一层运行时审批。
- **静默改写路径**（把仓库根散件自动挪到 scratch）—— 否决。替用户决定
  文件去哪，等于引入新的隐式行为，且模型下次仍会在原路径找文件。

## Consequences

- **放弃**：模型在 `write_file` 时把散件写进仓库根，不会被运行时弹卡
  拦下。改由 J1（提示词约束）+ J3（CI 门禁，在 `git add` 那一刻失败）
  覆盖。J3 只看**已跟踪**文件——但这正是问题成立的时刻：垃圾文件必须
  被 `git add` 才会成为仓库问题，而那正是应该失败的时候。
- **保留**：`resource_guard` 的 2026-09-26 定策不动，其三个钉死该行为的
  测试继续有效。写文件保持零摩擦，删除类高危仍走资源门。
- **注意**：J 提案设想的散件类型（`*.log` / `/*.txt` / `/*.py`）**已被
  `.gitignore` 覆盖**（`*.log` 全局，121-122 行的 `/*.txt`、`/*.py` 仅
  根目录），根本进不了索引。所以 J2 想拦的东西在多数情况下并不存在，
  这也是不实施它的补充理由。

## 证据

```
crates/hermix-agent/src/local_agent_service.rs:1120
  // 文件内容写入（write_file / patch / move_file）不做资源门拦截。
  // 2026-09-26 定策："写文件不用审批，只有删除等高危才审批"。

.gitignore:10   *.log
.gitignore:121  /*.txt
.gitignore:122  /*.py
```
