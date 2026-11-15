#!/usr/bin/env python3
"""Scan large files across Windows hot-spot directories.

Read-only: only uses os.scandir / os.stat / pathlib — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Output: JSON with top-N large files, grouped by category.

Usage (inside execute_code):
    Adjust the constants below, then run the script.
"""

import os
import json
import time
from pathlib import Path

# ── Configurable constants ──────────────────────────────────────────────────
MIN_SIZE_MB = 50          # Only report files >= this size
TOP_N = 50                # Max files to report
DAYS_STALE = 30           # Files not accessed in this many days are "stale"
SCAN_TIMEOUT_SECS = 80    # Hard wall-clock limit to avoid runaway scans

# ── Windows hot-spot directories ─────────────────────────────────────────────
def default_targets():
    """Return (label, path) pairs for standard Windows hot-spots."""
    profile = os.environ.get("USERPROFILE", "")
    local = os.environ.get("LOCALAPPDATA", os.path.join(profile, "AppData", "Local"))
    roaming = os.environ.get("APPDATA", os.path.join(profile, "AppData", "Roaming"))
    temp = os.environ.get("TEMP", os.path.join(local, "Temp"))

    return [
        ("user_profile", profile),
        ("appdata_local", local),
        ("appdata_roaming", roaming),
        ("temp", temp),
        ("downloads", os.path.join(profile, "Downloads")),
        ("program_files", os.environ.get("ProgramFiles", r"C:\Program Files")),
        ("program_files_x86", os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")),
        ("dev_cache_npm", os.path.join(profile, ".npm")),
        ("dev_cache_gradle", os.path.join(profile, ".gradle")),
        ("dev_cache_m2", os.path.join(profile, ".m2")),
        ("dev_cache_cargo", os.path.join(profile, ".cargo")),
        ("dev_cache_nuget", os.path.join(profile, ".nuget", "packages")),
        ("dev_cache_pip", os.path.join(local, "pip", "Cache")),
        ("dev_cache_uv", os.path.join(local, "uv")),
        ("dev_cache_yarn", os.path.join(local, "Yarn")),
        ("dev_cache_playwright", os.path.join(local, "ms-playwright")),
        ("dev_cache_go", os.path.join(local, "go-build")),
        ("user_cache", os.path.join(profile, ".cache")),
    ]


def fmt_size(bytes_val):
    """Human-readable file size."""
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def scan_directory(root, min_bytes, deadline):
    """Walk a directory tree and collect large files. Yields dicts."""
    try:
        it = os.scandir(root)
    except (PermissionError, OSError):
        return

    for entry in it:
        if time.time() > deadline:
            return
        try:
            if entry.is_dir(follow_symlinks=False):
                yield from scan_directory(entry.path, min_bytes, deadline)
            elif entry.is_file(follow_symlinks=False):
                st = entry.stat()
                if st.st_size >= min_bytes:
                    age_days = (time.time() - st.st_atime) / 86400
                    yield {
                        "path": entry.path,
                        "size_bytes": st.st_size,
                        "size_h": fmt_size(st.st_size),
                        "age_days": round(age_days, 1),
                        "stale": age_days >= DAYS_STALE,
                        "category": "stale" if age_days >= DAYS_STALE else "active",
                    }
        except (PermissionError, OSError):
            continue


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS
    min_bytes = MIN_SIZE_MB * 1024 * 1024

    all_files = []
    truncated = False
    denied = []
    groups = {}

    for label, root in default_targets():
        if time.time() > deadline:
            truncated = True
            break
        if not os.path.isdir(root):
            continue
        group_files = list(scan_directory(root, min_bytes, deadline))
        if group_files:
            group_files.sort(key=lambda f: f["size_bytes"], reverse=True)
            group_files = group_files[:TOP_N]
            groups[label] = group_files
            all_files.extend(group_files)

    # Global top-N
    all_files.sort(key=lambda f: f["size_bytes"], reverse=True)
    top_files = all_files[:TOP_N]

    elapsed = round(time.time() - start, 2)
    total_bytes = sum(f["size_bytes"] for f in all_files)
    stale_bytes = sum(f["size_bytes"] for f in all_files if f["stale"])

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "truncated": truncated,
        "total_files_found": len(all_files),
        "total_size": fmt_size(total_bytes),
        "stale_size": fmt_size(stale_bytes),
        "stale_count": sum(1 for f in all_files if f["stale"]),
        "top_files": top_files,
        "by_group": {k: v for k, v in groups.items()},
        "notes": [
            f"扫描全程只读，删除请使用 storage_clean 工具。",
            f"大小均为估算值，无权限目录会被漏掉。",
            f"stale = 超过 {DAYS_STALE} 天未访问。",
        ],
    }
    if truncated:
        result["notes"].append(f"⚠️ 扫描在 {SCAN_TIMEOUT_SECS}s 预算内未完成，部分目录被跳过。")

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
