#!/usr/bin/env bash
# Sidecar freshness guard.
#
# Why this exists (2026-10-06): a v2.0 defect report came back saying five
# bugs were "still unfixed" after a round of repairs. They were all fixed —
# in a commit made four hours *after* the sidecar binary the test ran
# against. `release/target/release/hermix.exe` had not been rebuilt, so
# `cargo tauri build` packaged the old one and the retest measured code that
# no longer existed. Two engineer-days of "the fix didn't work" on a binary
# that never had the fix.
#
# `binaries/hermix-<target>.exe` is what the Tauri app actually launches, and
# cargo does not refresh it as a side effect of `cargo tauri build`. Per
# AGENTS.md / MEMORY.md the copy is a manual step, which means it is a step
# that gets skipped.
#
# What it checks: is any Rust source newer than the sidecar? A "yes" means a
# rebuild is required before packaging. Soft gate by design — a fresh
# `git clone` gives every file the same mtime and passes trivially, so this
# is a local pre-release tripwire, not a CI correctness check.
#
# Implementation note: one `find -newer` rather than a per-file `stat` loop.
# The loop version spawned ~550 stat processes and blew past a 120s timeout
# on Windows Git Bash (same trap as check-notes.sh).
#
# Exit 0 = fresh (or nothing to compare), 1 = stale.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

BIN_DIR="apps/desktop-tauri/src-tauri/binaries"
CONF="apps/desktop-tauri/src-tauri/tauri.conf.json"
SIDECAR="$BIN_DIR/hermix-x86_64-pc-windows-msvc.exe"

if [ ! -f "$SIDECAR" ]; then
  echo "  SKIP: $SIDECAR not present (nothing packaged yet)"
  exit 0
fi

# --- Check 1: stray sibling binaries ---------------------------------------
# Every `externalBin` entry must resolve to EXACTLY one file. Tauri matches
# these by exact name, so a `.stale-*` / `.update` sibling never reaches the
# installer — but it is exactly what a future `cp binaries/hermix*.exe ...`
# glob would grab, and four of them were sitting here (175 MB, from v0.x,
# useless for rollback). This check exists so they cannot quietly accumulate
# again.
stray=$(ls "$BIN_DIR" 2>/dev/null | grep -E '\.(stale|update|old|bak|backup)(-|$)|~$' || true)
if [ -n "$stray" ]; then
  echo "  STRAY: backup binaries in $BIN_DIR"
  echo "$stray" | sed 's/^/         /'
  echo "         Tauri's externalBin matches by exact name so these are NOT"
  echo "         packaged — but they are what a glob-based cp would grab."
  echo "         Delete them."
  echo ""
fi

# --- Check 2: mtime vs the Rust workspace ---------------------------------
# What decides the sidecar's contents: the Rust workspace. Deliberately NOT
# `apps/desktop-tauri/src` — frontend .ts/.tsx changes do not go into the
# sidecar (they go into the Tauri bundle), so counting them here would raise a
# false STALE on every UI-only commit and train people to ignore the gate.
stale=$(find crates apps/desktop-tauri/src-tauri/src \
         -name '*.rs' -newer "$SIDECAR" -print -quit 2>/dev/null | head -1)

if [ -n "$stale" ]; then
  echo "  STALE: source changes postdate the packaged sidecar"
  echo "         sidecar : $SIDECAR"
  echo "         newest  : $stale"
  echo ""
  echo "         Anything tested through the packaged app is running code that"
  echo "         predates the current tree. Rebuild before believing a retest:"
  echo "           cd apps/desktop-tauri && npm run ensure-sidecar"
  echo "         (compiles hermix-server into repo-root target/,"
  echo "          copies into binaries/ and verifies byte-identical via sha256)"
  echo ""
  exit 1
fi

if [ -n "$stray" ]; then
  exit 1
fi

