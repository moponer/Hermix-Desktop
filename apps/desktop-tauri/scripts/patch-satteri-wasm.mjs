// scripts/patch-satteri-wasm.mjs
//
// Fix a packaging flaw in @bruits/satteri-wasm32-wasi: the browser loader
// references its worker with a bare specifier inside `new URL(...)`:
//
//   new URL('@bruits/satteri-wasm32-wasi/wasi-worker-browser.mjs', import.meta.url)
//
// Bare specifiers are not valid URL inputs, so at runtime this resolves to a
// double-path 404 — the fetch returns the SPA fallback HTML and the wasm init
// chain dies during module evaluation. Rewriting it to a relative URL makes
// both vite dev and the rollup production build emit/resolve it correctly.
//
// Idempotent: replaces only when the bad form is still present. Called from
// vite.config.ts at config load so it runs before dev pre-bundling and before
// the production build — a plain `pnpm install` cannot silently re-break it.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const target = path.join(
  here,
  "..",
  "node_modules",
  "@bruits",
  "satteri-wasm32-wasi",
  "satteri_napi.wasi-browser.js",
);

const BAD = "new URL('@bruits/satteri-wasm32-wasi/wasi-worker-browser.mjs', import.meta.url)";
const GOOD = "new URL('./wasi-worker-browser.mjs', import.meta.url)";

if (!fs.existsSync(target)) {
  console.info("[patch-satteri-wasm] target not found, skipping:", target);
} else {
  const src = fs.readFileSync(target, "utf8");
  if (src.includes(BAD)) {
    fs.writeFileSync(target, src.replace(BAD, GOOD));
    console.info("[patch-satteri-wasm] worker URL patched (bare specifier -> relative)");
  }
}
