# relay 远程作业手册（agent 用）

给持 SSH 密钥的 agent 执行部署与上传的完整作业书。所有命令已按本仓库
现状核实；凡标注 **[远端实况待核]** 的条目，先按 §1「事前检查」确认再动手。

---

## 0. 接入方式（SSH 密钥）

- 主机：`hermix.cssso.com`（用户以实际发放的密钥/用户名为准，常用 `root`）
- agent 通过 `ssh -i <密钥> root@hermix.cssso.com` 执行命令，`scp -i` 传文件
- **只允许发布目录内的写操作**（见 §6 禁区）；首次登录先跑一遍 §1 事前检查
- 本地仓库根：`<repo>`（下文相对路径都基于它）

## 1. 事前检查（每次作业前必跑，全部只读）

```bash
# 1) relay 服务实况：单元名、工作目录、启动命令 —— 一切路径以它为准 [远端实况待核]
ssh root@hermix.cssso.com 'systemctl status hermix-relay --no-pager | head -8; systemctl cat hermix-relay | grep -E "WorkingDirectory|ExecStart|Environment"'

# 2) relay 根与发布面现状（下文以 RELAY_ROOT=/www/wwwroot/hermix-relay 代称，
#    与上一步 WorkingDirectory 对不上时以实况为准）
ssh root@hermix.cssso.com 'ls -la /www/wwwroot/hermix-relay/ /www/wwwroot/hermix-relay/deploy/desktop/ 2>/dev/null; readlink /www/wwwroot/hermix-relay/deploy/desktop/current'

# 3) BBS 进程与端口（反代目标 127.0.0.1:3901）
ssh root@hermix.cssso.com 'ss -ltnp | grep 3901; systemctl status hermix-bbs --no-pager 2>/dev/null | head -5'

# 4) 服务健康基线（作业后对比用）
curl -s https://hermix.cssso.com/health | head -c 300
```

## 2. relay 根目录结构（发布面全景）

```
/www/wwwroot/hermix-relay/          # RELAY_ROOT（relay 进程的 WorkingDirectory）
├── src/ … package.json             # relay 服务代码（git 侧 apps/relay/ 的部署副本）
├── public/                         # 官网静态站 + APK（agent 可写：仅 hermix.apk）
│   ├── hermix.apk                  # ★ 手机安装包（§4）
│   ├── index.html … site.js        # 官网页面（changelog.html 会 fetch releases.json）
│   └── desktop/  spiker/           # 旧载荷目录：已退役，只读不动（见 §6）
└── deploy/                         # ★ 版本化发布面（agent 的主战场）
    ├── desktop/
    │   ├── 1/ 2/ …                 # 版本目录：latest.zip + manifest.json + releases.json
    │   └── current -> 2            # 符号链接；serve 只认 current；回滚 = 改指向
    └── spiker/                     # 同构（spiker 发布时启用）
```

首次走新链路时 `deploy/desktop/` 可能还不存在——release.sh 带 `--host`
上传时会自动迁移（旧 `public/desktop/` 载荷拷进 `1/` 并建 `current`）；
agent 手工部署则按 §3b 第 1 步自建。

## 3. 桌面端发布

### 3a. 自动路径（推荐，本地跑）

```bash
cd <repo>
./scripts/release.sh --host root@hermix.cssso.com --notes "…" [--skip-build]
```

脚本自动完成：上传到 `deploy/desktop/<N+1>/`（N = current 当前指向）→
远端 `sha256sum` 对账 → **单次 rename 原子切换** `current` → 清掉更早版本
（保留上一版做回滚）。对账失败则 current 不动，客户端继续用旧版。

### 3b. 手工路径（release.sh 不可用时，agent 逐条执行）

本地产物三件（`./scripts/release.sh --skip-build --skip-upload` 跑完后在
`target/release/update-staging/`）：`latest.zip`、`manifest.json`、
`releases.json`。设 `N` = 远端 `readlink …/deploy/desktop/current` 的值，
`NEXT=$((N+1))`：

```bash
# 1) 传到新版本目录（绝不在 current 指向的目录里覆盖任何文件）
ssh root@hermix.cssso.com "mkdir -p /www/wwwroot/hermix-relay/deploy/desktop/$NEXT"
scp target/release/update-staging/latest.zip    root@hermix.cssso.com:/www/wwwroot/hermix-relay/deploy/desktop/$NEXT/
scp target/release/update-staging/manifest.json root@hermix.cssso.com:/www/wwwroot/hermix-relay/deploy/desktop/$NEXT/
scp target/release/update-staging/releases.json root@hermix.cssso.com:/www/wwwroot/hermix-relay/deploy/desktop/$NEXT/

# 2) 远端对账（SHA 取本地 sha256sum latest.zip 的输出）
ssh root@hermix.cssso.com "set -e
  cd /www/wwwroot/hermix-relay/deploy/desktop/$NEXT
  echo '<本地sha256>  latest.zip' | sha256sum -c -"

# 3) 原子切换（单次 rename；失败不会留下半切换状态）
ssh root@hermix.cssso.com "cd /www/wwwroot/hermix-relay/deploy/desktop && ln -sfn $NEXT .current.tmp && mv -Tf .current.tmp current"

# 4) 清理（保留 current 与上一版 $N）
ssh root@hermix.cssso.com "cd /www/wwwroot/hermix-relay/deploy/desktop && for d in [0-9]*; do [ \"\$d\" -ne $NEXT ] && [ \"\$d\" -ne $N ] && rm -rf \"\$d\"; done; true"
```

### 重启 relay（仅当改了 relay 代码本身；纯发布不用重启）

