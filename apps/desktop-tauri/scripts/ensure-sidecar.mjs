// ensure-sidecar.mjs — 打包前置：编译并刷新 sidecar 二进制进 binaries/。
//
// 由 tauri.conf.json 的 beforeBuildCommand 经 `npm run ensure-sidecar` 调起：
// npm run 把脚本 cwd 固定到 package.json 所在目录（apps/desktop-tauri），
// 与 Tauri CLI 的调用 cwd 解耦；脚本内全部路径用本文件位置推算，不依赖 cwd。
//
// 任一步失败 exit 1 —— cargo tauri build 绝不能拿着陈旧 sidecar 打包
// （10-06 / 10-08 两次「安装包内嵌旧二进制」交付事故的根治）。
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, ".."); // apps/desktop-tauri
const repoRoot = path.resolve(appDir, "..", "..");
const srcTauri = path.join(appDir, "src-tauri");
// 单 workspace 后唯一产物目录（仓库根 target/），无任何 --target-dir/env 覆盖。
const target = path.join(repoRoot, "target");

function sh(cmd, args) {
  console.log(`[ensure-sidecar] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd: repoRoot, shell: false });
  if (r.error) {
    console.error(`[ensure-sidecar] ✗ 无法启动 ${cmd}:`, r.error);
    process.exit(1);
  }
  if (r.status !== 0) {
    console.error(
      `[ensure-sidecar] ✗ ${cmd} ${args.join(" ")} 失败（退出码 ${r.status}），打包中止。`,
    );
    process.exit(r.status ?? 1);
  }
}

// 拷贝 + sha256 字节校验：copyFileSync 成功不代表内容一致（进程被杀 /
// 磁盘错误的尾部截断都可能留半个文件），校验不过宁可断在这里。
function refresh(binName, destName) {
  const src = path.join(target, "release", binName);
  const dst = path.join(srcTauri, "binaries", destName);
  if (!existsSync(src)) {
    console.error(`[ensure-sidecar] ✗ 产物缺失: ${src}（编译声称成功却没有产物，环境异常）`);
    process.exit(1);
  }
  copyFileSync(src, dst);
  const a = createHash("sha256").update(readFileSync(src)).digest("hex");
  const b = createHash("sha256").update(readFileSync(dst)).digest("hex");
  if (a !== b) {
    console.error(`[ensure-sidecar] ✗ copy 校验失败（源/目标字节不一致）：${dst}`);
    process.exit(1);
  }
  console.log(`[ensure-sidecar] ✓ ${binName} -> ${dst} (sha256 ${a.slice(0, 12)}…)`);
}

sh("cargo", ["build", "--release", "-p", "hermix-server", "--bin", "hermix"]);
refresh("hermix.exe", "hermix-x86_64-pc-windows-msvc.exe");

// hermix-indexer 是独立 bin crate（无 tauri-build），先于壳编译产出，
// 不再有「build.rs 校验 binaries/ 里尚未编出的 usn-indexer」的 bootstrap 死锁。
sh("cargo", ["build", "--release", "-p", "hermix-indexer", "--bin", "usn-indexer"]);
refresh("usn-indexer.exe", "usn-indexer-x86_64-pc-windows-msvc.exe");

console.log("[ensure-sidecar] sidecar 已刷新并逐字节校验。");
