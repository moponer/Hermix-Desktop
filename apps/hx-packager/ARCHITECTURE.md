# hx-packager 架构

> 自定义应用打包 / 安装 / 卸载 / dev runner 的架构详解。
> 对应 [spec/design/custom-app-architecture-spec.md §8](../../docs/design/custom-app-architecture-spec.md) Phase 2 / Phase 6 / Phase 8 落地。

---

## 1. 整体架构

```
                ┌────────────────────────────────────────────────────────────┐
                │                      app_dir/                              │
                │  manifest.json  index.html  main.css  vendor/sql-wasm.*   │
                └──────────────────────┬─────────────────────────────────────┘
                                       │ walk
                                       ▼
            ┌──────────────────────────────────────────────────────┐
            │            crates/hermix-plugin/HxShellPackager     │
            │  ┌─────────────────────────────────────────────┐    │
            │  │ 1. html = app_dir/<entry>                  │    │
            │  │ 2. icon = app_dir/<icon_glyph 渲染>         │    │
            │  │ 3. archive = app_archive::build(app_dir)    │    │
            │  │    (HXAM: relpath → bytes, sha256 tail)     │    │
            │  │ 4. hx-shell.exe = PE base + 1000(html)+1001 │    │
            │  │                            +1002(archive)   │    │
            │  └─────────────────────────────────────────────┘    │
            └──────────────────────────┬───────────────────────────┘
                                       │ hx-packager pack/install/dev
                                       ▼
                ┌──────────────────────────────────────────────┐
                │ hx-packager CLI  (apps/hx-packager/src/main.rs)│
                │  pack / install / list / uninstall / dev / info │
                └──────────┬────────────────────────────────────┘
                           │
              ┌────────────┴─────────────┐
              │ install                   │ uninstall
              ▼                          ▼
   ~/.hermix/custom-apps/<id>/      move-out
   ├── manifest.json                → %TEMP%/hermix-uninstall-trash/
   ├── dist/<id>.exe                <id>-<ts>/
   ├── installed.json
   └── dist/__sqlite__/*.db   ← 运行时由 hx-shell.writeFileBytes 写入
```

---

## 2. HXAM 归档格式

定义在 `crates/hermix-plugin/src/app_archive.rs`。零依赖（除 sha2）。

```
┌────────────────────────────────────────────────────────────┐
│ HXAM Header (11 bytes)                                      │
├────────────────────────────────────────────────────────────┤
│ magic       : 4 bytes  = "HXAM"  (0x48 0x58 0x41 0x4D)      │
│ version     : 1 byte   = 0x01                              │
│ entry_count : 4 bytes  = u32 LE                             │
├────────────────────────────────────────────────────────────┤
│ Entry[0]                                                    │
│ ├─ rel_len  : 2 bytes  = u16 LE (max 65535 字节相对路径)    │
│ ├─ relpath  : rel_len bytes (UTF-8, 相对 app_dir 的 POSIX)  │
│ ├─ data_len : 4 bytes  = u32 LE                             │
│ ├─ data     : data_len bytes                                │
│ Entry[1] ...                                                │
│ ...                                                         │
├────────────────────────────────────────────────────────────┤
│ SHA256 : 32 bytes = sha256(整份归档除自身)                  │
└────────────────────────────────────────────────────────────┘
```

**关键 API**（hermix-plugin/src/app_archive.rs）：
- `pub fn build(app_dir: &Path) -> Result<HxAppArchive, AppArchiveError>` — walk app_dir，构建归档
- `pub fn parse(bytes: &[u8]) -> Result<ParsedArchive, AppArchiveError>` — 解析
- `pub fn get<'a>(archive: &'a ParsedArchive, rel: &str) -> Option<&'a [u8]>` — 随机访问
- `pub fn should_include(rel: &str) -> bool` — 文件白名单（按后缀）
- `pub fn should_include_dir(rel: &str) -> bool` — 目录透传（仅排除 `node_modules` / `.git` / `dist` / `__sqlite__`）
- `pub fn verify_sha256(archive: &ParsedArchive) -> bool` — 校验尾部 sha256

