---
name: storage-analyzer
description: >
  Windows 只读存储分析助手。扫描整机/指定目录磁盘占用，找出占空间大户，
  把每一项分成 🟢可自动清理 / 🟡需人工判断 / 🔴谨慎清理 三级并给出可执行
  处置方案。全程在聊天对话里完成——扫描用 storage_analyzer 工具或 Python
  脚本（通过 execute_code 执行，只读），清理用 storage_clean 工具（会弹
  聊天审批卡片确认，绝不起本地服务器/浏览器）。
  务必在以下场景使用：用户说"存储分析""磁盘满了""C盘/硬盘满了""空间不够"
  "清理空间""清理磁盘""占空间""哪些东西占地方""帮我看看存储""看一下电脑存储/空间"
  "存储空间""电脑空间不够""内存满了/不够/不足""看下内存/存储"（中文口语里
  "内存"常指存储空间）"storage analysis""disk cleanup""清缓存""磁盘清理"；
  或用户抱怨电脑没空间、想知道什么东西吃硬盘、想要清理建议时。注意：若用户明确
  指运行内存/RAM（如"哪个进程吃内存""内存占用高"想看任务管理器），那是 RAM
  不是存储，不属于本 skill。
  Keywords (English, for keyword auto-routing): storage analysis, disk
  cleanup, disk full, free space, C drive, clean cache, windows cleanup,
  disk space, junk files, hard drive.
tags:
  - storage
  - disk
  - cleanup
  - windows
  - cache
  - c-drive
  - free-space
  - disk-full
  - 清理磁盘
  - 磁盘清理
  - C盘
  - 空间不够
  - 清缓存
  - 存储分析
  - 清理空间
  - 占空间
---

# Storage Analyzer (Rust 原生 + Python 脚本 / 对话内执行)

扫描 → 在对话里分级 → 用户确认 → 聊天审批后清理。没有本地 HTTP 服务，没有
浏览器报告，所有交互都在聊天里。

## 铁律

- **扫描只读。** `storage_analyzer` 工具和 Python 脚本只列目录/算大小/读元信息，
  绝不创建、移动、删除任何东西。
- **删除走聊天审批。** 清理用 `storage_clean` 工具，它注册为 Dangerous，
  dispatch 闸门会先弹聊天审批卡片让用户确认，且工具自身要求 `confirm: true`
  双保险。永远不要在用户没明确同意前调用它。
- **估算标注清楚。** 涉及"可释放空间"一律说明是估算值（无权限目录会被漏掉，
  体量可能低估）。
- **路径、命令保留原文不翻译。**
- **C 盘可访问，你不在沙盒里。** 你有完整文件系统访问权限，不要因为看到
  `C:\Windows\System32` 等系统目录禁令就误以为整个 C 盘不可碰——只有那几个
  系统目录受限，`C:\Users`、`C:\Projects` 等都可正常读。
- **不要用 terminal + PowerShell 做磁盘分析。** Git Bash 会剥离引号导致
  PowerShell 命令失败。优先用 `storage_analyzer` 工具或 `execute_code` + Python。

## 扫描工具选择

根据任务选择最合适的扫描方式：

| 工具 | 适用场景 | 风险级别 | 审批 |
|------|---------|---------|------|
| `storage_analyzer` | 全局热点扫描、盘符容量概览 | Safe | 无 |
| `execute_code` + Python 脚本 | 定向分析（大文件/临时文件/浏览器缓存/开发缓存/重复文件/回收站/其他盘） | Caution | 无 |
| `terminal` + PowerShell | ❌ 不要用！Git Bash 引号剥离导致命令失败 | Caution | 每次弹 |

**优先级：`storage_analyzer` → `execute_code` + Python → `terminal`（最后手段）**

## Python 扫描脚本

以下脚本位于 `builtin-skills/hermix/storage-analyzer/scripts/` 目录。
通过 `execute_code` 工具调用，**不要用 terminal 运行**。

### 调用方式

用 `execute_code` 工具，将脚本代码作为 `code` 参数传入：

```json
{
  "language": "python",
  "code": "<脚本代码>",
  "timeout": 90
}
```

### 可用脚本

#### 1. `scan_large_files.py` — 大文件 + 陈旧文件扫描

找出一台机器上占空间的大文件，标记哪些超过 N 天没访问过（stale）。
适合回答"什么东西占我硬盘"这类全局问题。

脚本内的常量可按需调整：
- `MIN_SIZE_MB = 50` — 只报告 ≥50MB 的文件
- `DAYS_STALE = 30` — 超过 30 天未访问标记为 stale
- `TOP_N = 50` — 每组最多报告 50 个文件

#### 2. `scan_temp_stale.py` — Temp 目录旧文件扫描

扫描 TEMP / AppData\Local\Temp 下的过期临时文件，按扩展名分组统计。
**这些全是 🟢 可安全清理的候选。**