# --- Check 2b: embedded / bundled static resources --------------------------
# Same mtime logic as Check 2, but for static assets that ride into the app
# through compile-time include_str! or the Tauri resources block:
#   builtin-soul/SOUL.md — compiled into hermix-desktop.exe (first-launch seed)
#   resources/loader/*   — shipped verbatim via tauri.conf.json `resources`
# 2026-10-08: builtin-soul/SOUL.md was still the 08-12 revision while the
# runtime copy had a same-day fix — fresh installs seeded the old persona.
# Compare each file against its own directory subtree; a hit means the
# packaged copy predates the tree and the bundle must be rebuilt.
# 外来二进制（esbuild.exe / WebView2Loader.dll）没有本仓源码，与自研产物
# （hx-shell.exe，源码在 apps/hx-shell/）混放同一目录时，按目录比会让
# 后来放入的外来文件把自研产物误判成 STALE——这类改跟自己的源码比。
RES_STALE=""
res_check() { # $1 = sentinel file, $2 = dir to compare against, $3 = 源码目录（可选，缺省用 $2）
  [ -f "$1" ] || return 0
  local hit
  hit=$(find "${3:-$2}" -type f -newer "$1" -print -quit 2>/dev/null | head -1)
  if [ -n "$hit" ]; then
    echo "  STALE: bundled resource predates its sources"
    echo "         resource: $1"
    echo "         newest  : $hit"
    echo ""
    RES_STALE=1
  fi
}
res_check "apps/desktop-tauri/src-tauri/builtin-soul/SOUL.md" "apps/desktop-tauri/src-tauri/builtin-soul"
res_check "apps/desktop-tauri/src-tauri/resources/loader/hx-shell.exe" "apps/desktop-tauri/src-tauri/resources/loader" "apps/hx-shell/src"
# 外来二进制（esbuild.exe / WebView2Loader.dll）**没有本仓源码** —— 它们随
# 上游发行版（esbuild / WebView2 SDK）一起提交。按同目录比，它们必然比同目录
# 里后放入的自研产物（hx-shell.exe）旧 → 误判成 STALE（2026-10-22 实测：
# 打包前检查一直 exit 1，与本次改动无关）。它们该比的是**上游发行版**，仓库
# 里没有 → 与 markitdown 同一条不对称：**只查在位，不查新鲜度**。
res_presence() {
  if [ ! -f "$1" ]; then
    echo "  MISSING: bundled resource absent: $1"
    RES_STALE=1
  fi
}
res_presence "apps/desktop-tauri/src-tauri/resources/loader/WebView2Loader.dll"
res_presence "apps/desktop-tauri/src-tauri/resources/loader/esbuild.exe"

if [ -n "$RES_STALE" ]; then
  echo "         Rebuild the bundle so the fresh copy is compiled/packaged in."
  echo ""
  exit 1
fi

# --- Check 3: the other externalBin sidecars are PRESENT --------------------
# `markitdown` ships through the same mechanism, so a missing one
# breaks the installer outright. Presence is checked; **staleness is not**,
# and that asymmetry is deliberate:
#
#   hermix      — built from THIS repo. Its binary must track the Rust tree,
#                 so mtime is a valid signal and Check 2 applies.
#   others      — prebuilt artifacts of separate upstream projects (e.g.
#                 markitdown-rs). There is no crate and no build script for
#                 them in this repo; they are refreshed when their own project
#                 publishes a release. Comparing their mtime against this
#                 tree's .rs files would be meaningless — every hermix commit
#                 would light the gate up, which is how a gate gets ignored.
#
# So: presence yes, staleness no. If you ever vendor one of these into the
# workspace and build it here, add it to the Check 2 list explicitly rather
# than widening the glob.
if [ -f "$CONF" ]; then
  for base in $(sed -n 's/.*"binaries\/\([a-z0-9_-]*\)".*/\1/p' "$CONF" | sort -u); do
    f="$BIN_DIR/${base}-x86_64-pc-windows-msvc.exe"
    if [ ! -f "$f" ]; then
      echo "  MISSING: externalBin '$base' is declared in tauri.conf.json but"
      echo "           $f does not exist — the installer cannot be built."
      echo ""
      exit 1
    fi
  done
fi

echo "  OK: hermix sidecar tracks the Rust tree; all externalBins present; no stray binaries"
exit 0
