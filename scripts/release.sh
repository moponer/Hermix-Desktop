#!/usr/bin/env bash
# release.sh — 桌面端发布链：每次构建出双产物（安装包 exe + 更新包 zip）。
#
# 决策记录：
#   docs/notes/implemented/feature/2026-10-09-online-update.md    （A 路：更新包）
#   docs/notes/implemented/feature/2026-10-09-custom-installer.md （B 路：安装包，NSIS 退场）
#
# 流程：构建（前端 + ensure-sidecar + 主程序 + 安装器）→ 自组装 app 根 staging
# → A 路：latest.zip + manifest.json + releases.json → 上传到远端**版本目录**
#   （deploy/desktop/<N+1>/，N = current 指向的编号），服务端对账 sha256 后
#   **单次 rename 原子切换** current 符号链接——客户端请求要么看到完整旧版
#   要么完整新版，不存在「manifest 先落地/zip 传一半」的窗口期。首次运行
#   自动把旧 public/desktop/ 迁入 deploy/desktop/1/（幂等）。
#   （决策笔记 2026-10-11-relay-structured-deployment.md §3/§6）
# → B 路：zip 嵌入安装器 exe → Hermix-Setup-<ver>.exe。
#
# 远端布局（relayRoot = /www/wwwroot/hermix-relay）：
#   deploy/desktop/<N>/{latest.zip,manifest.json,releases.json}
#   deploy/desktop/current -> <N>     # serve 只认 current；回滚 = 链接指回
#
# 用法：
#   scripts/release.sh [--host root@1.2.3.4] [--notes "..."] [--min-version 0.1.9]
#     [--remote-dir ...] [--skip-build] [--skip-upload]
#     [--nsis-baseline <setup.exe>]   # 退场对照：与 NSIS 基准产物比文件清单
#
# 更新源：默认 https://hermix.cssso.com/desktop（与客户端内置默认一致），
# 可用 HERMIX_UPDATE_BASE_URL 覆写。
#
# 依赖：node/npm、cargo、powershell（Windows 自带）。NSIS / 7-Zip 不再需要。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAURI_CONF="$REPO_ROOT/apps/desktop-tauri/src-tauri/tauri.conf.json"
TARGET="$REPO_ROOT/target/release"
STAGING="$TARGET/update-staging"
OUT_DIR="$TARGET/dist"
CHANGELOG="$REPO_ROOT/CHANGELOG.md"

NOTES="" MIN_VERSION="" HOST="" REMOTE_DIR="/www/wwwroot/hermix-relay/deploy/desktop"
NSIS_BASELINE="" SKIP_BUILD=0 SKIP_UPLOAD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --notes) NOTES="$2"; shift 2 ;;
    --min-version) MIN_VERSION="$2"; shift 2 ;;
    --host) HOST="$2"; shift 2 ;;
    --remote-dir) REMOTE_DIR="$2"; shift 2 ;;
    --nsis-baseline) NSIS_BASELINE="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --skip-upload) SKIP_UPLOAD=1; shift ;;
    *) echo "未知参数: $1"; exit 1 ;;
  esac
done

BASE_URL="${HERMIX_UPDATE_BASE_URL:-https://hermix.cssso.com/desktop}"
BASE_URL="${BASE_URL%/}"

# ── 版本号：单一真相源 = tauri.conf.json ─────────────────────────
# 用 sed 而非 node require：Git Bash 的 POSIX 路径（/f/...）Windows node
# 的 require() 不认，MODULE_NOT_FOUND 直接崩（双击运行表现为窗口秒退）。
VERSION="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TAURI_CONF" | head -1)"
PRODUCT_NAME="$(sed -n 's/^[[:space:]]*"productName"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TAURI_CONF" | head -1)"
[[ -n "$VERSION" && -n "$PRODUCT_NAME" ]] || { echo "错误：无法从 $TAURI_CONF 解析 version/productName"; exit 1; }
echo "── 版本: $VERSION（$PRODUCT_NAME）"

