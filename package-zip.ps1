$exe = "<repo-parent>\release\target\release\hermix-desktop.exe"
$sidecar = "<repo>\release\target\release\hermix.exe"
$dst = "<repo-parent>\HermixPortable"
$zip = "<repo>\HermixDesktop.zip"

if (Test-Path $dst) { Remove-Item $dst -Recurse -Force }
Remove-Item $zip -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $dst -Force | Out-Null
Copy-Item $exe $dst -Force
Copy-Item $sidecar $dst -Force
Add-Type -Assembly "System.IO.Compression.FileSystem"
[System.IO.Compression.ZipFile]::CreateFromDirectory($dst, $zip, "Fastest", $false)
$size = (Get-Item $zip).Length
Write-Host "OK zip=$zip size=$($size/1MB)MB"
