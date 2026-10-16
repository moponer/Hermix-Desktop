// tsc-unlock.mjs（本地诊断工具，勿提交进常规依赖流）
// 背景：node_modules 符号链接由提权进程创建，非提权进程被 Windows
// "不受信任的装入点"过滤拦截（os error 44），tsc 全灭。
// 原理：junction 的信任按**创建者**判定 —— 本脚本以当前进程重建全部
// 依赖入口为 junction（fs.symlinkSync 'junction' 无需特权），再用
// tsconfig paths 把模块解析指到这棵可信树，tsc 即可完整跑通。
import { readdirSync, readFileSync, writeFileSync, existsSync, mkdirSync, rmSync, symlinkSync, lstatSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { execSync } from "node:child_process";

const APP = resolve("apps/desktop-tauri");
const SRC = join(APP, "src");
const NM = join(APP, "node_modules");
const ROOT_NM = resolve("node_modules");
const PNPM = join(ROOT_NM, ".pnpm");
const JN = resolve(".tsc-junctions");

/* 1. 扫 src 全部裸模块导入 + /// <reference types> */
const bare = new Set(["vite/client"]);
const scan = (dir) => {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) scan(p);
    else if (/\.(tsx?|d\.ts)$/.test(e.name)) {
      const t = readFileSync(p, "utf8");
      for (const m of t.matchAll(/(?:from|import)\s+["']([^"']+)["']/g)) bare.add(m[1]);
      for (const m of t.matchAll(/import\(\s*["']([^"']+)["']\s*\)/g)) bare.add(m[1]);
      for (const m of t.matchAll(/\/\/\/\s*<reference\s+types=["']([^"']+)["']/g)) bare.add(m[1]);
    }
  }
};
scan(SRC);
const pkgs = new Set();
for (const spec of bare) {
  if (spec.startsWith(".") || spec.startsWith("/") || spec.startsWith("@/")) continue;
  pkgs.add(spec.startsWith("@") ? spec.split("/").slice(0, 2).join("/") : spec.split("/")[0]);
}
console.log("依赖入口：", [...pkgs].sort().join(", "));

/* 2. 解析每个包的 .pnpm 真实目录 */
function realDir(pkg) {
  // 优先读现有 symlink 的 target（readlink 只读元数据，无需穿透）
  try {
    const link = join(NM, pkg);
    if (existsSync(link) || lstatSync(link).isSymbolicLink?.()) {
      const t = readlinkSyncSafe(link);
      if (t) return resolve(dirname(link), t);
    }
  } catch { /* fallthrough */ }
  // 兜底：.pnpm 目录取匹配版本
  const key = pkg.replace("/", "+");
  if (!existsSync(PNPM)) return null;
  const hits = readdirSync(PNPM).filter((d) => d.startsWith(key + "@")).sort();
  if (hits.length === 0) return null;
  return join(PNPM, hits[hits.length - 1], "node_modules", pkg);
}
function readlinkSyncSafe(p) {
  try { return readlinkSync(p); } catch { return null; }
}
import { readlinkSync } from "node:fs";

/* 3. 建 junction 树 + 生成 tsconfig.check.json */
rmSync(JN, { recursive: true, force: true });
mkdirSync(JN, { recursive: true });
const paths = {};
let ok = 0, miss = [];
for (const pkg of [...pkgs].sort()) {
  const real = realDir(pkg);
  if (!real || !existsSync(real)) { miss.push(pkg); continue; }
  const jn = join(JN, pkg);
  mkdirSync(dirname(jn), { recursive: true });
  try {
    if (existsSync(jn)) rmSync(jn, { recursive: true, force: true });
    symlinkSync(real, jn, "junction");   // junction 无需特权，创建者=本进程 → 本进程可信
    paths[pkg] = ["./.tsc-junctions/" + pkg];
    ok++;
  } catch (e) {
    console.error("junction 失败:", pkg, String(e).slice(0, 120));
    miss.push(pkg);
  }
}
console.log(`junction 建 ${ok} 个，未解析 ${miss.length}：${miss.join(", ")}`);

writeFileSync(join(APP, "tsconfig.check.json"), JSON.stringify({
  extends: "./tsconfig.json",
  compilerOptions: {
    baseUrl: ".",
    paths: { "*": ["./.tsc-junctions/*"] },
    skipLibCheck: true,
    noEmit: true,
  },
  include: ["src"],
}, null, 2));
console.log("tsconfig.check.json 已生成");
