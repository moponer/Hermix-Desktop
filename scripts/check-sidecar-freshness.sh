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
  echo "           cargo build --release -p hermix-server --bin hermix \\"
  echo "             --target-dir release/target"
  echo "           cp release/target/release/hermix.exe \\"
  echo "              $SIDECAR"
  echo "           # verify, do not assume — the copy must be byte-identical:"
  echo "           cmp release/target/release/hermix.exe $SIDECAR"
  echo ""
  exit 1
fi

if [ -n "$stray" ]; then
  exit 1
fi

# --- Check 3: the other externalBin sidecars are PRESENT --------------------
# `officecli` / `markitdown` ship through the same mechanism, so a missing one
# breaks the installer outright. Presence is checked; **staleness is not**,
# and that asymmetry is deliberate:
#
#   hermix      — built from THIS repo. Its binary must track the Rust tree,
#                 so mtime is a valid signal and Check 2 applies.
#   others      — prebuilt artifacts of separate upstream projects (OfficeCLI,
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
