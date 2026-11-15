---

name: workflow-verification
version: 1
description: >
  Evidence-based completion: do not claim "done" until something outside your
  own reasoning proves it. Use before finishing any task that changed code,
  config, data, or files — especially before saying "fixed", "done",
  "implemented", "all tests pass", or "should work now". Turns vague claims into
  rerunnable evidence (command output, test names, file diffs, screenshots) and
  states explicitly what was NOT verified. Also use when the user asks "is it
  really done", "did you test it", or when a previous "fixed" turned out to be
  wrong. Do NOT use to pad trivial answers with ceremony.
license: MIT
tags:
  - verification
  - evidence
  - completion
  - workflow
---

# Workflow: Verification Before Completion

Plans, checklists, and confident prose are **not** evidence. A conclusion is
earned by an artifact someone else could re-run.

## Evidence ladder (weak → strong)

1. "The code looks right." — worth nothing.
2. "It compiles." — proves syntax.
3. "The unit test that encodes the requirement passes." — good.
4. "The failing reproduction now passes, and its regression test is in the
   suite." — strong.
5. "End to end: real command / real file / real HTTP call produced the expected
   output, shown below." — strongest.

Match the level to the claim: a UI claim needs a rendering/screenshot or a DOM
probe; a performance claim needs numbers; a "no regressions" claim needs the
full suite result for the affected crates, not one green test.

## 完成判据：中间量不是结果量

一项工作进"已完成"，凭的是**验收条件对应的外部事实**。下面三种常见"看起来完成了"
的东西都不是事实，2026-10-30 的一轮真实交付里三条同时出现过：

| 中间量 | 为什么不算 | 结果量长什么样 |
|---|---|---|
| 播放器 `ended=true` / 进度条到头 | 只是客户端播放状态，平台是否入账是另一回事 | 服务端进度 N/N、任务点状态、结果页字段 |
| "脚本已修好" / "bug 已改" / "矩阵已就绪" | 改动 ≠ 验证；没跑的脚本等于没修 | 同一个脚本跑通后的输出行 |
| 一次成功 / 单个用例通过 | 可能只是这一次的偶然状态 | 可重复：同配置重跑仍成立，或说清它为什么可能不复现 |

配套三条纪律：

- **部分成果不得混进"已完成"**：只要该项还有"未回查 / 未重跑 / 未跑完"的字样，整条记为
  `blocked` 或 `pending`，并且和已完成项**物理分开**（不同标题、不同表），不靠一句
  "不粉饰成已完成"来免责。
- **外部阻断不是完成**：工具被禁用、权限不足、环境缺失可以正当停止，但阻断前就未验证的
  项，不能因为阻断而被重新归类成完成。
- **计划不是交付**：收尾里出现"下一步：…"，说明本轮目标未达成；此时正确的状态是
  pending/blocked，而不是带计划的总结。

## Checklist before saying "done"

- [ ] The exact command(s) you ran are listed, with their result lines.
- [ ] For each requirement in the request: which check proves it? (If a
      requirement has no check, either add one or say "not verified".)
- [ ] Warnings: `cargo check -p <crate> --all-targets` shows none new.
- [ ] Tests: `cargo test -p <crate> --lib` green; name the new/changed tests.
- [ ] Known limits stated: env not available, platform not tested, path not
      exercised, feature flag off.
- [ ] Anything you changed "to be safe" is listed as an unverified change.

## Honesty rules

- Never say "all tests pass" if you ran a filtered subset — say the filter.
- Never say "fixed" for something you only compiled.
- If a failure is environmental (missing permission, no runtime), say so with
  the exact error and what would need to change — do not quietly skip it.
- If you stopped early (context/time), say what remains, with the next command
  to run. A precise "not done, here's where" beats a vague "done".

## In this repo

- Sidecar freshness: `./scripts/check-sidecar-freshness.sh` before packaging
  claims. Stale sidecars have caused real delivery incidents.
- Config changes: the value must be reachable from `~/.hermix/config.yaml` — a
  serde field nobody bridges is not a config feature.
- Tool behaviour: a schema/description change is part of the API; check the
  bridge (`bridge_tool_registry`) exposes it.

## Output shape

```
改了什么: <one line per change>
验证: <command> → <result line>
未验证: <explicit list or "none">
剩余风险: <one line or "none">
```
