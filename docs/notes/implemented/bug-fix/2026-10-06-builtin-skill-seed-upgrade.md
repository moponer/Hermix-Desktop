# Decision Note: 内置技能 seed 升级机制（hash 边车三方比对）

Status: implemented — `seed_one` 已落地于 `crates/hermix-server/src/dashboard/skills.rs`，seed 附带 `.seed-meta.json` hash 边车；新内置技能 `design/impeccable-design` 同批加入。

## Decision

内置技能的 seed 规则从「`SKILL.md` 已存在即一律跳过」升级为**三方比对**：

1. 每次成功 seed 时，在技能目录写一个边车文件 `.seed-meta.json`，记录所写内容的 SHA-256（hash 复用 crate 内既有 `sha2` 依赖，无新依赖）。
2. 下次启动 seed 时重算盘上文件 hash 并与边车比对：
   - **一致**（用户从未改过）→ 内置内容有更新就**原地覆盖**（`SeedOutcome::Updated`）；内容相同则不动（`AlreadyCurrent`）。
   - **不一致或无边车且内容与内置不同**（用户改过）→ **保留用户文件**，新版内置写到旁边 `SKILL.md.new` 供参考（`PreservedUserEdit`）；无边车但内容与内置逐字节相同（旧版安装）→ 记录 hash 收编（`AlreadyCurrent`）。
   - **无 `SKILL.md`** → 首装/用户删除后恢复，直接写入（`Fresh`）。
3. `seed_builtin_skills` 返回值语义改为「写入或刷新的文件数」，调用方（`state.rs` 两处、`seed_builtin_skills_default`）签名不变，无需改动。

同批注册第 19 个内置技能 `design/impeccable-design`（界面/平面/PPT 设计规范 + `npx impeccable@4.1.0 detect` 反模式检测收尾），description 含自动激活触发词，由 `skill_orchestrator` 按 description 匹配自动注入，无额外代码路径。

## Alternatives considered

- **维持「存在即跳过」**（旧方案）：用户手改绝对安全，但内置技能永远停在首次安装的版本——修 bug、改内容到不了存量机器，新旧机器行为分裂。被本决策取代的就是它。
- **版本号字段比对**（frontmatter 加 `version:`，升级时覆盖旧版本号以下的内容）：需要解析并维护每份技能的版本号，格式约束强、易漏改；hash 对比无格式要求，任何字节变化都能被感知。
- **只写不删 + 全量覆盖**：直接覆盖会毁掉用户手改（现有 dashboard_smoke 已钉住这条契约）；只写不删就是旧方案。
- **把 impeccable CLI 二进制打包进 Tauri resources**：离线可用，但增加打包体积与平台耦合；`npx -y impeccable@4.1.0` 锁定版本号已可复现行为，首次联网拉取后走 npm 缓存。离线需求出现时再评估。

## Consequences

- 每个内置技能目录多一个几十字节的 `.seed-meta.json`。技能扫描器（`FileSkillStore`）按 `SKILL.md` 发现技能，边车文件对它不可见；若未来有「目录内容校验/签名」类功能，需把边车列入白名单。
- hash 比对只区分「改过/没改过」，不合并差异：用户改过的技能拿不到新版内容（只有旁路 `SKILL.md.new`），合并责任在用户。这是「绝不毁用户内容」的代价，接受。
- 交付物的完整性以测试钉住：`dashboard_smoke.rs` 的 seed 计数断言 18→19，另加 4 个 `seed_one` 行为测试（升级覆盖、保留用户改+暂存、旧版收编、删除恢复）。
- `npx` 方案依赖目标机器有 Node/npm 且首次联网；无网机器上 detect 步骤会失败，技能正文已写明该命令是「代码型产物」的收尾项，失败不阻塞设计任务本身。