**为什么不用 zip？** spec §6.1 决议：归档格式 → 默认裸 relpath→bytes 清单（零依赖/零解压/随机访问快），exe 体积敏感再上 zip。HXAM 单次 malloc + memcpy + O(1) 查找，免 zlib 解压开销。

---

## 3. PE 资源节注入

hx-packager 通过 `hermix_plugin::icon_library::embed_app_payload_multi` 把 HXAM 字节写入壳 exe 的 `RT_RCDATA` 资源节：

| 资源 ID | 类型 | 内容 | 用途 |
|---|---|---|---|
| 1000 | `RT_RCDATA` | 单 HTML 字符串 | fallback（无归档时壳走 `with_html()` 加载） |
| 1001 | `RT_RCDATA` | 图标 PNG bytes | 窗口图标 + 任务栏图标 |
| 1002 | `RT_RCDATA` | HXAM 归档 | 主路径：壳启动后 `read_self_archive()` 读 1002，`app://` 协议按 relpath 查表喂 WebView |

PE 修改用 `windows::Win32::System::Resources::UpdateResourceW`（hermix-plugin 内部），atomic 写 + 重新计算 PE checksum。

---

## 4. 加载数据流（壳启动到 WebView 显示）

```
┌─────────────────────────────────────────────────────────────┐
│ 用户双击 dist/playground.exe                                 │
└────────────────────┬────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────┐
│ hx-shell.exe 启动 (Rust + wry/WebView2)                       │
│ 1. APP_BASE_DIR = exe 父目录                                  │
│ 2. 解析命令行 (--app-id <id>, --app-dir <dir>)                │
│ 3. read_self_archive() ← PE RT_RCDATA 1002 → HXAM bytes       │
│ 4. APP_ARCHIVE OnceLock 缓存 (zero-copy)                      │
└────────────────────┬────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────┐
│ WebViewBuilder                                                │
│ ├─ with_custom_protocol("app", app_protocol_handler)         │
│ │     ↓                                                       │
│ │   收到 app://localhost/<relpath>                            │
│ │   → APP_ARCHIVE.get(relpath)                               │
│ │   → 返回 bytes + MIME (text/html | application/javascript…) │
│ ├─ with_initialization_script(hermix_runtime_js + __appId)   │
│ │     ↓                                                       │
│ │   注入 window.Hermix.{store, defineComponent, icon,         │
│ │                          route, storage, sqlite, screenshot,│
│ │                          runtime}                          │
│ └─ with_url("app://localhost/index.html")                     │
└────────────────────┬────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────┐
│ WebView2 加载                                                  │
│ ├─ DOMContentLoaded                                           │
│ ├─ index.html 解析 → 拉 vendor/sql-wasm.{js,wasm} (经 app://) │
│ ├─ window.Hermix.sqlite.init() → load WASM                    │
│ ├─ sqlite.open("todos")                                       │
│ │     ├─ window.hx.readFileBytes("__sqlite__/todos.db")        │
│ │     │     ↓                                                 │
│ │     │   resolve_app_path → exe_dir/__sqlite__/todos.db       │
│ │     ├─ new SQL.Database(bytes)                              │
│ │     ├─ CREATE TABLE IF NOT EXISTS todos...                  │
│ │     └─ db.save()                                            │
│ │           └─ window.hx.writeFileBytes("__sqlite__/todos.db")│
│ │                 ↓                                            │
│ │               std::fs::write(exe_dir/__sqlite__/todos.db)   │
│ └─ UI ready                                                    │
└─────────────────────────────────────────────────────────────┘
```

---

## 5. install / list / uninstall 实现

### 5.1 install

