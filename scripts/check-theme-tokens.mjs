#!/usr/bin/env node
// Static guard against hardcoded color literals (#hex / rgb() / rgba())
// in *.tsx / *.ts under apps/desktop-tauri/src.
// Exclusions are listed in apps/desktop-tauri/src/styles/themes/EXCEPTIONS.md.
import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join, relative, sep } from "node:path";

const ROOT = process.cwd();
const SRC = join(ROOT, "apps/desktop-tauri/src");
const EXC = join(SRC, "styles/themes/EXCEPTIONS.md");

function walk(d) {
  const out = [];
  for (const entry of readdirSync(d)) {
    const p = join(d, entry);
    const s = statSync(p);
    if (s.isDirectory()) out.push(...walk(p));
    else if (/\.(tsx|ts)$/.test(entry)) out.push(p);
  }
  return out;
}

const allowedFiles = new Set();
if (existsSync(EXC)) {
  for (const line of readFileSync(EXC, "utf8").split("\n")) {
    const m = line.match(/\|\s*(E\d+)\s*\|\s*`([^`]+)`/);
    if (!m) continue;
    const p = m[2].replace(/^apps\/desktop-tauri\/src\//, "");
    allowedFiles.add(p);
  }
}

const COLOR_RX = /(#[0-9a-fA-F]{3,8}\b|rgba?\s*\()/g;
const TOKEN_CTX = /var\(--color-[a-z0-9-]+/;

const violations = [];
for (const file of walk(SRC)) {
  const rel = relative(SRC, file).split(sep).join("/");
  if (allowedFiles.has(rel)) continue;
  const lines = readFileSync(file, "utf8").split("\n");
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    if (/^\s*(\/\/|\*|\/\*)/.test(ln)) continue;
    if (!COLOR_RX.test(ln)) continue;
    if (TOKEN_CTX.test(ln)) continue;
    violations.push(`${rel}:${i + 1}: ${ln.trim()}`);
  }
}

if (violations.length) {
  console.error("[check-theme-tokens] Hardcoded color literals found:");
  for (const v of violations) console.error("  " + v);
  console.error("\nMove them into tokens.css (or list them in EXCEPTIONS.md).");
  process.exit(1);
}
console.log(`[check-theme-tokens] OK (${walk(SRC).length - allowedFiles.size} files scanned)`);
