# 启动 sccache server，缓存落到 H 盘共享目录，供多人 / 多 worktree 复用。
#
# 为什么必须单独启动 server（不能只靠 cargo [env]）：
#   sccache 0.18.0 的 in-process 模式会忽略 SCCACHE_DIR，只有 server 才读它。
#   SCCACHE_START_SERVER=1 只能在“启动 server”这一次用，不能传给 rustc-wrapper
#   （wrapper 收到该变量会报 “can't be used with other commands” 而中断编译）。
#   所以这里显式设好环境变量后启动 server，server 常驻并接管所有缓存。
#
# 另：编译必须关增量（CARGO_INCREMENTAL=0，已在 .cargo/config.toml [env] 设），
#   sccache 对带 -C incremental 的编译一律 non-cacheable，不关则命中率 0%。
$ErrorActionPreference = "Stop"
$env:SCCACHE_DIR = "H:/sccache-cache"
$env:SCCACHE_CACHE_SIZE = "50G"
& "<USERPROFILE>/.cargo/bin/sccache.exe" --start-server
