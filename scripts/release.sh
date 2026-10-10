#!/usr/bin/env bash
# release.sh — 桌面端发布链：每次构建出双产物（安装包 exe + 更新包 zip）。
#
# 决策记录：
#   docs/notes/implemented/feature/2026-10-09-online-update.md    （A 路：更新包）
#   docs/notes/implemented/feature/2026-10-09-custom-installer.md （B 路：安装包，NSIS 退场）
#
# 流程：构建（前端 + ensure-sidecar + 主程序 + 安装器）→ 自组装 app 根 staging
# → A 路：latest.zip + sha256 + manifest.json → relay；B 路：zip 嵌入安装器 exe
# → Hermix-Setup-<ver>.exe。
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

NOTES="" MIN_VERSION="" HOST="" REMOTE_DIR="/www/wwwroot/hermix-relay/public/desktop"
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
  echo "── 构建：主程序 release"
  (cd "$REPO_ROOT" && cargo build --release -p hermix-desktop)
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

# sidecar（externalBin 五件套）：权威来源 = src-tauri/binaries/（ensure-sidecar
# 落点 + sha256 校验点，tauri.conf externalBin 所指）；target/release 仅作
# hermix/usn-indexer/hermix-viewer 的回退（workspace 直编产物）。禁止绕过
# ensure-sidecar。
BINARIES="$REPO_ROOT/apps/desktop-tauri/src-tauri/binaries"
for s in hermix officecli markitdown usn-indexer hermix-viewer; do
  f=""
  [[ -f "$BINARIES/$s.exe" ]] && f="$BINARIES/$s.exe"
  [[ -z "$f" && -f "$TARGET/$s.exe" ]] && f="$TARGET/$s.exe"
  [[ -n "$f" ]] || { echo "错误：缺 sidecar $s.exe（先跑 npm run build / ensure-sidecar）"; exit 1; }
  cp "$f" "$APP/$s.exe"
done

# resources（tauri.conf bundle.resources）：src-tauri/resources 相对路径原样保留。
# 2016 个小文件用 robocopy /MT 多线程（cp 逐个过 Defender 实测过慢，曾致打包中断）；
# robocopy 退出码 0-7 均为成功（1=有文件被复制），>=8 才是真失败。
# 注意：必须在同一命令里短路吞掉成功码——set -e 会在 RC 赋值前就杀脚本。
RES_SRC="$REPO_ROOT/apps/desktop-tauri/src-tauri/resources"
[[ -d "$RES_SRC" ]] || { echo "错误：缺 resources 目录 $RES_SRC"; exit 1; }
robocopy "$(cygpath -w "$RES_SRC")" "$(cygpath -w "$APP/resources")" //E //MT:16 //NFL //NDL //NJH //NJS //NP || { RC=$?; [[ $RC -le 7 ]] || { echo "错误：robocopy 复制 resources 失败（退出码 $RC）"; exit 1; }; }

# 许可与更新源自举（安装器 license 命令、更新后客户端检查都要）。
cp "$REPO_ROOT/apps/desktop-tauri/src-tauri/license.rtf" "$APP/license.rtf"
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
  NOTES="$(awk -v ver="## [$VERSION]" '
    $0 ~ ver {inblk=1; next}
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

# ── B 路：安装包（载荷嵌入安装器 exe）────────────────────────────
echo "── B 路：载荷嵌入安装器"
INSTALLER_EXE="$TARGET/hermix-installer.exe"
[[ -f "$INSTALLER_EXE" ]] || { echo "错误：找不到安装器 $INSTALLER_EXE"; exit 1; }
mkdir -p "$OUT_DIR"
SETUP_OUT="$OUT_DIR/Hermix-Setup-$VERSION.exe"
cp "$INSTALLER_EXE" "$SETUP_OUT"
"$TARGET/hermix-embed.exe" embed "$SETUP_OUT" "$ZIP"
echo "── 安装包: $SETUP_OUT"

# ── 上传（A 路；安装包走官网/网盘人工分发）────────────────────────
if [[ $SKIP_UPLOAD -eq 1 ]]; then
  echo "── 跳过上传（--skip-upload）。待传文件（面板上传用 [Win] 路径）："
  echo "   $ZIP → $REMOTE_DIR/latest.zip"
  echo "   $STAGING/manifest.json → $REMOTE_DIR/manifest.json"
  echo "   [Win] $(cygpath -w "$ZIP")"
  echo "   [Win] $(cygpath -w "$STAGING/manifest.json")"
  echo "   安装包（人工分发）: $SETUP_OUT"
  echo "   [Win] $(cygpath -w "$SETUP_OUT")"
  exit 0
fi
[[ -n "$HOST" ]] || { echo "错误：上传需要 --host user@server（或 --skip-upload）"; exit 1; }
echo "── 上传到 $HOST:$REMOTE_DIR"
ssh "$HOST" "mkdir -p '$REMOTE_DIR'"
# manifest 最后传：它一落地客户端就能看到新版本，此时 zip 已就位。
scp "$ZIP" "$HOST:$REMOTE_DIR/latest.zip"
scp "$STAGING/manifest.json" "$HOST:$REMOTE_DIR/manifest.json"
echo "── 完成。发版 $VERSION 已上线；安装包: $SETUP_OUT"
