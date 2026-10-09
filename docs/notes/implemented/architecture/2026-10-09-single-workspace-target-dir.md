# Decision Note: 单工作区 + 单一产物目录（根治旧二进制问题）

Status: implemented — 五批次落地（合并 workspace / build.rs 分级容错 / 解析收敛 / ensure-sidecar 自动化 / 文档收尾），`cargo check --workspace --all-targets` 与 `npm run ensure-sidecar` 全链路验证通过

## Decision

全仓**单 workspace、单一产物目录**：`apps/desktop-tauri/src-tauri` 并入根 workspace，产物一律落仓库根 `target/`，废除 `release/target/` 与 `src-tauri/target/` 两处历史布局；sidecar 刷新链自动化（`ensure-sidecar.mjs`），删除一切手工 copy 与静默跳过逃生口。

### 根因

- `src-tauri/Cargo.toml` 的空 `[workspace]` 表使其成为独立 sub-workspace，产物分裂三处共 5 份 hermix.exe；
- `sidecar.rs::hermix_bin_path()` first-exists 候选永远命中 `release/target/debug/` 的陈旧副本；
- `cargo tauri build` 不重编 sidecar，`binaries/` 靠手工 copy（cmd 版 `_:ensure-sidecar`），绕过 npm 直接 `cargo tauri build` 即拿陈旧二进制打包——2026-10-06 与 2026-10-08 两次「修了但新装的包里没有」交付事故的直接入口。

### 实施（五批提交）

1. **合并 workspace**：根 members 追加壳包；删空 `[workspace]` 表与 `.cargo/config.toml`；根 Cargo.toml 的 2026-06-20 target-dir 注释墙（描述与现状矛盾）精简，真相源移到本笔记。
2. **build.rs**：源路径固定仓库根 `target/<profile>/`；缺失/拷贝失败 release 下 panic、debug 下 warning；删 `HERMIX_SKIP_SIDECAR_COPY` 逃生口。
3. **usn-indexer 拆独立 crate `crates/hermix-indexer`**（bin 名不变）：原先是壳包 `[[bin]]`，build.rs 的 externalBin 校验要求 binaries/ 里已有它、而它又由该包编译产出——fresh clone 首次打包陷入 bootstrap 死锁。拆出后（无 tauri-build）由 ensure-sidecar 独立编译，死锁消除。
4. **`ensure-sidecar.mjs`**（Node，非 bash——beforeBuildCommand 在 Windows 走 cmd，且 dev.mjs 注释记录过逃离 cmd 内联脚本的原因）：编译 hermix-server + hermix-indexer → copy 到 binaries/ → sha256 逐字节校验，失败 exit 1。挂到 `tauri.conf.json beforeBuildCommand`（防绕过 dev.mjs 的路径）+ `npm run build`。
5. **解析收敛**：`sidecar.rs::hermix_bin_path()` dev 候选只剩 `target/{debug,release}`，first-exists 改 max-by-mtime（locate_helper 同款教训：first-exists 永远选中残留旧候选），全缺失返回 Err；`commands_local_search.rs::locate_helper` 候选同步收敛。
6. **dev.mjs**：删除 `CARGO_TARGET_DIR=release/target` env 覆盖（它是产物分裂的另一个源头）。

### 实施中新发现并修复的问题

**schemars 0.8.22 + indexmap 1.9.3 编译炸（E0107）**：合并后全图 feature 统一，tauri-build 的 build-dep schemars 开 `preserve_order` 拉起 indexmap 1.9.3；其 build.rs 靠 autocfg 运行时探测 emit `has_std`，本机探测静默失败 → 走 no_std 分支（`IndexMap<K, V, S>` 三泛型无默认）→ schemars 两参调用 E0107。修法：**build-dependencies feature 垫片**（`indexmap1 = { package = "indexmap", version = "1.9", features = ["std"] }`）——resolver v2 下 build-dep 图与普通依赖图 feature 不互通，垫片必须放 `[build-dependencies]` 才统一进 schemars 编译单元；std 显式激活后 build.rs 走 `CARGO_FEATURE_STD` 快速路径无条件 emit，跳过探测。

## Alternatives

- **保留双 workspace + 自动化 copy**：只是把手工环节变自动，三产物目录与解析歧义仍在，下次换个入口（新脚本、IDE 集成）还会绕过。放弃。
- **统一 `CARGO_TARGET_DIR` 环境变量**：dev.mjs 曾以此「统一」到 release/target——环境变量只在经由 dev.mjs 的路径生效，裸 cargo / tauri CLI 各自为政，且相对路径按 cwd 解析的历史坑（src-tauri 上溯 4 级跑仓库外）正是 2026-08-10 笔记废弃它的原因。放弃。
- **build.rs 拷贝 usn-indexer**：时机悖论（build.rs 与该 bin 同包同时编译，产物此刻不存在），不可行——改为拆独立 crate。

## Consequences

- **全 workspace 编译量增加**：`cargo check --workspace --all-targets` 现包含壳包（tauri 全家桶，首次 check +4 分钟）；日常验证习惯不变（`cargo check -p hermix-server`），全仓 check 不要当例行。
- **tauri 依赖树进根 Cargo.lock**：锁文件体积增大；sccache 从此覆盖壳包编译。
- **壳包继承根 workspace profile/lints**：release 走 lto=thin + codegen-units=1 + strip（包体↓、编译时间↑）；`unwrap_used=warn` 等 lint 开始作用于壳代码，出现 warning 属预期。
- **hx-shell 保持独立工程**（`apps/hx-shell` 自带 workspace，注释明示有意隔离），其脚本与产物目录不在本次收敛范围。
- **验收口径**：合并前后 `cargo tree -i windows / windows-core / webview2-com` 输出一致（不引入新重复版本，而非消灭既有的 windows-core 0.58/0.62 双版本——tauri 2.12.1 与 hermix-cua 各需其 minor，`src-tauri/Cargo.toml` 注释钉了这条配对）。
