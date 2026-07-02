# 清理 WebView2 缓存和 DevTools 扩展
# 关闭 hermix 后跑这个脚本，会清掉：
#   - HTTP 磁盘缓存（最大头，~200MB）
#   - Code Cache（编译缓存，~96MB）
#   - DevTools 扩展（~28MB）
#
# 不删除的东西：
#   - Crashpad（崩溃报告，Edge 自带，删了下次又生成）
#   - BrowserMetrics（同上）
#   - IndexedDB（前端本地存储）

$ErrorActionPreference = "Stop"

$paths = @(
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\Cache",
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\Code Cache",
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\Extensions",
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\File System",
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\Storage\ext",
    "$env:LOCALAPPDATA\com.hermix.desktop\EBWebView\Default\GPUCache"
)

$totalBefore = 0
foreach ($p in $paths) {
    if (Test-Path $p) {
        $size = (Get-ChildItem -Recurse -Force $p -ErrorAction SilentlyContinue |
                 Measure-Object -Property Length -Sum).Sum
        $totalBefore += $size
        Write-Host ("  {0,-55} {1,8:N1} KB" -f $p, ($size / 1KB))
    }
}
Write-Host ""
Write-Host ("Total: {0:N1} MB" -f ($totalBefore / 1MB))
Write-Host ""

# 检查是否还在跑
$running = Get-Process -Name "hermix-desktop", "msedgewebview2" -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "❌ hermix-desktop 或 msedgewebview2 还在跑，请先关闭 hermix"
    $running | Format-Table Name, Id, @{N="Mem(MB)";E={[math]::Round($_.WorkingSet64/1MB,1)}}
    exit 1
}

foreach ($p in $paths) {
    if (Test-Path $p) {
        Remove-Item -Recurse -Force $p
        Write-Host "  deleted: $p"
    }
}

Write-Host ""
Write-Host "✅ done. 重启 hermix 后磁盘占用应该下降 ~300MB。"