# ── 构建 ────────────────────────────────────────────────────────
if [[ $SKIP_BUILD -eq 0 ]]; then
  echo "── 构建：前端 + ensure-sidecar（dev.mjs 全链路）"
  (cd "$REPO_ROOT/apps/desktop-tauri" && npm run build)
  echo "── 构建：主程序 release（custom-protocol：嵌入前端资产，缺它窗口导航 devUrl = localhost 拒绝连接）"
  (cd "$REPO_ROOT" && cargo build --release --features hermix-desktop/custom-protocol -p hermix-desktop)
  echo "── 构建：安装器（vite 前端 → cargo，版本经 env 注入）"
  (cd "$REPO_ROOT/apps/installer" && npm install --no-audit --no-fund && npm run build:vite)
  (cd "$REPO_ROOT" && HERMIX_PRODUCT_VERSION="$VERSION" cargo build --release -p hermix-installer)
else
  echo "── 跳过构建（--skip-build），沿用现有产物"
fi

# ── 自组装 app 根（NSIS 退场：不再解安装器，直接按 bundle 清单拼装）─────
# 主 exe 名与 NSIS 时代一致（productName.exe）——更新交换脚本按旧 exe 名拉起
# 新版，改名 = 交换后启动失败回滚，永远不许在 zip 里改主 exe 名。
MAIN_EXE_NAME="$PRODUCT_NAME.exe"
SRC_MAIN="$TARGET/hermix-desktop.exe"
[[ -f "$SRC_MAIN" ]] || SRC_MAIN="$TARGET/$MAIN_EXE_NAME"
[[ -f "$SRC_MAIN" ]] || { echo "错误：找不到主程序 exe（$TARGET/hermix-desktop.exe 或 $TARGET/$MAIN_EXE_NAME）"; exit 1; }

APP="$STAGING/app"
rm -rf "$STAGING"; mkdir -p "$APP"
cp "$SRC_MAIN" "$APP/$MAIN_EXE_NAME"

# sidecar 分两类，来源不同——这是事实，不是偏好。
#
#   本仓库构建（hermix / usn-indexer / hermix-viewer）
#     来源 = target/release/，即 cargo 刚编出来的那个文件。出包**不再经过
#     binaries/ 这一层副本**。此前这里写的是 $BINARIES/$s.exe，但目录里的实际
#     文件名带 target triple 后缀（<name>-x86_64-pc-windows-msvc.exe），该判断
#     恒为假、每轮都静默落到下面的 target/release 分支——与原注释"权威来源 =
#     binaries/"正好相反。两份副本一旦分叉，打包到哪一份取决于文件名猜没猜对，
#     而猜错不报错。直接取 target/release/，这个问题就不存在了。
#
#   项目资产 / 上游预编译（officecli / markitdown）
#     仓库里没有 crate、没有构建脚本（更新流程见 build.rs 注释），ensure-sidecar
#     也不碰它们。唯一的家就是 binaries/。
BINARIES="$REPO_ROOT/apps/desktop-tauri/src-tauri/binaries"

for s in hermix usn-indexer hermix-viewer; do
  f="$TARGET/$s.exe"
  [[ -f "$f" ]] || { echo "错误：缺 sidecar $f（先跑 npm run build / ensure-sidecar）"; exit 1; }

  # 新鲜度断言：产物必须晚于**它自己的**源码树里最新的 .rs。
  #
  # 为什么长在这里而不是只放在 check-sidecar-freshness.sh：那道门只被
  # ci-gate.sh 调用，而 release.sh 从不经过它——守门人不在出包路上等于没有，
  # 10-06 的「修了但新装的包里没有」就是这么漏的。这里是出包必经点，且
  # --skip-build 同样照跑。
  #
  # hermix 的扫描只看 crates/，且跳过 crates/hermix-indexer：
  #   跳过 indexer —— 否则改一下 indexer 就把 hermix 也误报成 STALE；
  #   不看 src-tauri/src —— 那是**壳**（hermix-desktop）的代码，不进 hermix-server，
  #     拿它做判定等于每次改壳都误报一次。误报多了这道门就没人当真了。
  case "$s" in
    hermix)
      stale=$(find "$REPO_ROOT/crates" \
                -type d -name hermix-indexer -prune -o \
                -type f -name '*.rs' -newer "$f" -print -quit 2>/dev/null) ;;
    usn-indexer)
      stale=$(find "$REPO_ROOT/crates/hermix-indexer" \
                -type f -name '*.rs' -newer "$f" -print -quit 2>/dev/null) ;;
    hermix-viewer)
      stale=$(find "$REPO_ROOT/apps/viewer" \
                -type f -name '*.rs' -newer "$f" -print -quit 2>/dev/null) ;;
  esac
  if [ -n "$stale" ]; then
    echo "错误：sidecar $s 比源码旧，拒绝出包（--skip-build 也不例外）"
    echo "       产物: $f"
    echo "       新于它的源码: ${stale#$REPO_ROOT/}"
    echo "       修复: cd apps/desktop-tauri && npm run ensure-sidecar"
    exit 1
  fi

  cp "$f" "$APP/$s.exe"
