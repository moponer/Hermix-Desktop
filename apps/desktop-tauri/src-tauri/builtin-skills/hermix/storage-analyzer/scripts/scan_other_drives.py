#!/usr/bin/env python3
"""Scan non-system drives (D:, E:, F:, ...) for storage hot spots.

Read-only: only uses os.scandir / os.stat — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

The built-in ``storage_analyzer`` tool focuses on C: (system drive) hot spots.
This script extends coverage to all other mounted volumes by:

  1. Enumerating all drives A:..Z:\.
  2. Skipping the system drive (C:).
  3. For each remaining drive, scanning its immediate children's sizes.
  4. Also checking common user-data patterns on those drives:
     - ``X:\\Users`` (user profiles on non-C drives)
     - ``X:\\Projects`` / ``X:\\workspace`` (dev projects)
     - ``X:\\Downloads``
     - ``X:\\Backups``
     - ``X:\\Steam`` / ``X:\\Games`` (game installs, typically large)
     - ``X:\\VirtualBox VMs`` / ``X:\\VMs`` (virtual machine images)
     - ``X:\\node_modules`` patterns (deep dev dependencies)

Output: JSON with per-drive breakdown, top directories, and space summary.
"""

import os
import json
import time

# ── Configurable constants ──────────────────────────────────────────────────
MIN_SIZE_MB = 50           # Only report dirs >= this size
TOP_N_PER_DRIVE = 40       # Max dirs per drive
SCAN_TIMEOUT_SECS = 90     # Hard wall-clock limit


def fmt_size(bytes_val):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def dir_size(path, deadline):
    """Recursively calculate directory size (read-only)."""
    total = 0
    file_count = 0
    denied = False
    try:
        it = os.scandir(path)
    except (PermissionError, OSError):
        return 0, 0, True

    for entry in it:
        if time.time() > deadline:
            break
        try:
            if entry.is_dir(follow_symlinks=False):
                s, fc, d = dir_size(entry.path, deadline)
                total += s
                file_count += fc
                if d:
                    denied = True
            elif entry.is_file(follow_symlinks=False):
                total += entry.stat().st_size
                file_count += 1
        except (PermissionError, OSError):
            denied = True
            continue

    return total, file_count, denied


def list_drives():
    """List all mounted drives on Windows."""
    drives = []
    for letter in range(ord('A'), ord('Z') + 1):
        root = f"{chr(letter)}:\\"
        if os.path.isdir(root):
            drives.append(root)
    return drives


def get_system_drive():
    """Get the system drive letter (e.g. 'C:\\')."""
    return os.environ.get("SystemDrive", "C:") + "\\"


def common_subdir_targets(drive_root):
    """Return well-known subdirectories on non-system drives that are often large."""
    return [
        ("users", os.path.join(drive_root, "Users")),
        ("projects", os.path.join(drive_root, "Projects")),
        ("workspace", os.path.join(drive_root, "workspace")),
        ("workspaces", os.path.join(drive_root, "workspaces")),
        ("downloads", os.path.join(drive_root, "Downloads")),
        ("backups", os.path.join(drive_root, "Backups")),
        ("backup", os.path.join(drive_root, "backup")),
        ("steam", os.path.join(drive_root, "Steam")),
        ("steam_library", os.path.join(drive_root, "SteamLibrary")),
        ("games", os.path.join(drive_root, "Games")),
        ("epic", os.path.join(drive_root, "Epic")),
        ("vms", os.path.join(drive_root, "VirtualBox VMs")),
        ("vms_alt", os.path.join(drive_root, "VMs")),
        ("docker", os.path.join(drive_root, "docker")),
        ("data", os.path.join(drive_root, "Data")),
        ("media", os.path.join(drive_root, "Media")),
        ("photos", os.path.join(drive_root, "Photos")),
        ("videos", os.path.join(drive_root, "Videos")),
    ]


def scan_drive(drive_root, deadline, min_bytes):
    """Scan a single drive: root children + common subdirs."""
    entries = []
    denied = False

    # Scan root-level children
    try:
        root_entries = list(os.scandir(drive_root))
    except (PermissionError, OSError):
        return [], True

    for entry in root_entries:
        if time.time() > deadline:
            break
        try:
            if entry.is_symlink():
                continue
            if entry.is_file(follow_symlinks=False):
                # Skip individual files at root level (not useful for directory analysis)
                continue
            if not entry.is_dir(follow_symlinks=False):
                continue

            sz, fc, d = dir_size(entry.path, deadline)
            if d:
                denied = True
            if sz >= min_bytes:
                entries.append({
                    "name": entry.name,
                    "path": entry.path,
                    "size_bytes": sz,
                    "size_h": fmt_size(sz),
                    "file_count": fc,
                    "denied": d,
                })
                print(f"  📁 {entry.name}: {fmt_size(sz)}")
        except (PermissionError, OSError):
            denied = True
            continue

    # Also scan known hot-spot subdirs (for drives where root-level scan
    # might miss user data stored deeper)
    for label, path in common_subdir_targets(drive_root):
        if time.time() > deadline:
            break
        if not os.path.isdir(path):
            continue
        # Check if we already scanned this dir from root
        if any(e["path"].lower() == path.lower() for e in entries):
            continue
        try:
            sz, fc, d = dir_size(path, deadline)
            if d:
                denied = True
            if sz >= min_bytes:
                entries.append({
                    "name": label,
                    "path": path,
                    "size_bytes": sz,
                    "size_h": fmt_size(sz),
                    "file_count": fc,
                    "denied": d,
                })
        except (PermissionError, OSError):
            denied = True

    # Sort by size descending, truncate
    entries.sort(key=lambda e: e["size_bytes"], reverse=True)
    entries = entries[:TOP_N_PER_DRIVE]

    return entries, denied


