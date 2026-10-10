# Decision Note: sidecar 新鲜度——把复制交给编译器，把守门人搬到出包路上

Status: implemented — build.rs 编译期复制已验证自愈；出包前新鲜度断言已就位

## Decision

sidecar 的"新鲜"不再由**流程记忆**保证，改由两处结构性约束保证：

1. **复制交给编译器。** `build.rs` 对 `usn-indexer` 从"存在性断言"升级为**编译期
   从 `target/release/` 复制**，与 `hermix` 对齐。此后 `binaries/` 与
   `target/release/` 不可能分叉——只要壳编译发生过。
2. **守门人搬到出包路上。** 新鲜度断言内联进 `release.sh` 组装 app 根之前，
   出包必经，`--skip-build` 同样照跑，不满足即 `exit 1`。

配套：**出包不再经过 `binaries/` 这一层副本**——`hermix` / `usn-indexer` /
`hermix-viewer` 直接取 `target/release/`（cargo 刚编出来的那个文件）。
`officecli` / `markitdown` 仍取 `binaries/`，因为它们是项目资产/上游预编译，
仓库里没有 crate、没有构建脚本。

## 根因：同一类事故反复出现，不是因为没修好

出包是一条**复制链**，每一环都能独立落后于上一环。要保证的不变式只有一条：
**出包用的文件 == 刚才编出来的那个文件**。这条不变式只在"复制由编译器做"时
自动成立。原来的结构有三处让这条不变式失效：

| # | 缺陷 | 后果 |
|---|---|---|
| 1 | `build.rs` 对 usn-indexer 只做**存在性断言**，复制交给脚本 | 忘了跑脚本就永远是旧的，而校验因"文件在"而放行 |
| 2 | `check-sidecar-freshness.sh` 只被 `ci-gate.sh` 调用，`release.sh` 从不经过；且 `ci-gate.sh:329` 是 `if [ -f ... ] else SKIP` | 守门人不在出包路上；守卫文件删掉就能关掉守卫 |
| 3 | `release.sh:88` 找 `$BINARIES/$s.exe`，实际文件名带 `-x86_64-pc-windows-msvc` 后缀 | 判断恒为假，静默落到 `target/release/`——与"权威来源 = binaries/"的注释正好相反 |

缺陷 1 的成因值得单独记：`build.rs:52-55` 给出的理由是"usn-indexer 是壳包自己的
`[[bin]]`，build.rs 与它同包同时编译，此刻产物尚不存在"。**这个前提在 2026-10-09
usn-indexer 拆成独立 crate 之后就不成立了**——它有自己的 package，壳包并不依赖它，
`ensure-sidecar` 先于壳编译把它编好。注释没跟着改，约束也就没跟上。

同期的第三个同类实例：安装器 `find_main_exe` 的候选第一位曾是 `hermix.exe`，
那是 sidecar 服务端（无 UI 后台进程），而 Windows 文件系统大小写不敏感，
连 `Hermix.exe` 都会误配——误启动 sidecar = 用户看到"点了运行却什么都没发生"。
**三个实例的共同形状：靠名字猜路径，猜错了不报错，只会更安静地做错事。**

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 继续加检查（如给 sidecar 打版本戳、比对哈希白名单） | 每一轮事故都加一环，链条单调增长，能漏的点越来越多。检查只能发现已经发生的分叉，消不掉产生分叉的那个环节。 |
| 让 `release.sh` 继续以 `binaries/` 为准（只修 `:88` 的文件名） | 修的是"猜错"，没修"存在两份可独立弄旧的副本"。两份分叉时仍要看谁更自觉。 |
| `cargo clean` 后统一重建、禁止增量出包 | 治不了并发构建与 `--skip-build` 这两条真实的分叉路径。 |
| 把 usn-indexer 也并回壳包的 `[[bin]]` | 会重新引入 2026-10-09 那次拆分的 bootstrap 死锁（build.rs 校验 binaries/ 里尚未编出的 usn-indexer）。 |
| 给 officecli / markitdown 也做新鲜度判定 | 仓库里没有它们的源码，拿本仓库的 `.rs` 去比毫无意义——每次提交都误报，几周内这道门就没人当真了。 |

## Consequences

- **出包路径少一个可分叉点**：`hermix` / `usn-indexer` / `hermix-viewer` 直接取
  `target/release/`。"两份里哪份更新鲜"这个问题在出包链路上不再存在。
- `binaries/` **没有删除**，但角色变了：它不再是出包路径上的中转站，而是
  `cargo tauri build`（tauri.conf externalBin）的落点 + 两个预编译资产的家。
- 新鲜度断言 **fail-closed**：`--skip-build` 也不再能把陈旧产物带进包里。
- 断言分三档扫描范围（`crates` 去掉 hermix-indexer / `crates/hermix-indexer` /
  `apps/viewer`）。`hermix` **不**纳入 `src-tauri/src`——那是壳的代码、不进
  hermix-server，拿它判定等于每次改壳都误报一次。这是实现过程中被 PowerShell
  等价复现抓出来的，照抄 `check-sidecar-freshness.sh` 的扫描范围会引入这个误报。
- `build.rs` 的复制**只在它真正重跑时生效**：cargo 按指纹去重，binaries/ 被破坏
  而 build.rs 与其 `rerun-if-changed` 源都没变时不会重跑。所以这是纵深防御，
  不是"任何时候都新鲜"——后者由出包前断言负责。
- `check-sidecar-freshness.sh` 与 ci-gate.sh 里的那道门**保留不动**：它作为开发期
  反馈仍有价值（改完源码立刻知道该重编），只是不再独自承担出包前的拦截职责。

## 验证

- **build.rs 自愈**：把 `binaries/usn-indexer-x86_64-pc-windows-msvc.exe` 故意写成
  64 字节垃圾，`cargo build --release -p hermix-desktop`（exit 0, 6m12s）后
  恢复为 229,376 字节 / sha256 `376EA4572E3EFCCB`，与 `target/release/` 一致。
- **新鲜度判定**（本机无 bash，用 PowerShell 等价复现 `find -newer -quit` 语义）：
  - 三个真实产物 → 均 fresh（放行）；
  - 把探针时间戳改旧 2 小时再查 usn-indexer → STALE，命中
    `crates/hermix-indexer/src/main.rs`；
  - 同一探针查 hermix → fresh，确认 indexer 的改动不会误伤 hermix（prune 生效）。
- **未执行**：`scripts/release.sh` 是 bash 脚本，本机只有 WSL stub、无 Git Bash，
  **未实际运行**。语法逐行人工核对（`case` 三分支均赋值 `stale` 满足 `set -u`；
  用 `find -quit` 而非 `| head` 避免 `pipefail` 下 SIGPIPE 误杀；`BINARIES` 仍被
  officecli/markitdown 使用）。需在有 bash 的环境跑一次 `scripts/release.sh`
  与 `./scripts/check-sidecar-freshness.sh` 补验。