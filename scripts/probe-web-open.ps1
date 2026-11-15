# 批量执行器：在查看器内嵌网页视图里打开网址并读出状态。
#
# 走查看器本地 MCP HTTP 端点（默认 http://127.0.0.1:3911/mcp，JSON-RPC 2.0）。
# 路径：set_mode pure -> web_open -> 读 webview_state / webview_text。
#
# 用法：
#   & .\scripts\probe-web-open.ps1
#   & .\scripts\probe-web-open.ps1 -Port 3911 -Url "https://passport.chaoxing.com/"
param(
    [int]$Port = 3911,
    [string]$Url = "https://passport.chaoxing.com/"
)

$endpoint = "http://127.0.0.1:$Port/mcp"
$scriptId = 0

function Call-McpTool {
    param(
        [string]$Name,
        [hashtable]$Arguments = @{}
    )
    $scriptId++
    $body = [ordered]@{
        jsonrpc = "2.0"
        id      = $scriptId
        method  = "tools/call"
        params  = [ordered]@{
            name      = $Name
            arguments = $Arguments
        }
    } | ConvertTo-Json -Depth 10 -Compress

    try {
        $resp = Invoke-RestMethod -Uri $endpoint -Method Post `
            -ContentType "application/json" -Body $body -TimeoutSec 45
    } catch {
        return "ERROR: $_"
    }
    if ($resp.error) {
        return "ERROR: $($resp.error.message)"
    }
    ($resp.result.content | Where-Object { $_.type -eq 'text' } |
        ForEach-Object { $_.text }) -join "`n"
}

# 端点可达性自检
try {
    $null = Invoke-WebRequest -Uri $endpoint -Method Post `
        -ContentType "application/json" -Body '{"jsonrpc":"2.0","id":0,"method":"ping"}' `
        -TimeoutSec 5 -UseBasicParsing
} catch {
    Write-Host "无法连上查看器 MCP 端点 $endpoint —— 查看器是否在运行且 MCP 已开启？" -ForegroundColor Red
    exit 1
}

Write-Host "==> 切纯净模式 (viewer_action set_mode pure)" -ForegroundColor Cyan
Call-McpTool -Name "viewer_action" -Arguments @{ action = "set_mode"; args = @{ mode = "pure" } }
Write-Host

Write-Host "==> 在内嵌网页视图打开: $Url" -ForegroundColor Cyan
Call-McpTool -Name "web_open" -Arguments @{ url = $Url }
Write-Host

Start-Sleep -Seconds 6

Write-Host "==> 网页视图状态 (webview_state)" -ForegroundColor Cyan
Call-McpTool -Name "webview_state"
Write-Host

Write-Host "==> 可见文本前 600 字 (webview_text)" -ForegroundColor Cyan
$text = Call-McpTool -Name "webview_text"
if ($null -ne $text -and $text.Length -gt 600) { $text.Substring(0, 600) } else { $text }
