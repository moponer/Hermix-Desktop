# hx-packager

Hermix 自定义应用打包器 CLI — 把 `app_dir/` 打成单 exe（PE 资源节嵌入 HXAM 归档），或一键安装到 `~/.hermix/custom-apps/<id>/`，或起 dev runner。

> 本 crate 是 [spec/design/custom-app-architecture-spec.md §8](../../docs/design/custom-app-architecture-spec.md) 的落地实现。
> 它复用 `crates/hermix-plugin` 的 `HxShellPackager`（构建 HXAM 归档 + 嵌入 PE 资源），本身只负责 **CLI 编排 + 安装路径**。

---

## 快速开始

```bash
# 1. 构建（一次性，cold build 30+ 分钟，建议放 F 盘）
CARGO_TARGET_DIR="F:/temp/hermix-target/hx-packager" \
  cargo build --release -p hx-packager

# 2. 打包 playground 示例（产物在 playground/dist/playground.exe）
hx-packager pack apps/playground playground

# 3. 安装到 ~/.hermix/custom-apps/playground/
hx-packager install apps/playground playground

# 4. 查看已安装列表
hx-packager list

# 5. 卸载（含数据）
hx-packager uninstall playground
# 保留 sqlite 数据再卸载
hx-packager uninstall playground --keep-data

# 6. 不打包直接起 WebView 看磁盘源码（dev runner）
hx-packager dev apps/playground playground

# 7. 查看 app_dir manifest + 文件树
hx-packager info apps/playground

# 8. 调试归档（验证 should_include 行为）
cargo run --release -p hx-packager --bin debug-archive -- apps/playground
```

---

## 子命令

| 子命令 | 作用 | 产物 |
|---|---|---|
| `pack <app_dir> <id> [--out-dir DIR]` | 把 app_dir 打成单 exe（PE 资源嵌入 HXAM 归档） | `app_dir/dist/<id>.exe` 或 `--out-dir/<id>.exe` |
| `install <app_dir> <id> [--skip-pack]` | pack + 安装到 `~/.hermix/custom-apps/<id>/` | `manifest.json` + `dist/<id>.exe` + `installed.json` |
| `list` | 列出所有已安装 custom app（扫 `custom-apps/*/manifest.json`） | stdout 文本 |
| `uninstall <id> [--keep-data]` | 卸载已安装 app（move-out 到 `%TEMP%/hermix-uninstall-trash/`） | trash 目录 + （可选）重建空 dir |
| `dev <app_dir> <id>` | 不打包，直接 spawn hx-shell 加载磁盘 app_dir（开发热重载） | spawn 子进程 |
| `info <app_dir>` | 打印 manifest 关键字段 + 文件树 | stdout 文本 |

---

## 关键路径

| 路径 | 含义 |
|---|---|
| `~/.hermix/custom-apps/<id>/` | app 安装根目录 |
| `~/.hermix/custom-apps/<id>/manifest.json` | app 元数据（id/name/entry/icon/...） |
| `~/.hermix/custom-apps/<id>/dist/<id>.exe` | 打包产物（壳 + PE 嵌入 HXAM 归档） |
| `~/.hermix/custom-apps/<id>/installed.json` | 安装元数据（installed_at unix ts + app_dir 来源） |
| `~/.hermix/custom-apps/<id>/dist/__sqlite__/<name>.db` | sqlite WASM 落盘（由壳 `writeFileBytes` 写入） |
| `%TEMP%/hermix-uninstall-trash/<id>-<ts>/` | 卸载回收站（move-out 模式，sandbox 拦 rm 故改 mv） |
| `~/.hermix/loader/hx-shell.exe` | 壳本体（dev 模式 spawn 入口） |
| `F:/temp/hermix-target/hx-packager/` | 推荐 CARGO_TARGET_DIR（C 盘经常满，F 盘 79-86 GB free） |

---

## app_dir 期望结构

```
my-app/
├── manifest.json          ← 必需：id/name/entry/icon/tile_color/icon_glyph/category/description
├── index.html             ← manifest.entry 指定（默认 index.html）
├── main.css / app.js      ← 普通前端资源
└── vendor/                ← 现成库（sql-wasm.js、pdf.js 等）
        ├── sql-wasm.js
        ├── sql-wasm.wasm
        └── ...
```

`should_include` 白名单接受的后缀：`.html` / `.htm` / `.css` / `.js` / `.mjs` / `.json` / `.svg` / `.png` / `.jpg` / `.jpeg` / `.gif` / `.webp` / `.ico` / `.wasm` / `.map` / `.txt` / `.md`。

`manifest.json` 示例：
```json
{
  "id": "playground",
  "name": "Playground · Todo",
  "version": "0.1.0",
  "entry": "index.html",
  "icon": "bookmark",
  "tile_color": "#1A1A1A",
  "icon_glyph": "✓",
  "category": "productivity",
  "description": "..."
}
```

---

## 与其它模块的关系

```
apps/hx-packager    ← 本 crate，CLI 编排 + 安装路径
        │
        ├─ 依赖 crates/hermix-plugin
        │       ├─ HxShellPackager (build_final_payload: html + icon + archive)
        │       ├─ app_archive (HXAM 归档格式 build/parse/get/should_include)
        │       ├─ icon_library (embed_app_payload_multi → PE RT_RCDATA 1002)
        │       └─ resolve_hx_shell_path / resolve_webview2_loader_path
        │
        └─ 复用 apps/hx-shell (HxShellPackager 内部嵌入了 hx-shell.exe 作为壳)
```

---

## 已知约束

1. **CARGO_TARGET_DIR 必须 F 盘**：C 盘 100% 满时 cargo link 失败（os error 112）。统一 `CARGO_TARGET_DIR="F:/temp/hermix-target/<crate>"`。
2. **同主机同一时刻只跑一个 cargo**：两个 cargo 抢 `~/.cargo/.package-cache` 全局锁必死锁（"Blocking waiting for file lock on package cache"）。改顺序而非并行。
3. **sandbox 拦 truncate 已有 .exe**：pack 后再 pack 同 id → `dist/<id>.exe` truncate `os error 5`。绕法：写到新文件名（`playground_v2`）或 `--out-dir F:/temp/.../`。
4. **sandbox 拦 uninstall `remove_dir_all`**（WebView2 LOCK 文件常驻）→ 改 `std::fs::rename` move-out 到 `%TEMP%/hermix-uninstall-trash/<id>-<ts>/`。
5. **Windows 黑窗**：spawn hx-shell dev runner 必须 `.creation_flags(0x08000000)`（hermix-plugin 内部已处理）。
6. **PE 资源节类型**：`RT_RCDATA` 1002 = HXAM 归档；1000 = 单 HTML 字符串（fallback）；1001 = 图标。

---

## 参见

- [docs/design/custom-app-architecture-spec.md](../../docs/design/custom-app-architecture-spec.md) — 设计 spec
- [apps/hx-packager/ARCHITECTURE.md](./ARCHITECTURE.md) — 架构详解（HXAM 格式 + PE 注入 + 数据流）
- [apps/hx-packager/E2E_TEST_LOG.md](./E2E_TEST_LOG.md) — 端到端实测记录