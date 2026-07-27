# download-esbuild.ps1
#
# Fetches the Windows x64 esbuild binary used by the custom-app packager
# (crates/hermix-plugin -> EsbuildPackager) into the Tauri resources dir so
# it gets bundled into the NSIS package. The binary is intentionally NOT
# committed to git (keeps the remote repo small); this script is the only
# source of it.
#
# The file must land at:
#   apps/desktop-tauri/src-tauri/resources/esbuild/esbuild.exe
# (expected by commands_custom_app::resolve_esbuild_path + ensure_app_shell_and_esbuild)
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/download-esbuild.ps1 [-Required]
#   -Required : exit non-zero if the binary cannot be fetched (use for builds).
#               Omit for dev (warn-only, so offline dev still starts).

[CmdletBinding()]
param(
    [switch]$Required
)

$ErrorActionPreference = 'Continue'

# scripts/ -> repo root -> tauri resources/esbuild
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$esbuildDir = Join-Path $repoRoot ([System.IO.Path]::Combine('apps', 'desktop-tauri', 'src-tauri', 'resources', 'esbuild'))
$dest = Join-Path $esbuildDir 'esbuild.exe'

# Pinned esbuild version. The packager only calls `esbuild --bundle`, which
# is stable across minor versions; bump this when a new feature is needed.
$esbuildVersion = '0.21.5'
# Official GitHub release asset (same CDN as the OCR models download script,
# so the --ssl-no-revoke workaround for Windows schannel applies identically).
$zipUrl = "https://github.com/evanw/esbuild/releases/download/v$esbuildVersion/esbuild-windows-64.zip"

if (-not (Test-Path $esbuildDir)) {
    New-Item -ItemType Directory -Force -Path $esbuildDir | Out-Null
}

if (Test-Path $dest) {
    Write-Host "[esbuild] skip (already present): $dest"
    exit 0
}

Write-Host "[esbuild] downloading esbuild $esbuildVersion (windows-64) ..."

$tmpDir = Join-Path $env:TEMP ("hermix-esbuild-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null
$zip = Join-Path $tmpDir 'esbuild-windows-64.zip'

# --ssl-no-revoke: Windows schannel otherwise fails with
# CRYPT_E_REVOCATION_OFFLINE on the GitHub release CDN.
& curl.exe --ssl-no-revoke -L -o $zip $zipUrl
if ($LASTEXITCODE -ne 0) {
    Write-Warning "[esbuild] FAILED to download zip (curl exit $LASTEXITCODE)"
    Remove-Item $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    if ($Required) { exit 1 } else { exit 0 }
}

# Extract and locate esbuild.exe (top-level entry inside the zip).
Expand-Archive -Path $zip -DestinationPath $tmpDir -Force
$extracted = Get-ChildItem -Path $tmpDir -Recurse -Filter 'esbuild.exe' | Select-Object -First 1

if ($null -eq $extracted) {
    Write-Warning "[esbuild] esbuild.exe not found inside the zip."
    Remove-Item $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    if ($Required) { exit 1 } else { exit 0 }
}

Copy-Item -Path $extracted.FullName -Destination $dest -Force
Remove-Item $tmpDir -Recurse -Force -ErrorAction SilentlyContinue

if (-not (Test-Path $dest)) {
    Write-Warning "[esbuild] copy failed."
    if ($Required) { exit 1 } else { exit 0 }
}

$size = (Get-Item $dest).Length
Write-Host "[esbuild]   -> $dest ($size bytes)"

exit 0
