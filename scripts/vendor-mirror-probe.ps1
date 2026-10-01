# vendor-mirror-probe.ps1
# 实测各 vendor 下载源在「开发机」网络下的连通性 + 延迟。
# 用于固化 docs/notes/implemented/architecture/2026-08-22-custom-app-architecture.md 的
# 「下载源策略」/「首日验证清单 · 默认镜像源可达性」两节的默认镜像源列表。
# 必须在开发机(非 CI 沙箱)跑一遍，结果才有代表性。

$TimeoutSec = 8
$sources = @(
    @{ name = 'npmmirror';       url = 'https://registry.npmmirror.com/' }
    @{ name = 'gitee';           url = 'https://gitee.com/' }
    @{ name = 'jsdelivr-fastly'; url = 'https://fastly.jsdelivr.net/' }
    @{ name = 'jsdelivr-gcore';  url = 'https://gcore.jsdelivr.net/' }
    @{ name = 'jsdelivr-cdn';    url = 'https://cdn.jsdelivr.net/' }
    @{ name = 'github';          url = 'https://github.com/' }
    @{ name = 'npmjs';           url = 'https://registry.npmjs.org/' }
    @{ name = 'unpkg';           url = 'https://unpkg.com/' }
)

$ok = @()
$fail = @()
foreach ($s in $sources) {
    $t0 = [datetime]::Now
    try {
        $r = Invoke-WebRequest -Uri $s.url -TimeoutSec $TimeoutSec -Method Head -UseBasicParsing -ErrorAction Stop
        $ms = [int]([datetime]::Now - $t0).TotalMilliseconds
        '{0,-18} OK   {1,5}ms  HTTP {2}' -f $s.name, $ms, $r.StatusCode
        $ok += @{ name = $s.name; ms = $ms }
    }
    catch {
        $ms = [int]([datetime]::Now - $t0).TotalMilliseconds
        '{0,-18} FAIL {1,5}ms  {2}' -f $s.name, $ms, $_.Exception.Message
        $fail += $s.name
    }
}

'`n=== 结论 ==='
if ($ok.Count -gt 0) {
    $ordered = ($ok | Sort-Object ms | ForEach-Object { '{0}={1}ms' -f $_.name, $_.ms }) -join ', '
    "可达(按延迟排序): $ordered"
}
if ($fail.Count -gt 0) {
    "不可达: $($fail -join ', ')"
}
"默认源列表建议 = 可达源按延迟升序回退；全部失败才报错(绝不静默残缺)。"
