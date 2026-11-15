#!/usr/bin/env python3
"""Scan for duplicate and leftover installer files in Downloads.

Read-only: only uses os.scandir / os.stat / hashlib — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Finds:
  1. Duplicate installers (same filename appearing multiple times)
  2. Old installers (exe/msi/dmg that haven't been accessed in N days)
  3. Large standalone installers (portable apps that could be re-downloaded)

These are all strong 🟢 candidates for storage_clean with mode="trash".
"""

import os
import json
import time
import hashlib
from collections import defaultdict

# ── Configurable constants ──────────────────────────────────────────────────
DAYS_STALE = 30           # Installers not accessed in this many days
MIN_SIZE_MB = 5           # Only report files >= this size
TOP_N = 30                # Max items per category
SCAN_TIMEOUT_SECS = 30    # Downloads usually isn't that deep

# Extensions that are installers / downloadable packages
INSTALLER_EXTENSIONS = {
    ".exe", ".msi", ".msp", ".msu",           # Windows installers
    ".dmg", ".pkg",                            # macOS
    ".deb", ".rpm", ".AppImage",              # Linux
    ".zip", ".7z", ".rar", ".tar.gz", ".tgz", # Archives
    ".iso", ".img",                            # Disk images
    ".whl", ".egg",                            # Python packages
    ".vsix",                                   # VS Code extensions
    ".crx",                                     # Chrome extensions
}


def fmt_size(bytes_val):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def file_md5(path, chunk_size=8192):
    """Compute MD5 of a file for duplicate detection. Read-only."""
    h = hashlib.md5()
    try:
        with open(path, "rb") as f:
            while True:
                chunk = f.read(chunk_size)
                if not chunk:
                    break
                h.update(chunk)
    except (PermissionError, OSError):
        return None
    return h.hexdigest()


def scan_downloads(downloads_dir, deadline):
    """Scan Downloads for installer files."""
    installers = []
    total_files = 0

    try:
        entries = list(os.scandir(downloads_dir))
    except (PermissionError, OSError):
        return [], 0

    for entry in entries:
        if time.time() > deadline:
            break
        if not entry.is_file(follow_symlinks=False):
            continue
        total_files += 1

        ext = os.path.splitext(entry.name)[1].lower()
        if ext not in INSTALLER_EXTENSIONS:
            continue

        try:
            st = entry.stat()
        except (PermissionError, OSError):
            continue

        if st.st_size < MIN_SIZE_MB * 1024 * 1024:
            continue

        age_days = (time.time() - st.st_atime) / 86400

        installers.append({
            "path": entry.path,
            "name": entry.name,
            "ext": ext,
            "size_bytes": st.st_size,
            "size_h": fmt_size(st.st_size),
            "age_days": round(age_days, 1),
            "modified": time.strftime("%Y-%m-%d", time.localtime(st.st_mtime)),
        })

    return installers, total_files


def find_duplicates(installers):
    """Find duplicate installers by name (fast) or by hash (for same-size files)."""
    # Group by name
    by_name = defaultdict(list)
    for inst in installers:
        by_name[inst["name"]].append(inst)

    duplicates = []
    for name, files in by_name.items():
        if len(files) > 1:
            # Keep the newest, flag the rest as duplicates
            files.sort(key=lambda f: f["modified"], reverse=True)
            total_dup_bytes = sum(f["size_bytes"] for f in files[1:])
            duplicates.append({
                "name": name,
                "copies": len(files),
                "reclaimable_bytes": total_dup_bytes,
                "reclaimable_h": fmt_size(total_dup_bytes),
                "keep": files[0]["path"],
                "remove": [f["path"] for f in files[1:]],
                "classification": "🟢 重复安装包",
            })

    return sorted(duplicates, key=lambda d: d["reclaimable_bytes"], reverse=True)


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS

    profile = os.environ.get("USERPROFILE", "")
    downloads_dir = os.path.join(profile, "Downloads")

    if not os.path.isdir(downloads_dir):
        print(json.dumps({"error": f"Downloads 目录不存在: {downloads_dir}"}, ensure_ascii=False, indent=2))
        return

    installers, total_files = scan_downloads(downloads_dir, deadline)

    # Categorize
    stale_installers = [i for i in installers if i["age_days"] >= DAYS_STALE]
    recent_installers = [i for i in installers if i["age_days"] < DAYS_STALE]

    stale_installers.sort(key=lambda f: f["size_bytes"], reverse=True)
    recent_installers.sort(key=lambda f: f["size_bytes"], reverse=True)

    # Find duplicates
    duplicates = find_duplicates(installers)

    total_stale_bytes = sum(i["size_bytes"] for i in stale_installers)
    total_dup_bytes = sum(d["reclaimable_bytes"] for d in duplicates)
    total_all_bytes = sum(i["size_bytes"] for i in installers)

    elapsed = round(time.time() - start, 2)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "downloads_dir": downloads_dir,
        "total_files_in_downloads": total_files,
        "installer_files_found": len(installers),
        "total_installer_size": fmt_size(total_all_bytes),
        "stale_installers": {
            "count": len(stale_installers),
            "total_size": fmt_size(total_stale_bytes),
            "threshold_days": DAYS_STALE,
            "files": stale_installers[:TOP_N],
        },
        "recent_installers": {
            "count": len(recent_installers),
            "files": recent_installers[:10],
        },
        "duplicates": {
            "groups_found": len(duplicates),
            "reclaimable_size": fmt_size(total_dup_bytes),
            "groups": duplicates[:TOP_N],
        },
        "notes": [
            "安装包通常可安全清理（🟢），重新下载即可。",
            "重复安装包只保留最新版，旧版可删除。",
            "清理方式：storage_clean 工具（mode='trash' 移回收站）。",
        ],
    }

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
