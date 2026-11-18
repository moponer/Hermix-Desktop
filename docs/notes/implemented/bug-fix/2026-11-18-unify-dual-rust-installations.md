# Decision Note: 根治「两套 Rust」——统一到 rustup 的 1.92 工具链

Status: implemented — PATH 已清理、1.92 完整工具链（含 rust-src）由 rustup 管理、`rust-toolchain.toml` 固定版本

## Decision

本机此前同时存在两套 Rust，这是 rust-src 缺失与 LSP 标准库跳转失效的根源：

| | 路径 | 来源 | rust-src |
|---|---|---|---|
| ① | `C:\Program Files\Rust stable MSVC 1.92` | MSI 独立安装 | **无** |
| ② | `~/.rustup/toolchains/...` | rustup 管理 | 有（但版本是 1.95） |

**机器级 PATH** 把 ① 放在最前，所以 `rustc` / `cargo` / `rust-analyzer` 全部解析到 ①。
rustup 的 shims 在**用户级 PATH**，排在后面，永远不生效。

`rust-toolchain.toml` 对此**无效**——它不是「选择工具链」，而是「让 rustup 的 shim 去选」；
PATH 上的 `rustc.exe` 是真实可执行文件，直接运行，根本不经过 rustup。

### 修复（四步）

1. **装完整的 1.92 工具链**：`rustup toolchain install 1.92.0 --component rust-src --component rust-analyzer --component clippy --component rustfmt`。
   选 1.92 而不是 1.95，是为了**版本与项目原有版本一致**，零迁移风险。
2. **`rustup default 1.92.0`**：让 shim 指向它。
3. **从机器级 PATH 移除 ① 的 `bin` 目录**（需管理员，用 `Start-Process -Verb RunAs` 提权完成）。
   这一步是关键——不移除，`rust-toolchain.toml` 和 `rustup default` 都是摆设。
4. **加 `rust-toolchain.toml`**：把版本和所需组件写进仓库，任何人 clone 下来都自动对齐。

### 验证

```
rustc           -> ~/.cargo/bin/rustc.exe (shim)
rustc --version -> 1.92.0
sysroot         -> ~/.rustup/toolchains/1.92.0-x86_64-pc-windows-msvc
src present     -> True
rust-analyzer   -> 1.92.0
```

标准库跳转实测（1.92 + 它自己的 rust-src）：

```
Vec      -> library/alloc/src/vec/mod.rs:434
Vec::new -> library/alloc/src/vec/mod.rs:459
len      -> library/alloc/src/vec/mod.rs:2911
hover    -> pub struct Vec<T, A = Global> { buf: RawVec<T, A>, len: usize }
```

### 顺手修掉的两个历史问题

**`target` 目录 ACL 损坏**：`<repo>/target` 的 ACL 里授权的是三个陌生 SID，
当前用户只能靠 `Authenticated Users` 的 Modify 权限工作，缺 `DeleteSubdirectoriesAndFiles`，
导致 cargo 创建/删除临时目录时报 `os error 5 拒绝访问`。用 `takeown /R` + `icacls /grant /T`
处理了 539 个文件后，旧 target 可正常删除重建。

**`1.92.0` rustup 工具链是残缺的**：此前某次安装中断留下只有 `cargo.exe` 没有 `rustc.exe` 的空壳，
`--force` 重装后完整。

## Alternatives considered

- **统一到 1.95**：rustup 那套 1.95 本来就完整，改动更小。但 1.95 与项目当前 1.92 有 3 个版本差，
  可能存在行为差异；而且当时 1.95 构建在本机因 target ACL 问题验证不了。装 1.92 的代价只是多下一次 1.2 GB。
- **卸载独立安装的 1.92，全用 rustup**：这是最干净的终态，但 `msiexec /x` 报 **1612**
  （原始 .msi 缓存缺失，Windows 找不到卸载源），需要用户手动「应用和功能」里修复/卸载。
  **当前状态已足够**：PATH 里没有它，所有工具走 shim，残留目录不影响任何功能，只是占磁盘。
- **改 `RUST_SRC_PATH`**：实测对 rust-analyzer 无效（新版不再用它定位 sysroot）。
- **给 `lsp.rs` 加特殊逻辑挑工具链**：上一轮已经加过（`rustup_toolchain_bin`），
  但那是**治标**——它只让 LSP 走对的那套，`cargo build` 仍然用错的。根治必须动 PATH。

## Consequences

- 现在只有一套生效的 Rust：rustup 的 1.92。`rust-toolchain.toml` 保证 clone 后自动对齐。
- **`lsp.rs` 里那段挑工具链的代码现在是冗余的**（因为 PATH 已统一）。保留它作为兜底：
  在没有 rustup 的机器上会返回 `None`，行为退回默认。
- **`C:\Program Files\Rust stable MSVC 1.92` 目录仍在**（49326 个文件，约 1.2 GB），
  MSI 卸载失败（错误 1612）。它不在 PATH 里，不影响功能。要彻底清理需在
  「设置 → 应用 → Rust 1.92」里修复安装后再卸载，或手动删除目录后清理注册表项。
- 之前设的 `RUST_SRC_PATH` 用户环境变量现在完全多余，可删（留着无害）。
- **需要重启终端**（或重新登录）才能让新 PATH 对已打开的窗口生效。当前会话是通过重建 PATH 模拟的。
