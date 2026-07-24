#!/usr/bin/env python3
"""Scan browser cache sizes on Windows.

Read-only: only uses os.scandir / os.stat — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Reports total cache size for Chrome, Edge, Firefox, and other Chromium browsers.
Browser cache is almost always safe to clear (🟢).

Note: Chrome/Edge lock their cache while running, so the reported size
may be slightly stale — but for cleanup decisions this is fine.
"""

import os
import json
import time
from pathlib import Path

# ── Configurable constants ──────────────────────────────────────────────────
SCAN_TIMEOUT_SECS = 60


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
    try:
        it = os.scandir(path)
    except (PermissionError, OSError):
        return 0, 0, True

    denied = False
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


def find_browser_caches():
    """Return (label, path) pairs for known browser cache directories."""
    profile = os.environ.get("USERPROFILE", "")
    local = os.environ.get("LOCALAPPDATA", os.path.join(profile, "AppData", "Local"))
    roaming = os.environ.get("APPDATA", os.path.join(profile, "AppData", "Roaming"))

    candidates = [
        # Chrome
        ("Google Chrome - Cache", os.path.join(local, "Google", "Chrome", "User Data", "Default", "Cache")),
        ("Google Chrome - Code Cache", os.path.join(local, "Google", "Chrome", "User Data", "Default", "Code Cache")),
        ("Google Chrome - Service Worker", os.path.join(local, "Google", "Chrome", "User Data", "Default", "Service Worker")),
        # Edge
        ("Microsoft Edge - Cache", os.path.join(local, "Microsoft", "Edge", "User Data", "Default", "Cache")),
        ("Microsoft Edge - Code Cache", os.path.join(local, "Microsoft", "Edge", "User Data", "Default", "Code Cache")),
        # Firefox
        ("Firefox - Cache", os.path.join(roaming, "Mozilla", "Firefox", "Profiles")),
        # Brave
        ("Brave - Cache", os.path.join(local, "BraveSoftware", "Brave-Browser", "User Data", "Default", "Cache")),
        # Opera
        ("Opera - Cache", os.path.join(local, "Opera Software", "Opera Stable", "Cache")),
        # Vivaldi
        ("Vivaldi - Cache", os.path.join(local, "Vivaldi", "User Data", "Default", "Cache")),
        # WeChat (mini-program cache, very common on Chinese Windows)
        ("WeChat Files", os.path.join(profile, "Documents", "WeChat Files")),
    ]
    return candidates


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS

    browsers = []
    total_bytes = 0

    for label, path in find_browser_caches():
        if not os.path.isdir(path):
            continue
        if time.time() > deadline:
            break

        # For Firefox Profiles, scan each profile's cache2 subfolder
        if "Firefox" in label and "Profiles" in path:
            try:
                for prof_dir in os.scandir(path):
                    if prof_dir.is_dir(follow_symlinks=False):
                        cache2 = os.path.join(prof_dir.path, "cache2")
                        if os.path.isdir(cache2):
                            sz, fc, denied = dir_size(cache2, deadline)
                            if sz > 0:
                                browsers.append({
                                    "browser": f"Firefox ({prof_dir.name})",
                                    "path": cache2,
                                    "size_bytes": sz,
                                    "size_h": fmt_size(sz),
                                    "file_count": fc,
                                    "denied": denied,
                                    "classification": "🟢",
                                })
                                total_bytes += sz
            except (PermissionError, OSError):
                continue
            continue

        sz, fc, denied = dir_size(path, deadline)
        if sz > 0 or os.path.isdir(path):
            browsers.append({
                "browser": label,
                "path": path,
                "size_bytes": sz,
                "size_h": fmt_size(sz),
                "file_count": fc,
                "denied": denied,
                "classification": "🟢",
            })
            total_bytes += sz

    browsers.sort(key=lambda b: b["size_bytes"], reverse=True)
    elapsed = round(time.time() - start, 2)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "total_browser_cache": fmt_size(total_bytes),
        "total_browser_cache_bytes": total_bytes,
        "browsers": browsers,
        "notes": [
            "浏览器缓存几乎总是可安全清理（🟢）。",
            "清理方式：浏览器设置 > 清除浏览数据，或用 storage_clean 工具。",
            "Chrome/Edge 运行时会锁定缓存，建议关闭浏览器后再清理。",
        ],
    }

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
