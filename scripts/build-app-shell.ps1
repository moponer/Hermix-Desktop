# build-app-shell.ps1
#
# Builds the standalone custom-app shell (apps/custom-app-shell) and copies
# its release artifacts into the desktop Tauri app's resources dir so they
# get bundled into the NSIS package:
#   apps/custom-app-shell/src-tauri/target/release/app-shell.exe
#   apps/custom-app-shell/src-tauri/target/release/WebView2Loader.dll
#     -> apps/desktop-tauri/src-tauri/resources/shell/
#
# The shell binary is intentionally NOT committed to git (keeps the remote
# repo small); this script is what produces it. It is idempotent: if the
# target resources/shell/app-shell.exe already exists and is newer than the
# source, it is left alone.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-app-shell.ps1
# (invoked automatically by `pnpm build` via the prebuild:vite hook)

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot   = Resolve-Path (Join-Path $PSScriptRoot '..')
$shellProj  = Join-Path $repoRoot ([System.IO.Path]::Combine('apps', 'custom-app-shell', 'src-tauri'))
$outDir     = Join-Path $repoRoot ([System.IO.Path]::Combine('apps', 'desktop-tauri', 'src-tauri', 'resources', 'shell'))
$targetExe  = Join-Path $shellProj ([System.IO.Path]::Combine('target', 'release', 'app-shell.exe'))
$destExe    = Join-Path $outDir 'app-shell.exe'
$destLoader = Join-Path $outDir 'WebView2Loader.dll'

if (Test-Path $destExe) {
    $sz = (Get-Item $destExe).Length
    if ($sz -ge 1MB) {
        Write-Host "[app-shell] skip (already built, $sz bytes): $destExe"
        exit 0
    }
    Write-Warning "[app-shell] existing file is too small ($sz bytes) — rebuilding"
    Remove-Item $destExe -Force -ErrorAction SilentlyContinue
}

Write-Host "[app-shell] building custom-app-shell (release) ..."
Push-Location $shellProj
try {
    # NOTE: requires a Windows MSVC toolchain on PATH (vcvars/VS dev prompt).
    # Rust's bundled MSVC target uses link.exe from the VS install.
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Error "[app-shell] cargo build failed (exit $LASTEXITCODE)"
        exit 1
    }
} finally {
    Pop-Location
}

if (-not (Test-Path $targetExe)) {
    Write-Error "[app-shell] expected artifact not found: $targetExe"
    exit 1
}

if (-not (Test-Path $outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}

Copy-Item -Path $targetExe -Destination $destExe -Force

# WebView2Loader.dll ships next to app-shell.exe in the Tauri release output.
$srcLoader = Join-Path $shellProj ([System.IO.Path]::Combine('target', 'release', 'WebView2Loader.dll'))
if (Test-Path $srcLoader) {
    Copy-Item -Path $srcLoader -Destination $destLoader -Force
    Write-Host "[app-shell]   -> $destLoader"
} else {
    Write-Warning "[app-shell] WebView2Loader.dll not found at $srcLoader (shell may fail to launch without it)"
}

Write-Host "[app-shell]   -> $destExe"
exit 0
