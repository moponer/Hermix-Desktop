$repo = '<workspace>/hermix-desktop'
$sidecar = Join-Path $repo 'apps/desktop-tauri/src-tauri/binaries/hermix-x86_64-pc-windows-msvc.exe'
if (-not (Test-Path $sidecar)) {
    Write-Host "SKIP: sidecar not present"
    exit 0
}
$sidecarTime = (Get-Item $sidecar).LastWriteTime
$rsFiles = Get-ChildItem (Join-Path $repo 'crates'), (Join-Path $repo 'apps/desktop-tauri/src-tauri/src') -Filter '*.rs' -Recurse -ErrorAction SilentlyContinue
$newest = $rsFiles | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host "Sidecar mtime: $sidecarTime"
Write-Host "Newest .rs   : $($newest.FullName) => $($newest.LastWriteTime)"
if ($newest.LastWriteTime -gt $sidecarTime) {
    Write-Host "STALE: sidecar predates source"
    exit 1
} else {
    Write-Host "FRESH: sidecar is up to date"
    exit 0
}
