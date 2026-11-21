# 修复构建目录完整性级别 —— 解决 "拒绝访问 (os error 5)" / "EPERM"
#
# 症状
#   cargo / node / vite 等子进程报：拒绝访问 (os error 5)、EPERM、Access is denied，
#   但同一个目录用 PowerShell 自己写却成功。
#
# 原因
#   这些子进程运行在 Low 完整性级别（S-1-16-4096），而构建目录是 Medium。
#   Windows 的强制完整性控制（MIC）禁止「低完整性写高完整性」，于是：
#     - 创建文件失败  -> os error 5
#     - 删除/清空目录失败 -> EPERM
#
# 解法
#   只给「构建过程中真正需要写入」的目录设置 Low 完整性标签。Low 进程能写，
#   Medium 进程（你自己开的终端）也能写 —— 两种终端下都能构建。
#   源码目录保持 Medium，不要整体降级。
#
# 两个容易踩的点
#   1. 顺序：必须先拿到 FullControl（含 WRITE_OWNER），否则
#      icacls /setintegritylevel 会报 Access is denied。
#   2. 目录设 Low 只解决「新建文件」。要删除目录里已存在的旧文件，那些文件
#      本身也必须是 Low，否则清空产物目录时 EPERM。所以：
#        - 前端产物目录（dist / release/frontend）：清空重建，最简单
#        - cargo 产物目录（target / release/target）：用 /T 递归重设标签
#
# 用法
#   在自己的终端里直接运行（不需要管理员）。

$ErrorActionPreference = "Continue"
$root = "F:\spiker\hermix-desktop"
$me   = [Security.Principal.WindowsIdentity]::GetCurrent().Name

Write-Host "user: $me" -ForegroundColor Cyan
Write-Host "root: $root" -ForegroundColor Cyan
Write-Host ""

$ok = $true

function Set-Low {
    param([string]$Rel, [switch]$Wipe, [switch]$Recursive)

    $d = Join-Path $root $Rel

    if ($Wipe -and (Test-Path $d)) {
        try {
            Remove-Item $d -Recurse -Force -ErrorAction Stop
        } catch {
            Write-Host "  FAIL  $Rel  (清空失败: $($_.Exception.Message))" -ForegroundColor Red
            $script:ok = $false
            return
        }
    }

    if (-not (Test-Path $d)) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
    }

    icacls $d /grant ($me + ":(OI)(CI)F") 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL  $Rel  (授权失败)" -ForegroundColor Red
        $script:ok = $false
        return
    }

    if ($Recursive) {
        icacls $d /setintegritylevel "(OI)(CI)L" /T 2>&1 | Out-Null
    } else {
        icacls $d /setintegritylevel "(OI)(CI)L" 2>&1 | Out-Null
    }
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL  $Rel  (设标签失败)" -ForegroundColor Red
        $script:ok = $false
        return
    }

    Write-Host "  OK    $Rel" -ForegroundColor Green
}

Write-Host "=== cargo 产物目录（递归重设标签）===" -ForegroundColor Cyan
Set-Low "release" -Recursive
Set-Low "target"  -Recursive

Write-Host ""
Write-Host "=== 前端产物目录（清空重建）===" -ForegroundColor Cyan
Set-Low "release\frontend"    -Wipe
Set-Low "apps\viewer\dist"   -Wipe

Write-Host ""
Write-Host "=== 构建期会被写入的应用目录 ===" -ForegroundColor Cyan
# tauri-cli 构建时会重写 src-tauri 的 Cargo.toml，并往 src-tauri/binaries 拷 sidecar
Set-Low "apps\desktop-tauri\src-tauri" -Recursive
# vite 加载配置时会在 node_modules/.vite-temp 写临时文件、在 .vite 写缓存
Set-Low "apps\desktop-tauri\node_modules\.vite-temp"
Set-Low "apps\desktop-tauri\node_modules\.vite"
Set-Low "apps\viewer\node_modules\.vite-temp"
Set-Low "apps\viewer\node_modules\.vite"

Write-Host ""
Write-Host "=== 验证：Low 子进程能建/删文件吗 ===" -ForegroundColor Cyan

$py = "C:\Users\Administrator\.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\python\python.exe"
if (Test-Path $py) {
    $check = @(
        "release", "release\target", "release\frontend", "target",
        "apps\desktop-tauri\src-tauri",
        "apps\desktop-tauri\node_modules\.vite-temp",
        "apps\viewer\node_modules\.vite-temp",
        "apps\viewer\dist"
    )
    foreach ($rel in $check) {
        $d = Join-Path $root $rel
        if (-not (Test-Path $d)) { continue }
        $f = Join-Path $d "_il_probe.tmp"
        & $py -c "import os; open(r'$f','w').write('x'); os.remove(r'$f')" 2>&1 | Out-Null
        if (Test-Path $f) {
            Remove-Item $f -Force -ErrorAction SilentlyContinue
            Write-Host "  WRITE-FAIL  $rel" -ForegroundColor Red
            $ok = $false
        } else {
            Write-Host "  WRITE-OK    $rel" -ForegroundColor Green
        }
    }
} else {
    Write-Host "  (未找到 python，跳过子进程实测)" -ForegroundColor DarkGray
}

Write-Host ""
if (-not $ok) {
    Write-Host "有目录修复失败，见上面红色输出。" -ForegroundColor Red
    exit 1
}

Write-Host "修复完成。构建：" -ForegroundColor Green
Write-Host "  cd $root\apps\desktop-tauri"
Write-Host "  npm run build"
