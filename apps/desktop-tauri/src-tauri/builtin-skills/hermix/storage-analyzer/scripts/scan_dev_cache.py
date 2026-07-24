#!/usr/bin/env python3
"""Scan developer cache sizes on Windows.

Read-only: only uses os.scandir / os.stat — no write/delete ops.
Designed to run via the ``execute_code`` tool (CodeExecutionGuard: Approved).

Reports sizes of common dev tool caches (npm, pip, cargo, gradle, etc.).
Dev caches are almost always safe to clear (🟢) — they'll be re-downloaded
on the next build/install.
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


def dev_cache_targets():
    """Return (label, path, description) tuples for known dev caches."""
    profile = os.environ.get("USERPROFILE", "")
    local = os.environ.get("LOCALAPPDATA", os.path.join(profile, "AppData", "Local"))

    return [
        ("npm cache", os.path.join(profile, ".npm"),
         "npm 包缓存，npm cache clean --force 可清理，或直接删"),
        ("npm _cacache", os.path.join(profile, ".npm", "_cacache"),
         "npm 下载缓存核心，占大头"),
        ("pip cache", os.path.join(local, "pip", "Cache"),
         "pip 下载缓存，pip cache purge 可清理"),
        ("uv cache", os.path.join(local, "uv", "cache"),
         "uv 包管理器缓存，uv cache clean 可清理"),
        ("yarn cache", os.path.join(local, "Yarn", "Cache"),
         "Yarn 下载缓存，yarn cache clean 可清理"),
        ("gradle cache", os.path.join(profile, ".gradle", "caches"),
         "Gradle 构建缓存，可删旧版本"),
        ("gradle wrapper", os.path.join(profile, ".gradle", "wrapper", "dists"),
         "Gradle wrapper 分发包"),
        ("maven cache", os.path.join(profile, ".m2", "repository"),
         "Maven 本地仓库，可删旧版本节省空间"),
        ("nuget cache", os.path.join(profile, ".nuget", "packages"),
         "NuGet 包缓存，dotnet nuget locals all --clear 可清理"),
        ("cargo cache", os.path.join(profile, ".cargo", "registry"),
         "Cargo (Rust) 包缓存，cargo cache -a 可查看"),
        ("rustup toolchains", os.path.join(profile, ".rustup", "toolchains"),
         "Rust 工具链，可删旧版本"),
        ("go build cache", os.path.join(local, "go-build"),
         "Go 构建缓存，go clean -cache 可清理"),
        ("go module cache", os.path.join(profile, "go", "pkg", "mod"),
         "Go 模块缓存，go clean -modcache 可清理"),
        ("playwright browsers", os.path.join(local, "ms-playwright"),
         "Playwright 浏览器二进制，npx playwright install 可重装"),
        ("electron cache", os.path.join(profile, ".cache", "electron"),
         "Electron 二进制缓存"),
        ("general .cache", os.path.join(profile, ".cache"),
         "通用用户缓存目录"),
        ("pnpm store", os.path.join(profile, ".local", "share", "pnpm", "store"),
         "pnpm 内容寻址存储，pnpm store prune 可清理"),
        ("bun cache", os.path.join(profile, ".bun", "install", "cache"),
         "Bun 运行时缓存"),
    ]


def main():
    start = time.time()
    deadline = start + SCAN_TIMEOUT_SECS

    caches = []
    total_bytes = 0
    found_count = 0

    for label, path, desc in dev_cache_targets():
        if not os.path.isdir(path):
            continue
        if time.time() > deadline:
            break

        sz, fc, denied = dir_size(path, deadline)
        if sz > 0 or os.path.isdir(path):
            found_count += 1
            caches.append({
                "name": label,
                "path": path,
                "size_bytes": sz,
                "size_h": fmt_size(sz),
                "file_count": fc,
                "denied": denied,
                "description": desc,
                "classification": "🟢",
            })
            total_bytes += sz

    caches.sort(key=lambda c: c["size_bytes"], reverse=True)
    elapsed = round(time.time() - start, 2)

    # Group by ecosystem
    ecosystems = {}
    ecosystem_map = {
        "npm": ["npm"], "pip": ["pip"], "uv": ["uv"], "yarn": ["yarn"],
        "gradle": ["gradle"], "maven": ["maven"], "nuget": ["nuget"],
        "cargo": ["cargo"], "rustup": ["rustup"], "go": ["go"],
        "playwright": ["playwright"], "electron": ["electron"],
        "general": ["general"], "pnpm": ["pnpm"], "bun": ["bun"],
    }
    for c in caches:
        eco = "other"
        for key, names in ecosystem_map.items():
            if any(n in c["name"].lower() for n in names):
                eco = key
                break
        ecosystems.setdefault(eco, []).append(c)

    result = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "scan_seconds": elapsed,
        "dev_caches_found": found_count,
        "total_dev_cache_size": fmt_size(total_bytes),
        "total_dev_cache_bytes": total_bytes,
        "caches": caches,
        "by_ecosystem": {k: {"items": v, "total": fmt_size(sum(c["size_bytes"] for c in v))}
                         for k, v in ecosystems.items()},
        "notes": [
            "开发缓存几乎总是可安全清理（🟢）——下次构建/安装时自动重新下载。",
            "优先清理最大的缓存；旧版本的工具链（rustup/gradle wrapper）可放心删。",
            "清理方式：对应工具的清理命令（见 description），或用 storage_clean 工具直接删目录。",
        ],
    }

    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
