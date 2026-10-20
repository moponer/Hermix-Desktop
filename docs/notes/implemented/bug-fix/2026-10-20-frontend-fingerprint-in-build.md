# Decision Note: 前端内容指纹纳入编译指纹 —— 根治「exe 里是旧 UI」

Status: implemented — viewer 与 desktop 两端的 build.rs 都把前端产物的内容哈希写进编译指纹（OUT_DIR 的 .rs + `cargo:rustc-env`），dev 链路补上查看器构建；实测「build → test → 复查 exe」三步后 exe 仍是新的

## Decision

**前端产物（`frontendDist`）的内容哈希必须进入编译指纹，否则 cargo 会把「嵌着旧 UI 的 rlib」当新鲜的继续用。**

根因是三件事叠在一起，缺一不可：

1. **前端在宏展开期被读进二进制**。Tauri 的 `generate_context!` 宏把 `frontendDist` 整个读进来内嵌到可执行文件。读文件的动作发生在 rustc 的宏展开阶段。
2. **sccache 的缓存键不含宏展开期读取的文件**。sccache 对 rustc 调用做键控时用的是「crate 源码 + 编译参数 + 环境」，**不包含**宏展开期才去读的外部文件。
3. **`tauri-build` 只发了 `rerun-if-changed=<dist>`**。它让 build script 在 dist 变化时重跑，但 build script 的**输出没变**，于是 crate 的指纹没变 → cargo 判定 crate 仍是新鲜的 → 直接复用旧 rlib → 链接出的 exe 里是旧界面。

第 3 点是关键：单看第 2 点像是「sccache 的问题」，但真正让缺陷落地的是**没有任何随前端内容变化的信号进入指纹**。`rerun-if-changed` 只管「要不要重跑 build script」，不管「crate 要不要重编」。

修法（两端同款）：build.rs 算出 dist 下所有文件的内容哈希，同时写进两处——

- `OUT_DIR/dist_fingerprint.rs`（或 `frontend_fingerprint.rs`），由 lib.rs 用 `include!` 引进来。**源码变了 → cargo 必然重编**，与 sccache 无关。
- `cargo:rustc-env=<NAME>=<hash>`。**环境变了 → sccache 必然 miss** → rustc 真跑。

两条一起上不是为了保险，而是因为它们各自防的是不同的一环：前者管 cargo 的新鲜度判定，后者管 sccache 的缓存键。任何一条单独失效，另一条仍在。

**dev 链路同步补齐**：`apps/desktop-tauri/scripts/dev.mjs` 此前只构建 `hermix` sidecar，**从不构建 viewer**——`target/debug/hermix-viewer.exe` 停在 cargo 随手留下的旧产物，主程序按「同目录」找它，找到的就是几周前的界面，且没有任何提示。现在 dev 分支会先 `vite build` 再 `cargo build -p hermix-viewer`（产物正好落在主程序查找的 `target/debug/`，不需要拷贝），并在构建前结束正在运行的同名进程（Windows 文件锁会让 cargo 覆盖失败，旧二进制原地留下）。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 打包前断言 exe 比 dist 新（`check-sidecar-freshness.sh` 那样） | 检查只能发现**已经发生**的分叉，消不掉产生分叉的那个环节。2026-10-19 的「10-14 的 exe 配 10-16 的 UI」就是这么漏过去的。 |
| 每次构建前 `cargo clean -p <crate>` | 治不了并发构建；且把「前端变了要重编」这件事从编译器的判断转成人的记忆。 |
| 关掉 sccache（`SCCACHE_DISABLE=1`） | 能绕过，但只是把「缓存键缺一块」换成「每次全量重编」。全仓的编译时间代价换一处缺陷，不划算；而且真机上 sccache 是共享的（见 `2026-10-15-sccache-h-shared-cache`），全局关掉会拖累所有人。 |
| 把前端产物复制进 Rust 源码树（`include_bytes!` 逐文件） | 要为每个资源文件写一条 `include!`，前端一改就是几百条，且路径一多就脆。 |
| 让 build.rs 直接把 dist 拷进 `OUT_DIR` 并在宏里引 | 资源路径要重写，且 `generate_context!` 的资源清单是宏自己生成的，绕不过去。 |
| 只修 viewer，不修 desktop | 两端是同一个根因、同一个 `generate_context!` 机制。desktop 的 `frontendDist` 是 `release/frontend`，同样会被 sccache 捡回旧产物——只修一半等于下一次 `npm run build` 照样发旧界面。 |

## Consequences

- **得到**：前端改了必然重编、必然重链。2026-10-19 记的那类「装好的包里有旧界面」从此在构造上不成立。
- **顺带查明一个既有阻断**：`tauri-build 2.7.1` 的 `copy_binaries`（`src/lib.rs:162-166`）把 sidecar 拷到 **target 目录根**（`target/debug/hermix.exe`），拷之前先 `fs::remove_file(&dest).unwrap()`。**主程序正在运行时，那个文件就是运行中的 sidecar 本体** → Windows 文件锁 → `PermissionDenied` → **build script panic**。症状是 `cargo check -p hermix-desktop` 报「failed to run custom build command」，与本改动无关，是既有的 dev 期阻断：重建 desktop 前必须先关掉主程序。viewer 侧有同一个形状的坑（运行中的 `hermix-viewer.exe` 锁住自己），`dev.mjs` 因此在构建前结束同名进程。
- **代价**：前端一变，对应 crate 就要重编 + 重链。desktop 的 debug 构建本来就要 4~8 分钟，这一步在它的链路上是净增；但相比「静默发出旧界面」，这是必须付的代价——而且它把一次**静默错误**换成了一次**可预期的耗时**。
- **两端各有一份几乎相同的 build.rs 片段**（约 30 行）。没有抽公共 crate：build script 之间无法共享代码，跨 crate 抽一个只为 30 行的辅助函数不划算。
- **`include!` 的常量是 `pub` 的**：否则会触发 `dead_code` 警告（它是 crate root 的 `pub const`，不触发）。
- **dev 启动会多花约 20 秒**（viewer 的 vite build）。`dev.mjs` 在 viewer 依赖未安装时跳过并告警，不阻断启动。
- **`dev.mjs` 会结束正在运行的 `hermix-viewer`**。这是替换同名 exe 的前提；它是一个开发期产物，替换它不丢用户数据（图片编辑器的未导出改动会丢，这是 dev 工具的正常代价）。

## 验证

复现（修复前的真实序列，本机实测）：

```
cargo build -p hermix-viewer   →  deps/hermix_viewer.exe 27,264 KB（新的）
cargo test  -p hermix-viewer   →  target/debug/hermix-viewer.exe 退回 13,012 KB（10-15 旧产物）
```

修复后同一序列：

```
实验前:  13,012 KB  10-15 11:48:25  新版标记=NO(旧)
build后: 27,264 KB  10-20 11:03:35  新版标记=YES
test后:  27,264 KB  10-20 11:03:35  新版标记=YES
```

「新版标记」= exe 字节里能否扫到本次任务新增的串（`image_write_composite` / `media-src 'self' asset:`）；旧产物这两串都是 0 命中。
