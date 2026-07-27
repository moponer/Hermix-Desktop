# download-ocr-models.ps1
#
# Fetches the PP-OCRv5 mobile models used by the local OCR engine
# (crates/hermix-ocr -> oar-ocr) into the Tauri resources dir so they get
# bundled into the NSIS package. Models are intentionally NOT committed to
# git (keeps the remote repo small); this script is the only source of them.
#
# The 3 files must match the names expected by `crate::tools::file::ocr_engine()`
# (crates/hermix-tools/src/tools/file.rs):
#   pp-ocrv5_mobile_det.onnx, pp-ocrv5_mobile_rec.onnx, ppocrv5_dict.txt
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/download-ocr-models.ps1 [-Required]
#   -Required : exit non-zero if any model cannot be fetched (use for builds).
#                Omit for dev (warn-only, so offline dev still starts).

[CmdletBinding()]
param(
    [switch]$Required
)

$ErrorActionPreference = 'Continue'

# scripts/ -> repo root -> tauri resources/ocr
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$ocrDir = Join-Path $repoRoot 'apps' 'desktop-tauri' 'src-tauri' 'resources' 'ocr'

$models = @(
    @{ name = 'pp-ocrv5_mobile_det.onnx'; url = 'https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/pp-ocrv5_mobile_det.onnx' },
    @{ name = 'pp-ocrv5_mobile_rec.onnx'; url = 'https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/pp-ocrv5_mobile_rec.onnx' },
    @{ name = 'ppocrv5_dict.txt';        url = 'https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/ppocrv5_dict.txt' }
)

if (-not (Test-Path $ocrDir)) {
    New-Item -ItemType Directory -Force -Path $ocrDir | Out-Null
}

$failed = @()

foreach ($m in $models) {
    $dest = Join-Path $ocrDir $m.name
    if (Test-Path $dest) {
        Write-Host "[ocr] skip $($m.name) (already present)"
        continue
    }
    Write-Host "[ocr] downloading $($m.name) ..."
    # --ssl-no-revoke: Windows schannel otherwise fails with
    # CRYPT_E_REVOCATION_OFFLINE on GitHub release CDN.
    & curl.exe --ssl-no-revoke -L -o $dest $m.url
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "[ocr] FAILED to download $($m.name) (curl exit $LASTEXITCODE)"
        if (Test-Path $dest) { Remove-Item $dest -Force -ErrorAction SilentlyContinue }
        $failed += $m.name
    } else {
        $size = (Get-Item $dest).Length
        Write-Host "[ocr]   -> $dest ($size bytes)"
    }
}

if ($failed.Count -gt 0) {
    $msg = "[ocr] $($failed.Count) model(s) missing: $($failed -join ', '). " +
           'OCR will be disabled (graceful fallback to vision sub-agent). ' +
           'For an offline build, pre-place these files in: ' + $ocrDir
    if ($Required) {
        Write-Error $msg
        exit 1
    } else {
        Write-Warning $msg
    }
} else {
    Write-Host "[ocr] all models ready in $ocrDir"
}

exit 0
