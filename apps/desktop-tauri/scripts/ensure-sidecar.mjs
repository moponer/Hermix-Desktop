// ensure-sidecar.mjs — 打包前置：编译并刷新 sidecar 二进制进 binaries/。
//
// 由 tauri.conf.json 的 beforeBuildCommand 经 `npm run ensure-sidecar` 调起：
// npm run 把脚本 cwd 固定到 package.json 所在目录（apps/desktop-tauri），
// 与 Tauri CLI 的调用 cwd 解耦；脚本内全部路径用本文件位置推算，不依赖 cwd。
//
// 任一步失败 exit 1 —— cargo tauri build 绝不能拿着陈旧 sidecar 打包
// （10-06 / 10-08 两次「安装包内嵌旧二进制」交付事故的根治）。
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, readFileSync, utimesSync } from "node:fs";
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

/**
 * 清理占用 binaries 目录 sidecar 的残留进程（2026-10-25：用户关窗
 * [只是隐藏到托盘] 后编译，hermix.exe / hermix-filesearch.exe 残留 →
 * 拷贝 EBUSY，编译失败）。
 *
 * 按可执行路径**精确**匹配（只杀从 binaries/ 或安装目录跑的），不碰
 * dev 实例（target/debug）与用户在跑的其它程序：
 * - hermix-filesearch.exe：写停止请求文件（新版 daemon 有看门狗，10s 内
 *   刷盘退出；提权进程 taskkill 杀不动，请求它自己走是唯一正道）。
 * - hermix.exe / hermix-viewer.exe（binaries 路径）：taskkill。
 */
function clearSidecarOccupants() {
  const marker = path.join(process.env.LOCALAPPDATA || "", "hermix", "filesearch");
  try {
    // filesearch：写停止请求（daemon 看门狗认时间戳，见 daemon.rs）。
    const req = path.join(marker, "stop-request.json");
    const body = JSON.stringify({
      requestedAt: Math.floor(Date.now() / 1000),
    });
    require("node:fs").writeFileSync(req, body);
    console.log("[ensure-sidecar] 已写 filesearch 停止请求。");
  } catch {
    /* 写不了就算了——后面还有 taskkill 兜底 */
  }
  // 找出 exe 路径在 srcTauri/binaries（或安装目录旁）的 sidecar 进程击杀。
  const binaries = path.join(srcTauri, "binaries").toLowerCase();
  const ps = spawnSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `Get-CimInstance Win32_Process | Where-Object { ` +
        `$_.Name -in @('hermix.exe','hermix-viewer.exe','hermix-filesearch.exe') -and ` +
        `$_.ExecutablePath -and $_.ExecutablePath.ToLower().StartsWith('${binaries.slice(0, -1)}') } ` +
        `| ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; Write-Output $_.ProcessId }`,
    ],
    { encoding: "utf8", timeout: 30_000 },
  );
  const killed = (ps.stdout || "").trim();
  console.log(killed ? `[ensure-sidecar] 已结束残留进程: ${killed}` : "[ensure-sidecar] 未发现 binaries 路径的残留进程。");
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
  let copied = false;
  for (let attempt = 1; attempt <= 3 && !copied; attempt++) {
    try {
      copyFileSync(src, dst);
      copied = true;
    } catch (e) {
      if (e && (e.code === "EBUSY" || e.code === "EPERM")) {
        if (attempt === 1) {
          console.log(
            `[ensure-sidecar] ${destName} 被占用（${e.code}）——自动清理残留 sidecar 进程后重试…`,
          );
          clearSidecarOccupants();
        } else {
          console.log(`[ensure-sidecar] 重试拷贝（第 ${attempt}/3 次）…`);
        }
        // 清理/等待后稍停，让进程退出与文件句柄释放完成。
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2000);
        continue;
      }
      throw e;
    }
  }
  if (!copied) {
    console.error(
      `[ensure-sidecar] ✗ ${destName} 拷贝 3 次均被占用——自动清理无效。
` +
      `[ensure-sidecar]   手动处理：托盘图标右键退出桌面端（点 X 只是隐藏到托盘！），` +
      `或任务管理器结束 hermix / hermix-filesearch / hermix-viewer 进程后重新打包。`,
    );
    process.exit(1);
  }
  // copyFileSync 会**保留源文件的时间戳**（Windows CopyFileEx 的行为），
  // 于是"刚拷贝过来"的 sidecar 可能带着几个月前的时间戳，
  // check-sidecar-freshness.sh 的 mtime 比较就会误判 STALE。
  // 拷贝完成 + sha256 逐字节校验通过后，把落地文件的 mtime 打到"现在" ——
  // 此刻 cargo 刚刚按当前源码重新构建过，内容与当前树一致，
  // 所以"现在"就是名副其实的"这份 sidecar 是新的"。
  const now = new Date();
  utimesSync(dst, now, now);
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

// dsh 转换引擎**不再**构建期捆绑：安装包不带这 190MB，改由运行期首次
// Office→PDF / PDF→图片时按需下载到 <hermix home>/dsh-engine（见
// crates/hermix-tools/src/tools/dsh_engine_fetch.rs）。
// fetch-dsh-engine.mjs 保留给开发/打包机本地取引擎做 smoke 验证（产物落在
// src-tauri/resources/dsh-engine，用 HERMIX_DSH_ENGINE 指过去即可），
// 不进安装包、不进更新 zip，也不再由本脚本调用。

console.log("[ensure-sidecar] sidecar 已刷新并逐字节校验。");
