# 停掉本地文件索引助手（hermix-filesearch.exe）。
#
# 为什么需要这个脚本：索引助手是**提权进程**（读 MFT 要管理员权限），
# 普通 shell 的 `taskkill` 会被 Windows 拒绝（Access is denied），
# 于是「重新编译它」会因 exe 被占用而失败（os error 5）。
#
# 但它**能自己退出**：回环协议里有 `shutdown` 方法，收到后刷盘、删
# endpoint.json、退出。所以正确姿势是「请它走」，不是「杀它」。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/stop-filesearch.ps1
#
# 等价命令（助手新版自带）：target\debug\hermix-filesearch.exe --stop

$ErrorActionPreference = "Stop"

$endpointFile = Join-Path $env:LOCALAPPDATA "hermix\filesearch\endpoint.json"
if (-not (Test-Path $endpointFile)) {
    Write-Host "index service not running (no endpoint.json)"
    exit 0
}

$endpoint = Get-Content $endpointFile -Raw | ConvertFrom-Json
$port = [int]$endpoint.port
$token = [string]$endpoint.token

try {
    $client = New-Object System.Net.Sockets.TcpClient
    $client.Connect("127.0.0.1", $port)
    $stream = $client.GetStream()
    $payload = '{"id":1,"method":"shutdown","token":"' + $token + '"}' + "`n"
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
    $reader = New-Object System.IO.StreamReader($stream)
    $response = $reader.ReadLine()
    $reader.Close()
    $client.Close()
    Write-Host "index service stopped: $response"
} catch {
    Write-Host "stop failed (service may have exited already): $($_.Exception.Message)"
    exit 1
}
