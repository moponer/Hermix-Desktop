# AGENTS.md

## What this is

Hermix Desktop — a portable, zero-install AI assistant. Windows-only (MSVC). Rust backend (21 crates, ~146K lines) + React 19 + Tauri v2 frontend.

## Build & verify

**Windows-only**: source MSVC env before any cargo command in Git Bash:

```bash
. ./cargo-env.sh && cargo check --workspace --all-targets
```

Without this, Git Bash's `link.exe` shadows MSVC's and you get opaque linker errors.

Full CI gate (compile + tests + TS typecheck):

```bash
. ./cargo-env.sh && ./scripts/ci-gate.sh
```

Quick commands:

```bash
cargo test --workspace --lib          # ~1,834 tests
cargo test -p hermix-agent --lib      # single crate
cargo check --workspace --all-targets # compile check
cd apps/desktop-tauri && pnpm tsc --noEmit  # frontend types
```

## Structure

- `crates/` — 21 Rust crates. Core dependency chain: `hermix-core` → `hermix-config` → `hermix-agent` → `hermix-server` / `hermix-cli`
- `apps/desktop-tauri/` — Tauri v2 desktop app (React 19 + Tailwind 4 + Vite)
- `scripts/` — CI gate, smoke tests, fixture recording
- `crates/hermix-parity-tests/` — Python↔Rust golden fixtures (run: `cargo test -p hermix-parity-tests`)

Key crates:
- `hermix-cli` — binary entrypoint (`hermix`)
- `hermix-server` — HTTP/WebSocket API + Dashboard
- `hermix-agent` — agent loop, LLM providers, memory
- `hermix-tools` — 30+ tool backends, approval engine
- `hermix-config` — config loading, path management
- `hermix-intelligence` — tool router, thinking modes
- `hermix-focus` — FocusStack, memory salience scoring

## Conventions

- **Windows MSVC only** — no cross-compilation to macOS/Linux
- **PR template required** — must include test evidence (`cargo check`, `cargo test`, `pnpm tsc`), pre-existing cleanup status, and `ci-gate.sh` passing
- **Pre-existing bug markers** — scan for `TODO: pre-existing` / `FIXME: pre-existing` in CI gate; don't introduce them
- **fastembed is optional** — `hermix-intelligence` has `fastembed` feature for local embeddings; `cdn.pyke.io` is geo-blocked in some regions, so default build doesn't require it
- **Parity fixtures** — `hermix-parity-tests/fixtures/` has golden JSON; `pending/` is excluded from test runs
- **Desktop app frontend** — `apps/desktop-tauri/src/`, uses `@` alias for `src/`

## Gotchas

- `.cargo/config.toml` hardcodes MSVC linker path — don't override
- `cargo-env.sh` strips Git's `link.exe` from PATH — critical for linking
- Tauri v2.9.2 doesn't support standalone zip bundle — NSIS produces portable archive
- `hermix-server` has `test-helpers` feature for integration tests
- PR禁区: no multi-platform distribution, no external resource sensing, no cross-platform targets