脚本内的常量可按需调整：
- `DAYS_STALE = 7` — 超过 7 天未访问的临时文件
- `MIN_SIZE_KB = 100` — 过滤掉太小的文件

#### 3. `scan_browser_cache.py` — 浏览器缓存统计

统计 Chrome、Edge、Firefox 等浏览器的缓存大小。
**浏览器缓存全是 🟢 可安全清理。** 建议关闭浏览器后再清理。

#### 4. `scan_duplicate_installers.py` — Downloads 重复安装包

找出 Downloads 目录里的重复安装包（同名文件）和过期安装包。
**安装包全是 🟢，删了可以重新下载。**

脚本内的常量可按需调整：
- `DAYS_STALE = 30` — 超过 30 天的安装包标记为 stale

#### 5. `scan_dev_cache.py` — 开发缓存统计

统计 npm/pip/cargo/gradle/maven 等开发工具的缓存大小。
**开发缓存全是 🟢，下次构建时自动重新下载。**

#### 6. `scan_duplicate_files.py` — 全盘重复文件检测

基于内容哈希（MD5）的重复文件检测，覆盖用户目录（Downloads/Documents/Desktop/Pictures/AppData 等）。

检测策略（快→慢）：
1. 按文件大小分组（零成本过滤）
2. 对同大小文件计算首尾 64KB 的快速指纹
3. 对快速指纹碰撞的文件计算全文件 MD5 确认

**重复文件建议 🟢（保留最新版，删冗余副本），但需人工确认文件确实冗余。**

脚本内的常量可按需调整：
- `MIN_SIZE_MB = 10` — 只检测 ≥10MB 的文件
- `TOP_N_GROUPS = 30` — 最多报告 30 组重复
- `MAX_FILES_TO_HASH = 5000` — 全量哈希的安全上限

#### 7. `scan_recycle_bin.py` — 回收站扫描

扫描所有盘符的 `$Recycle.Bin` 目录，统计回收站占用的空间和文件数量。
**回收站是 🟡 — 可安全清空，但清空后不可恢复，建议用户确认。**

注意：回收站目录通常需要管理员权限才能完整读取，部分目录可能显示 `denied`。

#### 8. `scan_other_drives.py` — 非 C 盘热区扫描

扫描系统盘（C:）以外的所有盘符（D:/E:/F:/...），列出每个盘符的大目录。

额外检测常见用户数据模式：
- 游戏（Steam/Epic/Ubisoft）
- 虚拟机（VirtualBox/Hyper-V/WSL/Docker）
- 开发项目（Projects/workspace/code）
- 备份（Backups）
- 媒体（Videos/Photos/Music）

每个目录自动给出 `classification_hint`（🎮游戏/🖥️虚拟机/💻开发项目/📦备份 等），
帮助快速判断清理优先级。

**游戏/虚拟机目录通常是 🔴；开发缓存/下载 🟢🟡。**

## 执行流程

### Step 1 扫描（只读，调工具或脚本）

**方式 A：全局概览（推荐先用这个）**

```json
// storage_analyzer 工具
{ "action": "scan" }
// 或指定目录
{ "path": "C:\\" }
```

**方式 B：定向深入（用 Python 脚本）**

根据用户的具体需求，选择对应的脚本通过 `execute_code` 执行：
- "大文件在哪" → `scan_large_files.py`
- "临时文件多少" → `scan_temp_stale.py`
- "浏览器缓存多大" → `scan_browser_cache.py`
- "Downloads 有啥安装包" → `scan_duplicate_installers.py`
- "开发缓存多大" → `scan_dev_cache.py`
- "有没有重复文件" → `scan_duplicate_files.py`
- "回收站占多少" → `scan_recycle_bin.py`
- "其他盘（D/E/F）有什么" → `scan_other_drives.py`

可以组合使用：先用 `storage_analyzer` 看全局，再用 Python 脚本深入特定区域。

### Step 2 分析与分级（你来做，不动手）

读 `system.os` 判断系统（本项目仅 Windows）。然后做这几件事：

1. **挑 Top 5** 占用大户，判定类型（系统资产/应用本体/应用数据/应用缓存/开发缓存/
   用户文件/媒体内容/下载内容/虚拟机镜像/回收站/重复文件/其他盘数据/其他）。
   如果 Top 5 里有大项来自非 C 盘，用 `scan_other_drives.py` 深入查看。
2. **识别"神秘大目录"**：UUID 命名的、不明隐藏目录，要追查它属于哪个 App。
   必要时再调一次 `storage_analyzer` 深入一层（`path` 指向那个子目录），但仍只读。
