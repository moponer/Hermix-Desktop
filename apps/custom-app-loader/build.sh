#!/usr/bin/env bash
# 打包 word2pdf 自定义应用: 内联 convert.py -> 构建 loader -> 注入成单文件 exe
set -euo pipefail
cd "$(dirname "$0")"

APP_DIR="apps/word2pdf"

# 1) 内联 convert.py 到 index.html 占位符 __CONVERT_PY__
PY="$(command -v python3 || command -v python || echo "C:/Python313/python.exe")"
"$PY" - "$APP_DIR" <<'PYEOF'
import sys, pathlib
app = pathlib.Path(sys.argv[1])
py_src = (app / "convert.py").read_text(encoding="utf-8")
tpl = (app / "index.template.html").read_text(encoding="utf-8")
assert "__CONVERT_PY__" in tpl, "模板缺少 __CONVERT_PY__ 占位符"
html = tpl.replace("__CONVERT_PY__", py_src)
(app / "index.html").write_text(html, encoding="utf-8")
print("✅ index.html 已生成 (内联 convert.py %d 字节)" % len(py_src))
PYEOF

# 2) 构建 loader (需要 MSVC 环境)
. ../../cargo-env.sh
cargo build --release

# 3) 注入打包成单文件 exe
LOADER=./target/release/hermix-app-loader.exe
INJECT=./target/release/inject_app.exe
[ -f "$LOADER" ] || { echo "❌ loader 未构建: $LOADER"; exit 1; }
"$INJECT" "$LOADER" "$APP_DIR/index.html" "$APP_DIR/manifest.json" "$APP_DIR/word2pdf.exe"
echo "✅ 单文件应用已生成: $APP_DIR/word2pdf.exe"
