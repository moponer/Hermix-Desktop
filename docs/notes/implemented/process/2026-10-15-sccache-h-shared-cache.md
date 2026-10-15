# Decision Note: sccache 编译缓存固定 H 盘共享目录（server 模式 + 关闭增量）

Status: implemented — 已落地：`.cargo/config.toml` 指向 H:、`Cargo.toml` 关增量、`scripts/start-sccache.ps1` 拉起 server、开机任务 `HermixSccacheServer` 已建

## Decision

- sccache 作为 `rustc-wrapper` 在 `.cargo/config.toml` 生效，每次 rustc 调用先过 sccache，命中即复用产物。
- 编译缓存落在 `H:\sccache-cache`（共享盘，多人 / 多 worktree 复用同一套依赖编译产物，根治 target 滚雪球）。
- **必须运行 sccache server**：sccache 0.18.0 的 in-process 模式忽略 `SCCACHE_DIR`，只有 server 才读该变量；server 未运行时构建静默回退到 C 盘默认 `AppData\Local\Mozilla\sccache`，缓存等于没开。server 由 `scripts/start-sccache.ps1` 显式设 `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` 后拉起；已建 Windows 计划任务 `HermixSccacheServer`（ONSTART / SYSTEM）在开机时自动起 server，无需手动。
- **`[profile.dev]` 与 `[profile.test]` 的 `incremental` 必须为 `false`**：sccache 对带 `-C incremental` 的编译一律判 `non-cacheable`，开着则命中率 0%、无任何报错——看起来正常实则缓存废掉。在 `Cargo.toml` profile 直接置 false（cargo `[env] CARGO_INCREMENTAL=0` 只注入子进程、不影响 cargo 自身对 incremental 的决策，故无效）。
- 缓存上限 `SCCACHE_CACHE_SIZE=50G`（依赖树 ~30GB，默认 10GiB 会频繁驱逐）。

## Alternatives considered

- **仅靠 `.cargo/config.toml [env] SCCACHE_DIR` 指向 H/F 盘**：in-process 模式忽略该变量，实测缓存仍落 C 盘 Mozilla 路径，无效。放弃。
- **sccache.toml 配置文件**（`%APPDATA%\sccache\sccache.toml` 与 `~/.config/sccache/sccache.toml`）：实测 0.18.0 不读这两个路径，缓存仍落 C 盘。放弃。
- **`[env] CARGO_INCREMENTAL = "0"`**：cargo 的 `[env]` 只注入子进程（build script / rustc）环境，不改变 cargo 自身对 incremental 的决策；实测仍 non-cacheable。放弃，改在 `Cargo.toml` profile 直接设 `incremental = false`。
- **`[env] SCCACHE_START_SERVER = "1"`**：该变量会被传给每次 rustc-wrapper 调用，sccache 作为 wrapper 收到后报 `can't be used with other commands` 而中断编译。放弃；改为独立脚本拉起 server。
- **系统级 `CARGO_HOME/config.toml` 放 wrapper / `SCCACHE_DIR`**：机器级，会作用于本机所有 Rust 项目，且 server 仍须运行；不如项目级 `.cargo/config.toml` 干净、可随仓库走。放弃。
- **sccache redis / S3 后端（团队严格共享更稳妥）**：用户明确要求 H 盘目录，且当前是单机多 worktree 场景；目录共享已满足，留作未来扩展。放弃（暂）。

## Consequences

- **重启 / server 掉 = 静默失效**：server 不在则回退 C 盘、缓存命中率骤降且无任何报错；开机任务兜底，但清理过 server 进程后须手动 `scripts/start-sccache.ps1` 一次。
- **禁止把 `[profile.*] incremental` 改回 `true`**：唯一会让 sccache 静默失效的改法，无报错，排查极难。
- **target 不再只增不减**：依赖编译产物常驻 H 盘缓存，可安全 `cargo clean` 后秒级重建；`target/debug/incremental` 因 incremental 关闭已成死重可删，`target/release` 仅打包用可删（需要时从缓存重建）。
- **多 writer 并发**：若 H 盘为网络盘且多人同时写同一 `sccache-cache`，sccache 本地缓存非严格并发安全；当前单机多 worktree 可接受，团队严格共享建议换 redis/S3。
- **首次全量编译成本仍一次性存在**：改 profile 后首次 `cargo check --workspace` 重写 H 盘缓存（~40 对象），之后重建命中。
