# 阻塞型卡死点清单 · 独立复核（hermix-desktop）

被复核对象：`docs/blocking-hang-audit.md`（只读，未修改）
复核产物：本文件 `docs/blocking-hang-audit-review.md`
复核方式：独立 read_file / grep 回源核对，逐条比对位置、行号、机制与线程归属
本轮未启动应用、未跑构建、未跑测试：所有耗时数值均为「未实测」

## 线程归属判定口径（本轮复核标准）

| 类别 | 判定 |
|---|---|
| `#[tauri::command]` 非 async 的同步命令体 | 主进程主线程阻塞（可判） |
| Tauri `setup` 回调内的同步调用 | 主进程主线程阻塞（可判） |
| `pub async fn` 命令体内的同步阻塞调用 | tokio worker，不是主线程阻塞（async 命令体在 async runtime worker 上跑，不是主线程） |
| `spawn_blocking` 内的同步调用 | 阻塞线程池，不是主线程阻塞 |
| `tokio::spawn` / `block_in_place` 内的工作 | tokio worker 或阻塞池，不是主线程阻塞 |

待逐条核：发现 1 / 2 / 3a / 3b / 4 / 5，以及检索面 G1–G12。

## 逐条判定

（复核中，逐条回填）

## 覆盖统计

（复核中）
