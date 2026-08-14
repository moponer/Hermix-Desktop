// 跨 shell 的 dev/build 入口（替代 package.json 里依赖 %VAR%/set/pushd 的内联脚本）。
// 根因：npm 在 PowerShell 下跑内联脚本时 %VAR% 与 set 不展开，导致 CARGO_TARGET_DIR 残留字面量。
// 这里用 node 直接算出绝对路径并写入 env，cargo 子进程必然拿到正确目录，与 shell 无关。
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, ".."); // apps/desktop-tauri
const repoRoot = path.resolve(appDir, "..", ".."); // <repo>
const targetDir = path.join(repoRoot, "release", "target");

// 覆盖 env，确保 cargo（及其子进程）使用统一的绝对 target 目录。
process.env.CARGO_TARGET_DIR = targetDir;

const mode = process.argv[2] || "dev";
const isRelease = mode === "build" || mode === "release";

function sh(cmd, args, opts = {}) {
  console.log(`[${mode}] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (r.error) {
    console.error(`[${mode}] failed to spawn ${cmd}:`, r.error);
    process.exit(1);
  }
  if (r.status !== 0) process.exit(r.status ?? 1);
}

// 非致命：失败不中断主流程（与原始 `|| echo skipped` 语义一致）。
function shOptional(cmd, args) {
  console.log(`[${mode}] $ ${cmd} ${args.join(" ")} (best-effort)`);
  const r = spawnSync(cmd, args, { stdio: "inherit" });
  if (r.status !== 0) console.warn(`[${mode}] hx-shell 运行时同步已跳过（dev 模式不构建 hx-shell，不影响本体开发调试）`);
}

console.log(
  `[${mode}] CARGO_TARGET_DIR=${targetDir} (绝对路径, 前后端/tauri 全部落入此, 不随 cwd 漂移)`
);

if (mode === "dev") {
  sh("cargo", ["build", "-p", "hermix-server", "--target-dir", targetDir]);
  const src = path.join(targetDir, "debug", "hermix.exe");
  const dst = path.join(
    appDir,
    "src-tauri",
    "binaries",
    "hermix-x86_64-pc-windows-msvc.exe"
  );
  console.log(`[dev] Copying fresh binary to sidecar dir...`);
  copyFileSync(src, dst);
  console.log(`[dev] Sidecar updated: ${dst}`);
  console.log(`[dev] Starting Tauri dev (产物也写入 ${targetDir})...`);
  shOptional("npm", ["run", "_:sync-shell-runtime"]);
  sh("cargo", ["tauri", "dev"]);
} else {
  sh("npm", ["run", "_:build-shell"]);
  sh("npm", ["run", "_:sync-shell"]);
  sh("npm", ["run", "_:sync-shell-runtime"]);
  sh("npm", ["run", "build:backend"]);
  sh("npm", ["run", "_:ensure-sidecar"]);
  console.log(`[build] Building Tauri installer (CARGO_TARGET_DIR=${targetDir})...`);
  sh("cargo", ["tauri", "build"]);
}