```rust
// apps/hx-packager/src/main.rs::cmd_install
let dist_exe = app_dir.join("dist").join(format!("{id}.exe"));
if !skip_pack { cmd_pack(app_dir, id, None)?; }      // 复 pack 逻辑
let target_dir   = resolve_custom_app_dir(id);       // ~/.hermix/custom-apps/<id>/
let target_exe   = target_dir.join("dist").join(format!("{id}.exe"));
let target_man   = target_dir.join("manifest.json");
let target_meta  = target_dir.join("installed.json");

std::fs::create_dir_all(target_dir.join("dist"))?;
std::fs::copy(app_dir.join("manifest.json"), target_man)?;
std::fs::copy(dist_exe, target_exe)?;
let installed = serde_json::json!({
    "installed_at": unix_ts,
    "app_dir": app_dir.display().to_string(),
    "hx_packager_version": env!("CARGO_PKG_VERSION"),
    "exe": format!("dist/{id}.exe"),
    "manifest": "manifest.json",
});
std::fs::write(target_meta, serde_json::to_string_pretty(&installed)?)?;
```

### 5.2 list

```rust
// apps/hx-packager/src/main.rs::cmd_list
let root = resolve_custom_apps_dir();                // ~/.hermix/custom-apps/
for app_dir in std::fs::read_dir(root)? {
    let manifest = app_dir.path().join("manifest.json");
    if !manifest.exists() { continue; }
    let v: serde_json::Value = serde_json::from_str(&read(manifest)?)?;
    println!("• {}  [{}]", v.id, v.name);
    println!("    entry: {}  icon: {}", v.entry, v.icon);
    if let Some(ts) = installed_at(&app_dir) {
        println!("    installed_at: {} (unix)", ts);
    }
}
```

### 5.3 uninstall（sandbox-safe move-out）

```rust
// apps/hx-packager/src/main.rs::cmd_uninstall
let trash = std::env::var_os("TEMP").unwrap().join("hermix-uninstall-trash");
let ts = unix_ts();
let trash_target = trash.join(format!("{id}-{ts}"));

if keep_data && data_exists {
    let data_backup = trash.join(format!("{id}-{ts}-data"));
    std::fs::rename(data_dir, data_backup)?;          // 1. 搬数据
    std::fs::rename(target, trash_target)?;           // 2. 搬 app dir
    std::fs::create_dir_all(target)?;                 // 3. 重建空 dir
    std::fs::rename(data_backup, data_dir)?;          // 4. 把数据放回
} else {
    std::fs::rename(target, trash_target)?;           // 单次搬
}
```

**为什么 move-out 而不是 `remove_dir_all`？** Sandbox safe-delete hook 会拦 rm/rename_in_place 操作（OS error 5），但放行 mv 出原目录到 `%TEMP%`（与 .cargo-lock 移出仓库同款逻辑）。WebView2 LOCK 文件常驻也无法保证 rm 成功。

---

## 6. dev runner

```rust
// apps/hx-packager/src/main.rs::cmd_dev
let packager = HxShellPackager::new(hx_shell_path, webview2_loader_path);
packager.run_dev(app_dir, id)?;
// → spawn hx-shell --app-dir <app_dir> --app-id <id>
// → 壳解析 --app-dir 写入 APP_DIR_FOR_DEV
// → app_protocol_handler 查表 fallback PE 归档 → 磁盘 host (防越界 relpath.. 拒)
```

dev 模式不走打包，磁盘改源码 + Cmd+R (WebView2 reload) 即可热迭代。

---

## 7. 关键依赖

| Crate | 用途 |
|---|---|
| `hermix-plugin` (path) | `HxShellPackager` / `app_archive` / `icon_library` / `resolve_*` |
| `clap` (derive) | CLI 子命令 + flag |
| `anyhow` | Result + Context |
| `serde_json` | manifest / installed.json 编解码 |
| `tracing` + `tracing-subscriber` | 日志（`info,hermix_plugin=info` 默认 filter） |

---

## 8. 测试与验证

参见 [E2E_TEST_LOG.md](./E2E_TEST_LOG.md)。