def get_drive_usage(drive_root):
    """Get total/free space for a drive using ctypes (Windows)."""
    try:
        import ctypes
        free_bytes = ctypes.c_ulonglong(0)
        total_bytes = ctypes.c_ulonglong(0)
        ctypes.windll.kernel32.GetDiskFreeSpaceExW(
            drive_root,
            ctypes.byref(ctypes.c_ulonglong(0)),
            ctypes.byref(total_bytes),
            ctypes.byref(free_bytes),
        )
        return total_bytes.value, free_bytes.value
    except (OSError, AttributeError):
        return None, None


def classify_entry(name):
    """Give a rough classification hint based on directory name."""
    lower = name.lower()
    if any(kw in lower for kw in ("steam", "game", "epic", "ubisoft", "gog")):
        return "🎮 游戏"
    if any(kw in lower for kw in ("vm", "virtualbox", "hyper-v", "docker", "wsl")):
        return "🖥️ 虚拟机/容器"
    if any(kw in lower for kw in ("backup", "bak", "备份")):
        return "📦 备份"
    if any(kw in lower for kw in ("project", "workspace", "code", "dev", "repo", "github", "gitlab")):
        return "💻 开发项目"
    if any(kw in lower for kw in ("node_modules", ".npm", ".cargo", ".gradle", ".m2", "venv")):
        return "🟢 开发缓存"
    if any(kw in lower for kw in ("download", "下载")):
        return "📥 下载"
    if any(kw in lower for kw in ("video", "media", "movie", "电影", "视频")):
        return "🎬 媒体"
    if any(kw in lower for kw in ("photo", "图片", "图片")):
        return "📷 照片"
    if any(kw in lower for kw in ("user", "用户")):
        return "👤 用户数据"
    return "📁 其他"


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS
    min_bytes = MIN_SIZE_MB * 1024 * 1024

    system_drive = get_system_drive()
    all_drives = list_drives()
    non_system_drives = [d for d in all_drives if d.lower() != system_drive.lower()]

    drive_reports = []
    total_other_bytes = 0
    truncated = False

    for drive_root in non_system_drives:
        if time.time() > deadline:
            truncated = True
            break

        total, free = get_drive_usage(drive_root)
        used = total - free if total and free else None
        print(f"🔍 扫描非系统盘: {drive_root} (总 {fmt_size(total) if total else '?'}, 剩余 {fmt_size(free) if free else '?'})")

        entries, denied = scan_drive(drive_root, deadline, min_bytes)
        drive_total = sum(e["size_bytes"] for e in entries)

        # Add classification hints
        for e in entries:
            e["classification_hint"] = classify_entry(e["name"])

        drive_reports.append({
            "drive": drive_root,
            "disk_total": fmt_size(total) if total else "?",
            "disk_free": fmt_size(free) if free else "?",
            "disk_used": fmt_size(used) if used is not None else "?",
            "disk_total_bytes": total or 0,
            "disk_free_bytes": free or 0,
            "top_dirs": entries,
            "top_dirs_total": fmt_size(drive_total),
            "denied": denied,
        })
        total_other_bytes += drive_total
        print(f"  ↳ {drive_root}: {len(entries)} dirs, {fmt_size(drive_total)} data")

    elapsed = round(time.time() - start, 2)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "truncated": truncated,
        "system_drive": system_drive,
        "drives_scanned": len(non_system_drives),
        "total_other_drive_data": fmt_size(total_other_bytes),
        "drives": drive_reports,
        "notes": [
            "扫描全程只读，删除请使用 storage_clean 工具。",
            f"仅扫描非系统盘（{system_drive} 为系统盘，由 storage_analyzer 覆盖）。",
            "classification_hint 仅供参考，帮助快速判断目录类型。",
            "游戏/虚拟机目录通常很大但删除需谨慎（🔴）；开发缓存/下载可考虑清理（🟢🟡）。",
        ],
    }
    if truncated:
        result["notes"].append(f"⚠️ 扫描在 {SCAN_TIMEOUT_SECS}s 预算内未完成，部分盘符被跳过。")

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
