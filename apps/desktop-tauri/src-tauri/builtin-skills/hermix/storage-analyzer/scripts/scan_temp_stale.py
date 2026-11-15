#!/usr/bin/env python3
"""Scan stale temporary files on Windows.

Read-only: only uses os.scandir / os.stat — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Finds files in TEMP / AppData\\Local\\Temp that haven't been accessed in N days,
groups them by extension, and reports total reclaimable space.

These are strong 🟢 candidates for storage_clean with mode="trash".
"""

import os
import json
import time
from collections import defaultdict

# ── Configurable constants ──────────────────────────────────────────────────
DAYS_STALE = 7           # Files not accessed in this many days
MIN_SIZE_KB = 100        # Only report files >= this size (avoid tiny noise)
TOP_EXTENSIONS = 20      # Max extension groups to report
TOP_FILES = 30           # Max individual stale files to list
SCAN_TIMEOUT_SECS = 60   # Hard wall-clock limit


def fmt_size(bytes_val):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def scan_temp_dir(root, stale_cutoff, min_bytes, deadline):
    """Walk a temp directory and collect stale files."""
    total_files = 0
    stale_files = []
    denied = False

    try:
        it = os.scandir(root)
    except (PermissionError, OSError):
        return [], 0, True

    for entry in it:
        if time.time() > deadline:
            break
        try:
            if entry.is_dir(follow_symlinks=False):
                sub_files, sub_total, _ = scan_temp_dir(
                    entry.path, stale_cutoff, min_bytes, deadline
                )
                stale_files.extend(sub_files)
                total_files += sub_total
            elif entry.is_file(follow_symlinks=False):
                total_files += 1
                st = entry.stat()
                if st.st_atime < stale_cutoff and st.st_size >= min_bytes:
                    stale_files.append({
                        "path": entry.path,
                        "size_bytes": st.st_size,
                        "size_h": fmt_size(st.st_size),
                        "age_days": round((time.time() - st.st_atime) / 86400, 1),
                        "ext": os.path.splitext(entry.name)[1].lower() or "(no ext)",
                    })
        except (PermissionError, OSError):
            denied = True
            continue

    return stale_files, total_files, denied


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS
    stale_cutoff = time.time() - (DAYS_STALE * 86400)
    min_bytes = MIN_SIZE_KB * 1024

    # Scan both TEMP and AppData\Local\Temp
    temp_dirs = set()
    temp_dirs.add(os.environ.get("TEMP", ""))
    local = os.environ.get("LOCALAPPDATA", "")
    if local:
        temp_dirs.add(os.path.join(local, "Temp"))
    temp_dirs.discard("")
    temp_dirs.discard(os.path.join(local, "Temp") if not local else "")

    all_stale = []
    total_scanned = 0
    any_denied = False

    for td in temp_dirs:
        if not os.path.isdir(td):
            continue
        files, count, denied = scan_temp_dir(td, stale_cutoff, min_bytes, deadline)
        all_stale.extend(files)
        total_scanned += count
        if denied:
            any_denied = True

    # Group by extension
    ext_groups = defaultdict(lambda: {"count": 0, "size_bytes": 0, "examples": []})
    for f in all_stale:
        ext = f["ext"]
        ext_groups[ext]["count"] += 1
        ext_groups[ext]["size_bytes"] += f["size_bytes"]
        if len(ext_groups[ext]["examples"]) < 3:
            ext_groups[ext]["examples"].append(f["path"])

    # Sort extensions by size desc
    sorted_exts = sorted(ext_groups.items(), key=lambda x: x[1]["size_bytes"], reverse=True)
    sorted_exts = sorted_exts[:TOP_EXTENSIONS]

    ext_report = []
    for ext, data in sorted_exts:
        ext_report.append({
            "extension": ext,
            "count": data["count"],
            "total_size": fmt_size(data["size_bytes"]),
            "total_bytes": data["size_bytes"],
            "examples": data["examples"],
        })

    # Top individual stale files
    all_stale.sort(key=lambda f: f["size_bytes"], reverse=True)
    top_stale = all_stale[:TOP_FILES]

    total_stale_bytes = sum(f["size_bytes"] for f in all_stale)
    elapsed = round(time.time() - start, 2)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "temp_dirs_scanned": list(temp_dirs),
        "total_files_scanned": total_scanned,
        "stale_files_found": len(all_stale),
        "stale_total_size": fmt_size(total_stale_bytes),
        "stale_total_bytes": total_stale_bytes,
        "stale_threshold_days": DAYS_STALE,
        "by_extension": ext_report,
        "top_stale_files": top_stale,
        "denied_dirs": any_denied,
        "notes": [
            "扫描全程只读。删除请使用 storage_clean 工具（mode='trash' 移回收站）。",
            f"stale = 超过 {DAYS_STALE} 天未访问。",
            "临时文件通常可安全清理（🟢），但留意最近正在使用的日志/锁文件。",
        ],
    }

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