done

# 项目资产 / 上游预编译：唯一的家在 binaries/。**不参与上面的新鲜度判定**——
# 仓库里没有它们的源码，拿本仓库的 .rs 去比毫无意义，只会让这道门天天误报。
for s in officecli markitdown; do
  f="$BINARIES/$s-x86_64-pc-windows-msvc.exe"
  [[ -f "$f" ]] || { echo "错误：缺 sidecar $f"; exit 1; }
  cp "$f" "$APP/$s.exe"
done

# resources（tauri.conf bundle.resources）：src-tauri/resources 相对路径原样保留。
# 2016 个小文件用 robocopy /MT 多线程（cp 逐个过 Defender 实测过慢，曾致打包中断）；
# robocopy 退出码 0-7 均为成功（1=有文件被复制），>=8 才是真失败。
# 注意：必须在同一命令里短路吞掉成功码——set -e 会在 RC 赋值前就杀脚本。
RES_SRC="$REPO_ROOT/apps/desktop-tauri/src-tauri/resources"
[[ -d "$RES_SRC" ]] || { echo "错误：缺 resources 目录 $RES_SRC"; exit 1; }
robocopy "$(cygpath -w "$RES_SRC")" "$(cygpath -w "$APP/resources")" //E //MT:16 //NFL //NDL //NJH //NJS //NP || { RC=$?; [[ $RC -le 7 ]] || { echo "错误：robocopy 复制 resources 失败（退出码 $RC）"; exit 1; }; }

# 许可随包分发（MIT 要求带许可文本）+ 更新源自举。安装器许可页读的是
# 编译期内嵌副本（installer LICENSE.txt），这里拷同一份文件进 app 根，
# 便携包里永远能找到许可正文。不再用旧 license.rtf（Unicode 转义、与
# 内嵌文本无法对账）。
cp "$REPO_ROOT/apps/installer/src-tauri/LICENSE.txt" "$APP/LICENSE.txt"
printf '%s\n' "$BASE_URL/manifest.json" > "$APP/update-url.txt"

# 文件清单（对照与审计用）。
( cd "$APP" && find . -type f | sed 's|^\./||' | sort ) > "$STAGING/app.filelist.txt"
echo "── app 根组装完成：$(wc -l < "$STAGING/app.filelist.txt") 个文件"

# ── NSIS 退场对照（--nsis-baseline <setup.exe>）───────────────────
# 首次切换必须跑一次：自组装文件清单 ⊇ NSIS 安装清单（允许新增，不许缺失）。
if [[ -n "$NSIS_BASELINE" ]]; then
  echo "── NSIS 退场对照：$NSIS_BASELINE"
  NSIS_TMP="$STAGING/nsis-raw"; mkdir -p "$NSIS_TMP"
  powershell -NoProfile -Command "Expand-Archive -Path '$(cygpath -w "$NSIS_BASELINE")' -DestinationPath '$(cygpath -w "$NSIS_TMP")' -Force" \
    || { echo "错误：NSIS 基准不是 zip 结构（若是 exe，用 7z 解一遍再传目录）"; exit 1; }
  ( cd "$NSIS_TMP" && find . -type f | sed 's|^\./||' | sort ) > "$STAGING/nsis.filelist.txt"
  MISSING="$(comm -23 "$STAGING/nsis.filelist.txt" "$STAGING/app.filelist.txt" || true)"
  if [[ -n "$MISSING" ]]; then
    echo "错误：自组装缺以下文件（NSIS 有、app 根没有）——补齐组装清单后才能退场："
    echo "$MISSING"
    exit 1
  fi
  echo "── 对照通过：NSIS 清单全部被自组装覆盖（可安全删除 nsis target）"
fi

# ── A 路：更新包 latest.zip + sha256 ─────────────────────────────
echo "── A 路：打包 latest.zip"
ZIP="$STAGING/latest.zip"
rm -f "$ZIP"
# bsdtar（Windows 自带 tar.exe）按 .zip 扩展名自动选 zip 格式：比
# Compress-Archive 快一个量级、无 2GB 限制；-C 使包内路径以 app 根为基准。
(cd "$APP" && /c/Windows/System32/tar.exe -a -cf "$ZIP" .)
SHA256="$(sha256sum "$ZIP" | awk '{print $1}')"
echo "── sha256: $SHA256"

