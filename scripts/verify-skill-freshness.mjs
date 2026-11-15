// Skill 陈旧度守卫：拦截仍在引用已下线工具族 / 已删除客户端入口的 skill 文档。
//
// 起因：工作台架构从「schema 驱动的 workspace_* 工具族 + @work 客户端入口」换成
// 「workbench_manager action 工具 + 工作台页新建」后，仓库里的 builtin skill 目录删了，
// 但用户 home 下已 seed 的旧副本不会自动消失，两份文档并存——而旧文档的 description
// 关键词更宽（台账 / 管理面板 / 车辆管理…），路由先命中它，于是「工作台专用对话」拿着
// 一份描述不存在工具的文档在工作：workspace_build 调不到、@work 找不到入口。
//
// 守卫做的是：任何被 LLM 加载的 skill 文档里，只要出现已下线标识就硬失败并给出
// 文件:行号，防止这类「文档比代码活得久」的静默漂移。
//
// 用法: node scripts/verify-skill-freshness.mjs [--home <hermix_home>]

import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

const homeIdx = process.argv.indexOf("--home");
const hermixHome = homeIdx >= 0 ? process.argv[homeIdx + 1] : join(homedir(), ".hermix");

// 已下线标识：workspace_* 工具族（从未在当前代码里注册过）与 @work 客户端入口（已删）。
const STALE_PATTERNS = [
  {
    id: "workspace-tool-family",
    re: /workspace_(?:build|update_schema|table_schema|query|count|group_count|insert|update|delete)\b/,
    why: "workspace_* 工具族不存在——工作台代码操作唯一入口是 workbench_manager 的 action",
  },
  {
    id: "workspace-schema",
    re: /WorkspaceSchema\b/,
    why: "schema 驱动建台已废弃——工作台是代码层（code/）+ 宿主网关数据层（data/）",
  },
  {
    id: "work-mention",
    re: /@work\b/,
    why: "@work 客户端快捷入口已删除——入口是侧栏「工作台」页的「新建」",
  },
];

// 例外：生成的 workbench SKILL.md 里这些标识是作为「禁止臆想」的反例出现的，
// 命中它说明文档在正确地警告，而不是在教错的东西。
const EXCEPTIONS = ["skills/workbench/SKILL.md"];

function isException(absPath, roots) {
  for (const root of roots) {
    const rel = relative(root, absPath).replace(/\\/g, "/");
    if (EXCEPTIONS.some((e) => rel.endsWith(e))) return true;
  }
  return false;
}

function walk(dir, out = []) {
  if (!existsSync(dir)) return out;
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) walk(p, out);
    else if (name.endsWith(".md")) out.push(p);
  }
  return out;
}

const roots = [
  join(hermixHome, "skills"),
  join(repoRoot, "apps/desktop-tauri/src-tauri/builtin-skills"),
  join(repoRoot, "crates/hermix-server/src/dashboard/skills"),
];

const files = roots.flatMap((r) => walk(r));
const hits = [];

for (const file of files) {
  if (isException(file, roots)) continue;
  const lines = readFileSync(file, "utf8").split(/\r?\n/);
  lines.forEach((line, i) => {
    for (const p of STALE_PATTERNS) {
      if (p.re.test(line)) {
        hits.push({ file, line: i + 1, id: p.id, why: p.why, text: line.trim().slice(0, 160) });
      }
    }
  });
}

const scanned = files.length;
if (hits.length === 0) {
  console.log(`skill freshness: OK (${scanned} skill docs scanned, no stale identifiers)`);
  process.exit(0);
}

console.error(`skill freshness: FAIL — ${hits.length} stale reference(s) in ${scanned} skill docs\n`);
for (const h of hits) {
  console.error(`  ${h.file}:${h.line}  [${h.id}]`);
  console.error(`      ${h.text}`);
  console.error(`      -> ${h.why}`);
}
console.error(
  "\n修法：删除或移出 skills 加载目录（旧 builtin skill 从仓库删了，home 副本不会自动清理）。",
);
process.exit(1);