```bash
ssh root@hermix.cssso.com 'systemctl restart hermix-relay && sleep 2 && systemctl is-active hermix-relay'
```

> **[远端实况待核]** 仓库版 `src/server.js` 只导出 `createRelay`，没有
> 自启动尾段；生产进程的入口以 §1-1 查到的 `ExecStart` 为准。若实况就是
> `node src/server.js` 且重启后 `is-active` 失败，**先向主人报告实况**，
> 不要自作主张改 systemd 或写临时入口。

## 4. APK 发布（手机端）

APK 由主人本地编译，落在 `<repo>\apps\hm\dist\`（文件名
以 dist 实际为准）。上传两步：

```bash
# 1) 上传 APK（唯一允许写 public/ 的文件）
scp apps/hm/dist/<名字>.apk root@hermix.cssso.com:/www/wwwroot/hermix-relay/public/hermix.apk

# 2) 同步官网版本号：public/index.html 里 HERMIX_M_VERSION = "x.y.z"
#    （/api/app-info 与 mobile.html 下载区都从它读版本；APK 的 sha256 与
#    mtime 服务端自动按文件算，无需手工）
```

改 index.html 在本地仓库 `apps/relay/public/index.html` 改完随 relay 代码
一起发（或在远端就地 `sed -i`，下次 git 部署会覆盖）。

## 5. 发布后测试清单（每项都有期望值，全过才算完成）

```bash
# 1) 服务健康：ok:true 且 proto_version 存在
curl -s https://hermix.cssso.com/health

# 2) manifest 指向刚发的版本；X-Checksum-Sha256 响应头与 body.sha256 一致
curl -si https://hermix.cssso.com/desktop/manifest.json | grep -iE "checksum|version"
curl -s https://hermix.cssso.com/desktop/manifest.json | node -e "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{const m=JSON.parse(d);console.log(m.version,m.sha256)})"

# 3) zip 可下载且字节级一致（对账刚发的 sha256）
curl -sL https://hermix.cssso.com/desktop/latest.zip | sha256sum

# 4) releases.json：首条 = 刚发版本，notes 非空（≥1 条）
curl -s https://hermix.cssso.com/desktop/releases.json | head -c 400

# 5) 网页更新日志引用了动态源（页面 200 + fetch releases.json）
curl -s https://hermix.cssso.com/changelog.html | grep -o "desktop/releases.json"

# 6) APK：/api/app-info available:true 且版本号正确；下载头带指纹
curl -s https://hermix.cssso.com/api/app-info
curl -sI https://hermix.cssso.com/download/hermix.apk | grep -iE "content-length|checksum"

# 7) BBS 反代：200（BBS 未起时这里是 502，属 BBS 侧问题，不是 relay）
curl -s -o /dev/null -w "%{http_code}\n" https://hermix.cssso.com/bbs/

# 8) 安装包冒烟（本地）：Hermix-Setup-<ver>.exe 双击 → 向导 → 装完启动 →
#    设置里检查更新显示「已是最新」（版本号 = 刚发的）
```

## 6. 禁区（agent 不得做的事）

1. **不动 `public/desktop/`、`public/spiker/` 的旧载荷**——旧客户端可能仍
   在读迁移前的路径；退役清理等主人发话。
2. **不动 relay 数据库**（`HERMIX_RELAY_DB`，凭据登记/审计都在里面）；
   relayctl 是唯一合法工具。
3. **不改 systemd 单元 / 不开新端口 / 不装新依赖**——架构约束（同端口反
   代、零新端口）见
   `docs/notes/implemented/architecture/2026-10-11-relay-structured-deployment.md`。
4. **绝不直接覆盖 `current` 指向目录里的文件**——版本目录只新增不覆写，
   切换只走 `ln -sfn + mv -T` 单次 rename。
5. **不 push、不并行跑两个 release.sh**——发布链对 `current` 编号有竞态。
6. 任何对账失败/服务起不来：**停手，报告实况（§1 的检查输出）**，不要
   连续重试。

## 7. 回滚（一条命令）

```bash
# current 指回上一版（release.sh 每次都保留它）：
ssh root@hermix.cssso.com "cd /www/wwwroot/hermix-relay/deploy/desktop && ln -sfn <旧N> .current.tmp && mv -Tf .current.tmp current"
# 复核：manifest 版本号回到旧版
curl -s https://hermix.cssso.com/desktop/manifest.json | grep version
```

APK 回滚 = 把旧 apk 重新上传覆盖 `public/hermix.apk`（版本目录机制不覆盖
APK；旧包需从本地构建产物留存）。

## 8. BBS 部署（独立进程，仅在主人要求时）

部署包：`target/release/bbs-deploy/`（bbs 五件 + relay 侧反代说明 +
部署指引）。目标：RELAY_ROOT 同机、只绑 `127.0.0.1:3901`、SQLite 数据
目录在版本目录之外（**版本化代码，不版本化用户数据**）：

```bash
scp -r target/release/bbs-deploy/* root@hermix.cssso.com:/www/wwwroot/hermix-relay/deploy/bbs/current-new/
ssh root@hermix.cssso.com 'cd /www/wwwroot/hermix-relay/deploy/bbs && \
  [ -d current ] && mv current previous || true; mv current-new current; \
  cd current && (systemctl restart hermix-bbs 2>/dev/null || nohup node src/server.js >> /tmp/bbs.log 2>&1 &)'
curl -s -o /dev/null -w "%{http_code}\n" https://hermix.cssso.com/bbs/   # 期望 200
```

systemd 单元名 [远端实况待核]——以 §1-3 查到的为准；手工 nohup 只是兜底，
正式看护以 systemd 为准。