# ── 更新日志：CHANGELOG 顶部段 → --notes → 兜底 ──────────────────
if [[ -z "$NOTES" && -f "$CHANGELOG" ]]; then
  # 字面子串匹配（index），不能用 $0 ~ ver：ver 里的 [0.1.10] 方括号是
  # 正则元字符（字符类），`## [0.1.10]` 永远匹配不上字面标题，notes
  # 会静默回落默认文案（2026-10-11 出包实测踩中）。
  NOTES="$(awk -v ver="## [$VERSION]" '
     index($0, ver) == 1 {inblk=1; next}
     inblk && /^## / {exit}
     inblk {print}
   ' "$CHANGELOG")"
  [[ -n "$NOTES" ]] || echo "提示：CHANGELOG.md 无 [$VERSION] 段，回落 --notes / 默认文案"
fi
[[ -n "$NOTES" ]] || NOTES="本次更新包含修复与改进。"

# ── manifest.json ───────────────────────────────────────────────
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
NOTES_JSON="$(node -e "console.log(JSON.stringify(process.argv[1]))" "$NOTES")"
MIN_JSON="null"
[[ -n "$MIN_VERSION" ]] && MIN_JSON="\"$MIN_VERSION\""
cat > "$STAGING/manifest.json" <<EOF
{
  "version": "$VERSION",
  "notes": $NOTES_JSON,
  "pub_date": "$PUB_DATE",
  "url": "$BASE_URL/latest.zip",
  "sha256": "$SHA256",
  "min_version": $MIN_JSON
}
EOF
echo "── manifest.json:"
cat "$STAGING/manifest.json"