3. **三级分类 = 清理决策清单，不是全盘点。** 只把"存在'要不要动它'这个决策"的项
   放进三灯；日常在用的正常应用、操作系统本身、海量零碎小文件没有清理决策，不进
   三灯。判定标准：
   - 🟢 **可自动清理**：纯缓存、临时文件、安装包残留、明确可再生且不影响功能、
     不丢用户数据（npm/pip/uv/gradle 等开发缓存、浏览器缓存、Downloads 里的
     exe/msi 安装包）。
   - 🟡 **需人工判断**：含用户数据或有判断成本（离线视频、文档、项目代码
     node_modules、聊天记录、设计稿）。给内容画像 + 处置路径（应用内清理 / 系统
     工具 / 文件管理器手动审查）+ 风险提示。
   - 🔴 **谨慎清理（有决策但不建议手删）**：重复安装的应用、想卸载的大应用、运行
     中应用的核心数据等。给"为什么不建议手删" + 具体卸载步骤（自带卸载器 / 设置里
     卸载 / 第三方卸载工具清残留）。

每个 🟢 项要能给：预估释放空间、清理前需关闭的进程。

大小字段写干净：`约 14 GB` 即可，"约"已表示估算，别再加"（估算）"。

### Step 3 在对话里给结论 + 征求意见

用一段话给结论先行的摘要：总可释放估算、最该先清的 2-3 项、风险最高的一项。
然后以 🟢🟡🔴 三色列出决策清单（可折叠感用标题分隔），让用户决定清哪些。

**不要**自动调用 `storage_clean`。等用户说"清掉绿色的"或点名某几项后，再进入 Step 4。

### Step 4 清理（聊天审批，调工具）

用户确认后，调 `storage_clean`：

```json
{
  "paths": ["C:\\Users\\me\\AppData\\Local\\Temp\\old", "C:\\Users\\me\\.npm\\_cacache"],
  "mode": "trash",        // trash=移回收站(可逆，默认) / delete=直接删(不可逆)
  "confirm": true         // 必须显式 true
}
```

安全模型（工具内置，无需你判断）：
- **黑名单拒绝**：`C:\Windows`、Program Files、ProgramData、hermix 自身目录——
  任何试图清理这些的请求都会被工具直接拒绝。
- **白名单放行**：仅接受位于 AppData\Local、TEMP、用户开发缓存（.cache/.npm/
  .gradle/.m2/.nuget/.cargo/.rustup 等）、Downloads 的路径。
- 不在白名单内的路径会被 `refused` 列出，不会误删。

`trash` 走系统回收站（可逆，清空才真正释放）；`delete` 直接删（不可逆）。工具返回
`deleted` / `freed_bytes` / `refused` / `errors`，把结果如实反馈给用户。

**⚠️ 永远不要用 Python 脚本做清理！** `execute_code` 的 `CodeExecutionGuard`
会拒绝 `os.remove()`、`shutil.rmtree()` 等删除操作。所有清理必须走 `storage_clean`
工具——它有白名单+审批卡片+回收站三重安全机制。

## Windows 目录分级速查

| 目录 | 装什么 | 典型分级 |
|---|---|---|
| `%LOCALAPPDATA%` | 浏览器缓存、应用数据、Temp，最大头 | 缓存 🟢 / 应用数据 🟡 |
| `%LOCALAPPDATA%\Temp`、`%TEMP%` | 临时文件 | 🟢 |
| `%APPDATA%`（Roaming） | 应用配置/数据 | 🟡 |
| 浏览器缓存 `...\Google\Chrome\User Data\*\Cache`、Edge 同构 | 浏览器缓存 | 🟢 |
| `%USERPROFILE%\.cache`、`.npm`、`.gradle`、`.m2`、`.nuget\packages`、pip Cache、`Yarn`、`uv` | 开发缓存 | 🟢 |
| `C:\Program Files`、`Program Files (x86)` | 应用本体 | 🔴 仅重复/想卸时上灯 |
| `%USERPROFILE%\Downloads` 的安装包 | exe/msi 残留 | 🟢 |
| `C:\$Recycle.Bin`（及 D:/E:/... 同路径） | 回收站 | 🟡 提示用户清空 |
| 非 C 盘（D:/E:/F:/...） | 游戏/虚拟机/备份/开发项目/媒体 | 因类型而异（用 `scan_other_drives.py` 扫描） |
| 用户目录下的重复文件 | 完全相同的文件冗余副本 | 🟢 保留一份删其余（用 `scan_duplicate_files.py` 检测） |

## 系统占用（不上灯，归"系统及其他"）

- `C:\Windows\WinSxS`：组件存储，**绝不能手删**，建议 `DISM /Online /Cleanup-Image /StartComponentCleanup`
- `C:\Windows\SoftwareDistribution\Download`：Windows Update 缓存，用磁盘清理处理
- `hiberfil.sys`、`pagefile.sys`：系统管理，别手动删
- 间接释放：设置 > 系统 > 存储 > 存储感知；`cleanmgr`；扩展磁盘清理选 Windows 更新清理

## 长期优化建议（给用户的话术素材）

- 定期清理：浏览器缓存、开发缓存（npm/pip/gradle）、Downloads 安装包
- 大文件归档到外置盘 / 云盘
- 用系统"存储感知"自动回收空间
