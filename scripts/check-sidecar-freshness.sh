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

SIDECAR="apps/desktop-tauri/src-tauri/binaries/hermix-x86_64-pc-windows-msvc.exe"

if [ ! -f "$SIDECAR" ]; then
  echo "  SKIP: $SIDECAR not present (nothing packaged yet)"
  exit 0
fi

# First tracked-ish Rust source newer than the sidecar, if any.
stale=$(find crates apps/desktop-tauri/src-tauri/src-tauri apps/desktop-tauri/src \
         -name '*.rs' -newer "$SIDECAR" -print -quit 2>/dev/null | head -1)

if [ -z "$stale" ]; then
  echo "  OK: no Rust source is newer than the packaged sidecar"
  exit 0
fi

echo "  STALE: source changes postdate the packaged sidecar"
echo "         sidecar : $SIDECAR"
echo "         newest  : $stale"
echo ""
echo "         Anything tested through the packaged app is running code that"
echo "         predates the current tree. Rebuild before believing a retest:"
echo "           cargo build --release -p hermix-server --bin hermix \\"
echo "             --target-dir release/target"
echo "           cp release/target/release/hermix.exe \\"
echo "              apps/desktop-tauri/src-tauri/binaries/hermix-x86_64-pc-windows-msvc.exe"
echo "           # verify, do not assume — the copy must be byte-identical:"
echo "           cmp release/target/release/hermix.exe \\"
echo "               apps/desktop-tauri/src-tauri/binaries/hermix-x86_64-pc-windows-msvc.exe"
exit 1
