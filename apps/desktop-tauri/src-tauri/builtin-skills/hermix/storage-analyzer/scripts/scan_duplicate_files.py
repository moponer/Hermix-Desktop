#!/usr/bin/env python3
"""Scan for duplicate files by content hash across user directories.

Read-only: only uses os.scandir / os.stat / hashlib — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Strategy (fast → slow):
  1. Group files by exact size (free).
  2. For size-groups with 2+ files, compute MD5 of the first 64 KB (quick fingerprint).
  3. For files that still collide, compute full-file MD5 (slow but definitive).

Only files >= MIN_SIZE_MB are considered (small files cost more to hash than
the space they save).

Output: JSON with duplicate groups, each listing the files and total
reclaimable space (all but one copy in each group).
"""

import os
import json
import time
import hashlib
from collections import defaultdict

# ── Configurable constants ──────────────────────────────────────────────────
MIN_SIZE_MB = 10           # Only consider files >= this size
TOP_N_GROUPS = 30          # Max duplicate groups to report
MAX_FILES_TO_HASH = 5000   # Safety limit on total files to full-hash
SCAN_TIMEOUT_SECS = 90     # Hard wall-clock limit
QUICK_HASH_KB = 64         # Read first N KB for quick fingerprint

# Directories to scan (user-facing areas; skip Windows / Program Files)
def scan_targets():
    """Return (label, path) pairs for directories to scan for duplicates."""
    profile = os.environ.get("USERPROFILE", "")
    local = os.environ.get("LOCALAPPDATA", os.path.join(profile, "AppData", "Local"))
    roaming = os.environ.get("APPDATA", os.path.join(profile, "AppData", "Roaming"))

    return [
        ("downloads", os.path.join(profile, "Downloads")),
        ("documents", os.path.join(profile, "Documents")),
        ("desktop", os.path.join(profile, "Desktop")),
        ("pictures", os.path.join(profile, "Pictures")),
        ("videos", os.path.join(profile, "Videos")),
        ("music", os.path.join(profile, "Music")),
        ("appdata_local", local),
        ("appdata_roaming", roaming),
        ("user_profile_root", profile),
    ]


def fmt_size(bytes_val):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if bytes_val < 1024:
            return f"{bytes_val:.1f} {unit}"
        bytes_val /= 1024
    return f"{bytes_val:.1f} PB"


def quick_hash(path, size, chunk_size=QUICK_HASH_KB * 1024):
    """Hash the first chunk of a file for fast fingerprinting. Read-only."""
    h = hashlib.md5()
    try:
        with open(path, "rb") as f:
            chunk = f.read(chunk_size)
            h.update(chunk)
            # If file is small enough that we read it all, this IS the full hash
            if size <= chunk_size:
                return h.hexdigest()
            # Otherwise, also hash the last chunk for extra discrimination
            f.seek(max(0, size - chunk_size))
            tail = f.read(chunk_size)
            h.update(tail)
    except (PermissionError, OSError):
        return None
    return h.hexdigest()


def full_hash(path, chunk_size=8192):
    """Compute full MD5 of a file. Read-only."""
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


