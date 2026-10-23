// fetch-dsh-engine.mjs — 给**开发/打包机**取一份 dsh-libreoffice-kit 引擎做 smoke 验证。
//
// 产品的引擎获取在运行期：首次 Office→PDF / PDF→图片时由
// crates/hermix-tools/src/tools/dsh_engine_fetch.rs 下载到 <hermix home>/dsh-engine。
// 本脚本**不再由构建链调用**（ensure-sidecar 已摘除），产物落在
// src-tauri/resources/dsh-engine/package，被 tauri.conf 与 release.sh 显式排除，
// 不进安装包、不进更新 zip —— 用 HERMIX_DSH_ENGINE 指向它跑 dsh_engine_* 测试。
//
// 引擎来源与许可：deepseek-ai/dsh-libreoffice-kit（MPL-2.0），预裁剪 LibreOffice
// 引擎（禁 Java/Python/Base/安装器等，保留 Writer/Calc/Impress + PDF 导出），
// 其 Release 归档明确定位 "for application builders to prepare and bundle"。
// pin 升级：改 VERSION + INTEGRITY（npm view <pkg>@<ver> dist.integrity）。
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcTauri = path.resolve(__dirname, "..", "src-tauri");
const destRoot = path.join(srcTauri, "resources", "dsh-engine");

const VERSION = "0.2.0";
// npm view @deepseek-ai/libreoffice-kit-win32-x64@0.2.0 dist.integrity
const INTEGRITY =
  "sha512-5Fscl181XAcgKDTanf1poOgJOi5hdKuuLV9KmCuTOvKuirty1+kgMNXIuVQm4Rnq0Yh1mz02x/ThnqrPFta4vQ==";
const TARBALL_NAME = `libreoffice-kit-win32-x64-${VERSION}.tgz`;
const MIRRORS = [
  `https://registry.npmmirror.com/@deepseek-ai/libreoffice-kit-win32-x64/-/${TARBALL_NAME}`,
  `https://registry.npmjs.org/@deepseek-ai/libreoffice-kit-win32-x64/-/${TARBALL_NAME}`,
];

const pkgDir = path.join(destRoot, "package");
const versionFile = path.join(pkgDir, ".engine-version");

// 幂等：已就位且版本一致 → 秒过（增量构建常态）。
if (
  existsSync(path.join(pkgDir, "bin", "dsoffice.exe")) &&
  existsSync(versionFile) &&
  readFileSync(versionFile, "utf8").trim() === VERSION
) {
  console.log(`[fetch-dsh-engine] ✓ 引擎已就位（v${VERSION}），跳过`);
  process.exit(0);
}

console.log(`[fetch-dsh-engine] 下载引擎 v${VERSION}（~71MB，npmmirror 优先）…`);
mkdirSync(destRoot, { recursive: true });
const tgzPath = path.join(destRoot, TARBALL_NAME);

let downloaded = false;
for (const url of MIRRORS) {
  console.log(`[fetch-dsh-engine] $ curl -L -o ${TARBALL_NAME} ${url}`);
  const r = spawnSync("curl.exe", ["-L", "--fail", "--silent", "--show-error", "-o", tgzPath, url], {
    stdio: "inherit",
  });
  if (r.status === 0) {
    downloaded = true;
    break;
  }
  console.error(`[fetch-dsh-engine] 镜像失败（退出码 ${r.status}），尝试下一个`);
}
if (!downloaded) {
  console.error("[fetch-dsh-engine] ✗ 所有镜像均失败，打包中止。");
  process.exit(1);
}

// sha512 校验（npm dist.integrity 是 base64 的 sha512）。
const buf = readFileSync(tgzPath);
const actual = "sha512-" + createHash("sha512").update(buf).digest("base64");
if (actual !== INTEGRITY) {
  console.error(`[fetch-dsh-engine] ✗ sha512 校验失败\n  期望 ${INTEGRITY}\n  实际 ${actual}`);
  process.exit(1);
}
console.log(`[fetch-dsh-engine] ✓ sha512 校验通过 (${(buf.length / 1024 / 1024).toFixed(1)}MB)`);

// 解压（Windows 10+ 自带 bsdtar，支持 tgz）。先清残留保证原子性。
rmSync(pkgDir, { recursive: true, force: true });
const r = spawnSync("tar", ["-xzf", tgzPath, "-C", destRoot], { stdio: "inherit" });
if (r.status !== 0) {
  console.error(`[fetch-dsh-engine] ✗ 解压失败（退出码 ${r.status}）`);
  process.exit(1);
}

// 解包完整性断言：dsoffice.exe + program/program（uno.ini 所在）+ 清单版本。
for (const must of [
  path.join(pkgDir, "bin", "dsoffice.exe"),
  path.join(pkgDir, "program", "program", "uno.ini"),
  path.join(pkgDir, "prebuilds.json"),
]) {
  if (!existsSync(must)) {
    console.error(`[fetch-dsh-engine] ✗ 引擎包不完整：缺 ${must}`);
    process.exit(1);
  }
}

rmSync(tgzPath);
writeFileSync(versionFile, VERSION);
console.log(`[fetch-dsh-engine] ✓ 引擎 v${VERSION} 已捆绑到 ${pkgDir}`);
