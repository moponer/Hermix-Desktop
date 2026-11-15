// prepare-build.mjs — 打包前置：先构建前端，再编译并刷新 sidecar。
//
// 2026-10-19：打包链此前只跑 ensure-sidecar，前端靠人工 `npm run build:vite`，
// 结果 `release/frontend` 与 exe 长期过期（10-14 的 exe 配 10-16 的前端，源码已到 10-18），
// `cargo tauri build` 会把陈旧 UI 原样打进安装包。本脚本把两步串成一个入口，
// 挂在 tauri.conf.json 的 beforeBuildCommand 上，任何路径（含绕过 dev.mjs 直接
// `cargo tauri build`）都必须先过这里。
//
// 与 ensure-sidecar.mjs 同规矩：全部路径由本文件位置推算，不依赖调用方 cwd；
// 任一步失败 exit 1 —— 绝不能拿着陈旧前端或旧 sidecar 打包。
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, ".."); // apps/desktop-tauri
const npmBin = process.platform === "win32" ? "npm.cmd" : "npm";

function sh(cmd, args) {
  console.log(`[prepare-build] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, {
    stdio: "inherit",
    cwd: appDir,
    // npm.cmd 不是 PE 可执行文件，Windows 下必须交给 cmd.exe 解释。
    shell: process.platform === "win32" && /\.(cmd|bat)$/i.test(cmd),
  });
  if (r.error) {
    console.error(`[prepare-build] ✗ 无法启动 ${cmd}:`, r.error);
    process.exit(1);
  }
  if (r.status !== 0) {
    console.error(
      `[prepare-build] ✗ ${cmd} ${args.join(" ")} 失败（退出码 ${r.status}），打包中止。`,
    );
    process.exit(r.status ?? 1);
  }
}

// 1) 前端：vite build → ../../release/frontend（tauri.conf.json 的 frontendDist）
sh(npmBin, ["run", "build:vite"]);

// 2) 后端 sidecar：编译 + 拷贝 + sha256 校验
sh(process.execPath, [path.join(__dirname, "ensure-sidecar.mjs")]);

console.log("[prepare-build] ✓ 前端与 sidecar 均为最新，可以打包");
