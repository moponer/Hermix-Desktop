#!/usr/bin/env node
/**
 * P2-1：断言前端 wire 类型与 Rust 定义同步。
 *
 * 做法是「重新生成一遍，然后看生成目录有没有变化」——这是唯一能真正
 * 拦住「改了 Rust 结构体忘了同步前端」的方式（手写类型 + 手写类型并存
 * 时，两边各自演化，迟早漂移）。
 *
 * 用法：`npm run check:bindings`（在 apps/desktop-tauri 下）或在 CI 里直接跑。
 */
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const BINDINGS = "apps/desktop-tauri/src/api/bindings";

function run(cmd, args) {
  return execFileSync(cmd, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

try {
  run("cargo", [
    "test",
    "--manifest-path",
    "crates/hermix-transport/Cargo.toml",
    "--features",
    "typescript",
    "export_bindings",
  ]);
} catch (e) {
  console.error("[bindings] 生成失败：");
  console.error(e.stderr || e.message);
  process.exit(1);
}

// 同时覆盖「已跟踪文件被改」与「生成了新文件」两种情况。
const status = run("git", ["status", "--porcelain", "--", BINDINGS]).trim();
if (status) {
  console.error(
    `[bindings] 前端绑定与 Rust 定义不一致 —— 改了 Rust 结构体后要跑 ` +
      `\`npm run gen:bindings\` 并提交生成物：\n${status}`,
  );
  process.exit(1);
}

console.log("[bindings] 与 Rust 定义同步 ✓");
