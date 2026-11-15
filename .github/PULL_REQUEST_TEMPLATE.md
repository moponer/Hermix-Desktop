# Pull Request

> **必填段**：pre-existing cleanup — 防止"今天修 13+ 个 pre-existing"变成常态化

## 改动概要
<!-- 一句话说明本次 PR 改了什么 -->

## pre-existing cleanup
<!-- 必填：本次 PR 是否附带清理了 pre-existing bug？
     填 "无" / "清除了 X 个 pre-existing" / "扫过但没发现"。 -->

- [ ] 本次改动**没有**遗留 pre-existing
- [ ] 本次改动**清理**了 N 个 pre-existing: <列出>
- [ ] 已跑 `./scripts/ci-gate.sh` 全绿

## 测试证据
<!-- 必填：贴 cargo test / pnpm tsc 输出 -->

```
cargo check --workspace --all-targets   →  OK
cargo test --workspace --lib            →  <N> passed; 0 failed
pnpm tsc --noEmit                       →  0 errors
```

## 关联任务
<!-- 关联 PROGRESS_TRACKER.md §七 下一步里的某项 -->

Closes #<issue> | Task #<id>

## 禁区自检
主人哲学是"做好自己,不要去感知外部"。本 PR 不应包含：

- [ ] 没有引入多平台分发入口(微信/Discord/飞书/CLI TUI/17 平台 gateway)
- [ ] 没有引入外部资源感知(SSH/Git/桌面扫描/其他 AI Agent 宿主发现)
- [ ] 没有跨平台编译目标(macOS/Linux)— 仅 Windows MSVC
- [ ] 没有引入 / 修改 / 删除 主人工作目录下的任何文件(`~/Desktop`、`~/Downloads`、`~/Documents` 等)

## 复审 checklist
- [ ] 编译干净(`ci-gate.sh` 跑过)
- [ ] 新增代码有单元测试
- [ ] 修改了 API 的话 `MEMORY.md` 同步更新
- [ ] 修改了用户可见行为的话 `PROGRESS_TRACKER.md` 同步更新