# ── releases.json：版本历史，单一真相源（决策笔记 §6）──────────────
# 当前版本（含 sha256/pub_date）+ CHANGELOG.md 历史段（最近 9 个）。
# products/desktop.js 从最新条目**现算** manifest，网页 changelog.html
# fetch 本文件渲染——升级包、更新弹窗、网页三处消费端读同一份，
# 「升级包说 A 网页说 B」在结构上不可能发生。
node -e '
const fs = require("fs");
const [version, sha256, pubDate, minJson, notesArg, changelogPath, outPath] = process.argv.slice(1);
const notes = notesArg.split("\n").map((s) => s.trim()).filter(Boolean);
const releases = [{ version, date: pubDate.slice(0, 10), notes, sha256, min_version: JSON.parse(minJson) }];
try {
  const lines = fs.readFileSync(changelogPath, "utf8").split(/\r?\n/);
  let cur = null;
  for (const line of lines) {
    const h = line.match(/^## \[([0-9][^\]]*)\]/);
    if (h) {
      if (cur && cur.version !== version && releases.length < 10) {
        releases.push({ version: cur.version, notes: cur.notes });
      }
      cur = h[1] === version ? null : { version: h[1], notes: [] };
      continue;
    }
    if (cur) {
      const b = line.match(/^\s*[-*]\s+(.*)/);
      if (b) cur.notes.push(b[1].trim());
    }
  }
  if (cur && cur.version !== version && releases.length < 10) {
    releases.push({ version: cur.version, notes: cur.notes });
  }
} catch {}
fs.writeFileSync(outPath, JSON.stringify(releases, null, 2) + "\n");
' "$VERSION" "$SHA256" "$PUB_DATE" "$MIN_JSON" "$NOTES" "$CHANGELOG" "$STAGING/releases.json"
echo "── releases.json: $(node -e "console.log(JSON.parse(require('fs').readFileSync(process.argv[1],'utf8')).length)" "$STAGING/releases.json") 个版本条目"

# ── B 路：安装包（载荷嵌入安装器 exe）────────────────────────────
echo "── B 路：载荷嵌入安装器"
INSTALLER_EXE="$TARGET/hermix-installer.exe"
[[ -f "$INSTALLER_EXE" ]] || { echo "错误：找不到安装器 $INSTALLER_EXE"; exit 1; }
mkdir -p "$OUT_DIR"
SETUP_OUT="$OUT_DIR/Hermix-Setup-$VERSION.exe"
cp "$INSTALLER_EXE" "$SETUP_OUT"
"$TARGET/hermix-embed.exe" embed "$SETUP_OUT" "$ZIP"
echo "── 安装包: $SETUP_OUT"

# ── 上传（A 路：版本目录 + 原子切换，决策笔记 §3）─────────────────
if [[ $SKIP_UPLOAD -eq 1 ]]; then
  echo "── 跳过上传（--skip-upload）。待传文件（面板上传用 [Win] 路径）："
  echo "   三个文件 → $REMOTE_DIR/<新版本号>/：latest.zip、manifest.json、releases.json"
  echo "   [Win] $(cygpath -w "$ZIP")"
  echo "   [Win] $(cygpath -w "$STAGING/manifest.json")"
  echo "   [Win] $(cygpath -w "$STAGING/releases.json")"
  echo "   传完后在服务器上原子切换："
  echo "     cd $REMOTE_DIR && ln -sfn <新版本号> .current.tmp && mv -Tf .current.tmp current"
  echo "   安装包（人工分发）: $SETUP_OUT"
  echo "   [Win] $(cygpath -w "$SETUP_OUT")"
  exit 0
fi
[[ -n "$HOST" ]] || { echo "错误：上传需要 --host user@server（或 --skip-upload）"; exit 1; }

# 首次运行迁移（幂等）：旧布局 <relay根>/public/desktop/{latest.zip,manifest.json}
# → deploy/desktop/1/ + current 链接。已有 current 就直接读编号，不重复迁移。
# 客户端 URL（/desktop/latest.zip）不变——relay 由 deploy/desktop/current serve。
RELAY_ROOT="${REMOTE_DIR%/*/*}"
CUR_N="$(ssh "$HOST" "set -e
  mkdir -p '$REMOTE_DIR'
  if [ -L '$REMOTE_DIR/current' ]; then
    readlink '$REMOTE_DIR/current'
  else
    mkdir -p '$REMOTE_DIR/1'
    if [ -f '$RELAY_ROOT/public/desktop/latest.zip' ]; then
      cp -f '$RELAY_ROOT/public/desktop/latest.zip' '$RELAY_ROOT/public/desktop/manifest.json' '$REMOTE_DIR/1/' 2>/dev/null || true
    fi
    ln -sfn 1 '$REMOTE_DIR/.current.tmp' && mv -Tf '$REMOTE_DIR/.current.tmp' '$REMOTE_DIR/current'
    echo 1
  fi")"
CUR_N="$((CUR_N + 0))"
NEXT_N=$((CUR_N + 1))
echo "── 上传到 $HOST:$REMOTE_DIR/$NEXT_N/（current -> $CUR_N）"
ssh "$HOST" "mkdir -p '$REMOTE_DIR/$NEXT_N'"
scp "$ZIP" "$HOST:$REMOTE_DIR/$NEXT_N/latest.zip"
scp "$STAGING/manifest.json" "$STAGING/releases.json" "$HOST:$REMOTE_DIR/$NEXT_N/"

# 服务端对账 + 原子切换。对账失败 = 传输坏批：版本目录留在盘上但 current
# 不动，客户端继续消费旧版——坏载荷永远到不了用户。
# 切换 = 单次 rename（mv -T）：客户端请求要么看到完整旧版要么完整新版，
# 「manifest 先落地 / zip 传一半」的窗口期在结构上不存在。
ssh "$HOST" "set -e
  REMOTE_SHA=\$(sha256sum '$REMOTE_DIR/$NEXT_N/latest.zip' | awk '{print \$1}')
  if [ \"\$REMOTE_SHA\" != '$SHA256' ]; then
    echo \"错误：上传的 zip sha256 对账失败（远端=\$REMOTE_SHA 预期=$SHA256），放弃切换\" >&2
    exit 1
  fi
  ln -sfn '$NEXT_N' '$REMOTE_DIR/.current.tmp' && mv -Tf '$REMOTE_DIR/.current.tmp' '$REMOTE_DIR/current'
  # 只保留 current 与上一版（回滚窗口），更早的版本目录清掉。
  cd '$REMOTE_DIR'
  for d in [0-9]*; do
    if [ -d \"\$d\" ] && [ \"\$d\" -ne '$NEXT_N' ] && [ \"\$d\" -ne '$CUR_N' ]; then rm -rf \"\$d\"; fi
  done
"
echo "── 完成。发版 $VERSION 已上线（current -> $NEXT_N）；上一版 $CUR_N 保留可回滚（ln -sfn $CUR_N current）；安装包: $SETUP_OUT"
