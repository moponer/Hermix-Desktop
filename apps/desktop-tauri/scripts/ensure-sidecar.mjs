// ensure-sidecar.mjs — 打包前置：编译并刷新 sidecar 二进制进 binaries/。
//
// 由 tauri.conf.json 的 beforeBuildCommand 经 `npm run ensure-sidecar` 调起：
// npm run 把脚本 cwd 固定到 package.json 所在目录（apps/desktop-tauri），
// 与 Tauri CLI 的调用 cwd 解耦；脚本内全部路径用本文件位置推算，不依赖 cwd。
//
// 任一步失败 exit 1 —— cargo tauri build 绝不能拿着陈旧 sidecar 打包
// （10-06 / 10-08 两次「安装包内嵌旧二进制」交付事故的根治）。
//
// ⚠️ 打包前必须先停掉正在运行的 `hermix.exe` / `hermix-viewer.exe`：
// 它们占着 `target/release/<name>.exe`，cargo 无法替换正在运行的 exe ——
// 会报 `failed to remove file ... 拒绝访问 (os error 5)` 并中止打包
// （2026-10-22 踩了两次）。要边跑边打包，就把 sidecar / viewer 从**副本**启动。
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, ".."); // apps/desktop-tauri
const repoRoot = path.resolve(appDir, "..", "..");
const srcTauri = path.join(appDir, "src-tauri");
// 产物目录：默认仓库根 target/；设了 CARGO_TARGET_DIR 环境变量则优先用（与 cargo 一致，
// env 为绝对路径，避免 tauri build 子进程按 cwd 解析分裂两处）。release.sh /
// verify-stability-suite.py 同样读此变量，保证"取产物的地方"与"cargo 写产物的地方"一致。
const target = process.env.CARGO_TARGET_DIR
  ? path.join(process.env.CARGO_TARGET_DIR)
  : path.join(repoRoot, "target");

function sh(cmd, args, cwd = repoRoot) {
  console.log(`[ensure-sidecar] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd, shell: false });
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

// viewer（第 5 个 sidecar）：先 vite 再 cargo——generate_context! 编译期把
// apps/viewer/dist 嵌入 exe，顺序反了会嵌入旧 UI（与 sidecar 过期同类事故）。
const viewerVite = path.join(repoRoot, "apps", "viewer", "node_modules", "vite", "bin", "vite.js");
if (!existsSync(viewerVite)) {
  console.error("[ensure-sidecar] ✗ apps/viewer 依赖未安装（先在 apps/viewer 跑 npm install）");
  process.exit(1);
}
sh("node", [viewerVite, "build"], path.join(repoRoot, "apps", "viewer"));
sh("cargo", ["build", "--release", "-p", "hermix-viewer"]);
refresh("hermix-viewer.exe", "hermix-viewer-x86_64-pc-windows-msvc.exe");

// filesearch helper（第 6 个 sidecar）：本地文件索引服务。提权守护进程
// （MFT 建索引要管理员），跨提权边界继承不了 stdio → 走 127.0.0.1 回环 +
// token 握手。viewer's 路径栏模糊搜索（类 Everything）与 desktop 的
// Alt+F 文件搜索共用它；不随包发它，两边的全盘搜索就都是死的。
sh("cargo", ["build", "--release", "-p", "hermix-filesearch", "--bin", "hermix-filesearch"]);
refresh("hermix-filesearch.exe", "hermix-filesearch-x86_64-pc-windows-msvc.exe");

// desktop-tauri 前端：vite 构建到 release/frontend（与 tauri.conf.json frontendDist 一致），
// 必须在 cargo tauri build 之前完成（tauri bundler 直接读 frontendDist）。
const desktopVite = path.join(appDir, "node_modules", "vite", "bin", "vite.js");
if (!existsSync(desktopVite)) {
  console.error("[ensure-sidecar] ✗ apps/desktop-tauri 依赖未安装");
  process.exit(1);
}
sh("node", [desktopVite, "build"], appDir);

// dsh 转换引擎（Office→PDF，构建期捆绑，运行期完全离线）：幂等，
// 已就位且版本一致时秒过。见 scripts/fetch-dsh-engine.mjs 头注。
sh("node", [path.join(__dirname, "fetch-dsh-engine.mjs")]);

console.log("[ensure-sidecar] sidecar 已刷新并逐字节校验。");