def collect_files(root, min_bytes, deadline):
    """Walk a directory tree and collect files >= min_bytes. Yields dicts."""
    try:
        it = os.scandir(root)
    except (PermissionError, OSError):
        return

    for entry in it:
        if time.time() > deadline:
            return
        try:
            if entry.is_dir(follow_symlinks=False):
                yield from collect_files(entry.path, min_bytes, deadline)
            elif entry.is_file(follow_symlinks=False):
                st = entry.stat()
                if st.st_size >= min_bytes:
                    yield {
                        "path": entry.path,
                        "name": entry.name,
                        "size_bytes": st.st_size,
                        "size_h": fmt_size(st.st_size),
                        "modified": time.strftime("%Y-%m-%d", time.localtime(st.st_mtime)),
                    }
        except (PermissionError, OSError):
            continue


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS
    min_bytes = MIN_SIZE_MB * 1024 * 1024

    # ── Phase 1: Collect candidate files and group by size ───────────────
    size_groups = defaultdict(list)  # size_bytes -> [file_info, ...]
    total_scanned = 0
    truncated = False
    denied_dirs = []

    for label, root in scan_targets():
        if time.time() > deadline:
            truncated = True
            break
        if not os.path.isdir(root):
            continue
        dir_count = 0
        for finfo in collect_files(root, min_bytes, deadline):
            total_scanned += 1
            dir_count += 1
            size_groups[finfo["size_bytes"]].append(finfo)
            if dir_count % 200 == 0:
                print(f"📋 {label}: {dir_count} files, {total_scanned} total")

    # Only keep size-groups with 2+ files (potential duplicates)
    candidate_groups = {sz: files for sz, files in size_groups.items() if len(files) >= 2}

    # ── Phase 2: Quick-hash fingerprint ──────────────────────────────────
    # For each size-group, compute quick_hash and further group
    hash_groups = defaultdict(list)  # (size, quick_hash) -> [file_info, ...]
    total_quick_hashed = 0

    for sz, files in candidate_groups.items():
        if time.time() > deadline:
            truncated = True
            break
        for finfo in files:
            if time.time() > deadline:
                truncated = True
                break
            qh = quick_hash(finfo["path"], sz)
            if qh is not None:
                hash_groups[(sz, qh)].append(finfo)
                total_quick_hashed += 1
        if total_quick_hashed > 0 and total_quick_hashed % 50 == 0:
            print(f"🔑 快速哈希: {total_quick_hashed}/{sum(len(f) for f in candidate_groups.values())} files")

    # Only keep groups with 2+ files after quick-hash
    candidate_hash_groups = {
        key: files for key, files in hash_groups.items() if len(files) >= 2
    }

    # ── Phase 3: Full-hash verification ──────────────────────────────────
    # Confirm duplicates with full-file hash
    full_hash_groups = defaultdict(list)  # (size, full_hash) -> [file_info, ...]
    total_full_hashed = 0

    for (sz, qh), files in candidate_hash_groups.items():
        if time.time() > deadline:
            truncated = True
            break
        if total_full_hashed >= MAX_FILES_TO_HASH:
            truncated = True
            break
        for finfo in files:
            if total_full_hashed >= MAX_FILES_TO_HASH:
                truncated = True
                break
            if time.time() > deadline:
                truncated = True
                break
            fh = full_hash(finfo["path"])
            if fh is not None:
                full_hash_groups[(sz, fh)].append(finfo)
                total_full_hashed += 1
                if total_full_hashed % 10 == 0:
                    print(f"🔐 全量哈希: {total_full_hashed}/{MAX_FILES_TO_HASH} files")

    # Final duplicate groups: 2+ files with same size + full hash
    duplicate_groups = []
    total_reclaimable = 0

    for (sz, fh), files in full_hash_groups.items():
        if len(files) < 2:
            continue
        # Sort by modified date — keep the newest, the rest are redundant
        files.sort(key=lambda f: f["modified"], reverse=True)
        redundant_bytes = sum(f["size_bytes"] for f in files[1:])
        total_reclaimable += redundant_bytes

        duplicate_groups.append({
            "hash": fh,
            "size_bytes": sz,
            "size_h": fmt_size(sz),
            "copies": len(files),
            "reclaimable_bytes": redundant_bytes,
            "reclaimable_h": fmt_size(redundant_bytes),
            "keep_newest": files[0]["path"],
            "duplicates": [f["path"] for f in files[1:]],
            "all_copies": [f["path"] for f in files],
            "classification": "🟢 重复文件",
        })

    # Sort by reclaimable size, descending
    duplicate_groups.sort(key=lambda g: g["reclaimable_bytes"], reverse=True)
    top_groups = duplicate_groups[:TOP_N_GROUPS]

    # Summary by directory
    by_dir = defaultdict(lambda: {"groups": 0, "reclaimable_bytes": 0})
    for g in duplicate_groups:
        for p in g["all_copies"]:
            parent = os.path.dirname(p)
            by_dir[parent]["groups"] += 1
            by_dir[parent]["reclaimable_bytes"] += g["reclaimable_bytes"]

    elapsed = round(time.time() - start, 2)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "truncated": truncated,
        "files_scanned": total_scanned,
        "quick_hashed": total_quick_hashed,
        "full_hashed": total_full_hashed,
        "duplicate_groups_found": len(duplicate_groups),
        "total_reclaimable": fmt_size(total_reclaimable),
        "total_reclaimable_bytes": total_reclaimable,
        "top_duplicate_groups": top_groups,
        "by_directory": {
            k: {
                "groups": v["groups"],
                "reclaimable": fmt_size(v["reclaimable_bytes"]),
            }
            for k, v in sorted(by_dir.items(), key=lambda x: x[1]["reclaimable_bytes"], reverse=True)[:20]
        },
        "notes": [
            "扫描全程只读，删除请使用 storage_clean 工具。",
            f"仅扫描 ≥{MIN_SIZE_MB}MB 的文件（小文件节省空间有限但哈希开销大）。",
            "重复文件建议保留最新版（keep_newest），删其余副本。",
            "⚠️ 清理前请确认文件确实冗余——某些应用会在不同位置放功能不同的同名文件。",
        ],
    }
    if truncated:
        result["notes"].append(f"⚠️ 扫描在 {SCAN_TIMEOUT_SECS}s 预算内未完成，部分目录被跳过。")

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
