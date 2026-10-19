// 跨 shell 的 dev/build 入口（替代 package.json 里依赖 %VAR%/set/pushd 的内联脚本）。
// 根因：npm 在 PowerShell 下跑内联脚本时 %VAR% 与 set 不展开，导致 CARGO_TARGET_DIR 残留字面量。
// 这里用 node 直接算出绝对路径并写入 env，cargo 子进程必然拿到正确目录，与 shell 无关。
//
// 2026-10-09 单 workspace 收敛：不再设置 CARGO_TARGET_DIR——全仓一个 workspace，
// 产物一律落仓库根 target/，任何 env/参数覆盖都是产物分裂的源头。sidecar 刷新
// 统一走 scripts/ensure-sidecar.mjs（编译 + 拷贝 + sha256 校验一条龙）。
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, ".."); // apps/desktop-tauri
const repoRoot = path.resolve(appDir, "..", ".."); // <repo>
const targetDir = path.join(repoRoot, "target"); // 单 workspace 唯一产物目录

const mode = process.argv[2] || "dev";
const isRelease = mode === "build" || mode === "release";

// Windows 上 npm 的真实可执行文件是 npm.cmd，不带 shell 直接 spawn "npm" 会 ENOENT
// （Git Bash / 任何非 cmd 父进程下都会踩到，npm run 内部注入 cmd 才掩盖了这个问题）。
const npmBin = process.platform === "win32" ? "npm.cmd" : "npm";

// .cmd/.bat 不是 PE 可执行文件, 必须交给 cmd.exe 解释, 否则 spawnSync 直接 EINVAL。
function spawnOpts(cmd, opts) {
  const needShell = process.platform === "win32" && /\.(cmd|bat)$/i.test(cmd);
  return { stdio: "inherit", ...(needShell ? { shell: true } : {}), ...opts };
}

function sh(cmd, args, opts = {}) {
  console.log(`[${mode}] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, spawnOpts(cmd, opts));
  if (r.error) {
    console.error(`[${mode}] failed to spawn ${cmd}:`, r.error);
    process.exit(1);
  }
  if (r.status !== 0) {
    // 2026-10-07：原来这里静默 process.exit —— 子进程若自己没打印错误
    // （或被 shell 吞掉），表现为"命令无输出就断、后续步骤全没跑"，
    // 用户无从知道断在哪、为什么。失败必须可见。
    console.error(
      `[${mode}] ✗ ${cmd} ${args.join(" ")} 失败（退出码 ${r.status}${r.signal ? `，信号 ${r.signal}` : ""}），构建中止。`,
    );
    console.error(
      `[${mode}]   上一条命令输出通常包含真实原因（若为空，单独重跑该命令验证）。`,
    );
    process.exit(r.status ?? 1);
  }
}

// 非致命：失败不中断主流程（与原始 `|| echo skipped` 语义一致）。
function shOptional(cmd, args) {
  console.log(`[${mode}] $ ${cmd} ${args.join(" ")} (best-effort)`);
  const r = spawnSync(cmd, args, spawnOpts(cmd));
  if (r.status !== 0) console.warn(`[${mode}] hx-shell 运行时同步已跳过（dev 模式不构建 hx-shell，不影响本体开发调试）`);
}

console.log(`[${mode}] 产物目录: ${targetDir}（单 workspace，仓库根 target/）`);

if (mode === "dev") {
  sh("cargo", ["build", "-p", "hermix-server", "--bin", "hermix"]);
  const src = path.join(targetDir, "debug", "hermix.exe");
  const dst = path.join(
    appDir,
    "src-tauri",
    "binaries",
    "hermix-x86_64-pc-windows-msvc.exe",
  );
  if (!existsSync(src)) {
    console.error(`[dev] ✗ 后端产物缺失: ${src}（cargo build 声称成功，环境异常）`);
    process.exit(1);
  }
  console.log(`[dev] Copying fresh binary to sidecar dir...`);
  copyFileSync(src, dst);
  console.log(`[dev] Sidecar updated: ${dst}`);
  console.log(`[dev] Starting Tauri dev...`);
  shOptional(npmBin, ["run", "_:sync-shell-runtime"]);
  sh("cargo", ["tauri", "dev"]);
} else {
  sh(npmBin, ["run", "_:build-shell"]);
  sh(npmBin, ["run", "_:sync-shell"]);
  sh(npmBin, ["run", "_:sync-shell-runtime"]);
  // 前端（vite build）+ sidecar（编译/拷贝/sha256）由 tauri.conf.json 的
  // beforeBuildCommand → prepare-build.mjs 统一执行，这里不重复调用：
  // 直接 cargo tauri build 也会过同一个钩子，绕不过去（2026-10-19 前端
  // 陈旧事故的根治：此前钩子只刷 sidecar，前端靠人工构建）。
  console.log(`[build] Building Tauri installer...`);
  sh("cargo", ["tauri", "build"]);
}
