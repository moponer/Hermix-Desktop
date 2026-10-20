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

// viewer（第 5 个 sidecar）：dev 模式下主程序按「主程序 exe 同目录」找
// hermix-viewer.exe —— 也就是 target/debug/。cargo 的产物正好落在这儿，
// 不需要拷贝。**但前端是编译期嵌进 exe 的**（generate_context! 宏展开期
// 读 dist），所以必须先 vite build 再 cargo build，顺序反了就嵌进旧 UI。
//
// 不构建的后果不是"报错"而是"更安静地跑旧版"：dev 模式此前完全不碰 viewer，
// target/debug/hermix-viewer.exe 停在 cargo 随手留下的旧产物——用户从主程序
// 打开查看器，看到的是几周前的界面，且没有任何提示。
function buildViewer() {
  const viewerDir = path.join(repoRoot, "apps", "viewer");
  const viewerVite = path.join(viewerDir, "node_modules", "vite", "bin", "vite.js");
  if (!existsSync(viewerVite)) {
    console.warn("[dev] viewer 前端依赖未安装，跳过查看器构建（apps/viewer 跑 npm install）");
    return;
  }
  stopRunningViewer();
  sh("node", [viewerVite, "build"], viewerDir);
  sh("cargo", ["build", "-p", "hermix-viewer"]);
}

/** 关掉正在运行的查看器：它锁着 target/debug/hermix-viewer.exe，Windows 文件锁
 *  会让 cargo 报 "permission denied" 覆盖失败，旧二进制原地留下——用户随后从
 *  主程序打开查看器，看到的还是旧界面，且没有任何提示。 */
function stopRunningViewer() {
  if (process.platform !== "win32") return;
  const r = spawnSync("taskkill", ["/IM", "hermix-viewer.exe", "/F"], { encoding: "utf8" });
  if (r.status === 0) {
    console.log("[dev] 已结束正在运行的 hermix-viewer（同名 exe 被占用时 cargo 覆盖不掉）");
  }
}

console.log(`[${mode}] 产物目录: ${targetDir}（单 workspace，仓库根 target/）`);

/**
 * 编译一个 sidecar 并把 debug 产物同步进 `src-tauri/binaries/`。
 *
 * tauri 的 `externalBin` 在 **build.rs 阶段**就要求文件存在（不是打包时），
 * 所以清单里每加一个 sidecar，dev 链路就得同步一个——漏一个的报错是
 * `resource path binaries\xxx.exe doesn't exist`，看起来像配置错，其实是
 * 少了一次拷贝。
 */
function syncDevSidecar(pkg, binName) {
  sh("cargo", ["build", "-p", pkg, "--bin", binName]);
  const src = path.join(targetDir, "debug", `${binName}.exe`);
  const dst = path.join(
    appDir,
    "src-tauri",
    "binaries",
    `${binName}-x86_64-pc-windows-msvc.exe`,
  );
  if (!existsSync(src)) {
    console.error(`[dev] ✗ 产物缺失: ${src}（cargo build 声称成功，环境异常）`);
    process.exit(1);
  }
  console.log(`[dev] Copying fresh binary to sidecar dir...`);
  copyFileSync(src, dst);
  console.log(`[dev] Sidecar updated: ${dst}`);
}

if (mode === "dev") {
  syncDevSidecar("hermix-server", "hermix");
  // 本地文件索引守护进程（viewer 路径栏模糊搜索 / Alt+F 共用）。
  syncDevSidecar("hermix-filesearch", "hermix-filesearch");
  buildViewer();
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
