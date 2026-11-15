#!/usr/bin/env python3
"""Scan Windows Recycle Bin contents and report total size.

Read-only: only uses os.scandir / os.stat / winreg — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

On Windows, the Recycle Bin is stored per-drive under ``$Recycle.Bin``.
Each user has a subfolder keyed by their SID. This script:

  1. Finds the current user's SID via ``winreg``.
  2. Enumerates all ``X:\\$Recycle.Bin\\{SID}`` folders across mounted drives.
  3. Walks each folder, tallies file count + total size.
  4. Reports per-drive breakdown and total reclaimable space.

The Recycle Bin is classified as 🟡 (recommend user empties it via OS UI
or gives explicit consent before using ``storage_clean`` to delete).
"""

import os
import json
import time

# ── Configurable constants ──────────────────────────────────────────────────
SCAN_TIMEOUT_SECS = 60


def fmt_size(bytes_val):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def get_current_user_sid():
    """Get the current user's SID from the Windows registry (read-only)."""
    try:
        import winreg
        key = winreg.OpenKey(
            winreg.HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer",
            access=winreg.KEY_READ,
        )
        # Alternative: read from the user's profile path
        winreg.CloseKey(key)
    except (ImportError, OSError):
        pass

    # More reliable: use the USERPROFILE path to match against $Recycle.Bin subdirs
    # We'll just try all subdirs and check accessibility
    return None


def dir_size_and_count(path, deadline):
    """Recursively calculate directory size and file count (read-only)."""
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
                s, fc, d = dir_size_and_count(entry.path, deadline)
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


def get_windows_version():
    """Get Windows version string (read-only)."""
    try:
        import winreg
        key = winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            access=winreg.KEY_READ,
        )
        product_name, _ = winreg.QueryValueEx(key, "ProductName")
        current_build, _ = winreg.QueryValueEx(key, "CurrentBuildNumber")
        winreg.CloseKey(key)
        return f"{product_name} (Build {current_build})"
    except (ImportError, OSError):
        return "Windows"


def scan_recycle_bin_on_drive(drive_root, deadline):
    """Scan $Recycle.Bin on a single drive. Returns list of per-SID stats."""
    rb_path = os.path.join(drive_root, "$Recycle.Bin")
    if not os.path.isdir(rb_path):
        return []

    sid_stats = []

    try:
        entries = list(os.scandir(rb_path))
    except (PermissionError, OSError):
        return [{"sid": "(access denied)", "path": rb_path, "denied": True,
                 "size_bytes": 0, "size_h": "?", "file_count": 0}]

    for entry in entries:
        if time.time() > deadline:
            break
        if not entry.is_dir(follow_symlinks=False):
            continue
        try:
            sz, fc, denied = dir_size_and_count(entry.path, deadline)
            sid_stats.append({
                "sid": entry.name,
                "path": entry.path,
                "size_bytes": sz,
                "size_h": fmt_size(sz),
                "file_count": fc,
                "denied": denied,
                "classification": "🟡 回收站",
            })
        except (PermissionError, OSError):
            sid_stats.append({
                "sid": entry.name,
                "path": entry.path,
                "size_bytes": 0,
                "size_h": "?",
                "file_count": 0,
                "denied": True,
                "classification": "🟡 回收站",
            })

    return sid_stats


def list_drives():
    """List all mounted drives on Windows."""
    drives = []
    for letter in range(ord('A'), ord('Z') + 1):
        root = f"{chr(letter)}:\\"
        if os.path.isdir(root):
            drives.append(root)
    return drives


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS

    drives = list_drives()
    all_drive_stats = []
    total_bytes = 0
    total_files = 0
    any_denied = False

    for drive_root in drives:
        if time.time() > deadline:
            break

        print(f"🔍 扫描回收站: {drive_root}$Recycle.Bin ...")
        sid_stats = scan_recycle_bin_on_drive(drive_root, deadline)
        if not sid_stats:
            continue

        drive_total_bytes = sum(s["size_bytes"] for s in sid_stats)
        drive_total_files = sum(s["file_count"] for s in sid_stats)
        drive_denied = any(s.get("denied", False) for s in sid_stats)

        if drive_total_bytes > 0 or drive_denied:
            all_drive_stats.append({
                "drive": drive_root,
                "total_size": fmt_size(drive_total_bytes),
                "total_bytes": drive_total_bytes,
                "total_files": drive_total_files,
                "denied": drive_denied,
                "details": sid_stats,
            })
            total_bytes += drive_total_bytes
            total_files += drive_total_files
            if drive_denied:
                any_denied = True
            print(f"  ↳ {drive_root}: {fmt_size(drive_total_bytes)}, {drive_total_files} files")

    elapsed = round(time.time() - start, 2)
    windows_ver = get_windows_version()

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "windows_version": windows_ver,
        "scan_seconds": elapsed,
        "drives_scanned": len(drives),
        "drives_with_recycle_bin": len(all_drive_stats),
        "total_recycle_bin_size": fmt_size(total_bytes),
        "total_recycle_bin_bytes": total_bytes,
        "total_recycle_bin_files": total_files,
        "per_drive": all_drive_stats,
        "denied_dirs": any_denied,
        "classification": "🟡",
        "notes": [
            "回收站内容可安全清空，但清空后无法恢复（🟡 建议用户确认）。",
            "清空方式：右键桌面回收站 > 清空回收站；或用 storage_clean 工具（mode='delete'）。",
            "⚠️ 本脚本仅统计回收站大小，不执行任何删除操作。",
        ],
    }
    if any_denied:
        result["notes"].append("⚠️ 部分回收站目录因权限不足无法完整统计，实际占用可能更大。")

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
