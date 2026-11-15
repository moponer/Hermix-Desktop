# hermix-relay

Hermix M 的自建中转。**只路由，不执行**：按 token 哈希把手机的消息转到你的电脑，
不解析消息内容，不参与授权判定。

## 信任边界（先读这段，它决定其余所有设计）

一帧数据会经过两个独立的校验点：

| | 校验什么 | 谁做 |
|---|---|---|
| **relay（这里）** | 「这个 token 是不是本 relay 登记过、绑到哪个 node」——**路由级** | `auth.js` |
| **hermix（你的电脑）** | token 真伪 + 是否撤销 + **请求签名** + 能力位 + 会话归属——**授权级** | `security.rs` / `mobile.rs` |

所以：**relay 被攻破，攻击者能改路由，但拿不到会话内容、拿不到 hermix 侧身份。**

relay **不做**的事，每一条都有理由：

- **不解析内层 payload。** 签名覆盖 `METHOD\npath`，relay 一旦重组请求，手机端的
  `X-Hermix-Signature` 就全废。relay 是**字节级透明管道**。
- **不校验 hermix 侧授权。** 它没有 hermix 的设备表，也不需要。
- **不存明文 token。** 库里只有 sha256 哈希，明文只在 `relayctl` 签发那一刻出现一次。
- **不写 payload 进审计。** 审计只记「谁在什么时候连过」，不记内容。

## 为什么必须用域名，不能用 IP

1. 配对时 App 会**显示并锁定 relay 证书指纹**。裸 IP 拿不到 Let’s Encrypt 公开信任
   证书，只能自签——那就退化成"首次信任对象由用户手输决定"，输错或被换址就交出设备
   token。
2. WSS 需要可信证书链，否则首连就提示证书无效，用户只会认为 App 坏了。
3. VPS 重装换 IP 很常见，改 DNS 比改 App 配置省事。

## 部署

### 1. 域名与证书

```bash
# 假设域名 relay.example.com 已 A 记录到你的服务器 IP
sudo apt update && sudo apt install -y nodejs npm
sudo mkdir -p /opt/hermix-relay && sudo chown $USER /opt/hermix-relay
cd /opt/hermix-relay
# 把本目录（除 node_modules）传上来，然后：
npm install --omit=dev
```

**TLS 交给 Caddy**（自动续期，省掉证书过期这类半夜故障）：

```bash
sudo apt install -y caddy
```

`/etc/caddy/Caddyfile`：

```
relay.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

```bash
sudo systemctl reload caddy
```

### 1b. 导出证书指纹（给手机屏 1 核对用）

```bash
# Caddy 签的证书在 /var/lib/caddy/... 或用 certbot 的话在 /etc/letsencrypt
# 取线上证书的 sha256（DER，openssl 出的十六进制）
echo | openssl s_client -servername relay.example.com -connect relay.example.com:443 2>/dev/null \
  | openssl x509 -outform DER \
  | openssl dgst -sha256 -hex | sed 's/^.*= //'
```

把结果写进 systemd 的环境变量，relay 会在 `/health` 的
`x-hermix-cert-sha256` 头里回它：

```ini
[Service]
Environment=HERMIX_RELAY_CERT_SHA256=<上面那串 hex>
```

> ⚠️ **这还不是公钥固定**。它只是 relay **自报**的指纹：relay 被整个换掉，
> 指纹会跟着换，客户端照样通过。真正的 pinning 要客户端预置可信指纹
> （配对时交换）或走原生 TLS 校验。`hermix-m` 的屏 1 在拿不到这个头时会
> 明确显示「无法核对指纹」，而不是显示一个假的「已核对」。

Caddy 会自动申请并续期证书。装完后 `curl https://relay.example.com/health` 应当返回
`{"ok":true,...}`。

### 2. systemd 常驻

`/etc/systemd/system/hermix-relay.service`：

```ini
[Unit]
Description=Hermix M relay
After=network.target

[Service]
Type=simple
User=hermix
WorkingDirectory=/opt/hermix-relay
Environment=HERMIX_RELAY_DB=/var/lib/hermix-relay/relay.db
ExecStart=/usr/bin/node src/server.js
Restart=always
RestartSec=3
# 最小权限：不需要写代码目录
ProtectSystem=strict
ReadWritePaths=/var/lib/hermix-relay
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /usr/sbin/nologin hermix
sudo mkdir -p /var/lib/hermix-relay && sudo chown hermix /var/lib/hermix-relay
sudo systemctl daemon-reload && sudo systemctl enable --now hermix-relay
sudo systemctl status hermix-relay
```

默认监听 `127.0.0.1:8787`（只给 Caddy 反代，**不要**直接暴露到公网——明文 HTTP 面
被扫到会泄露审计表）。要换端口改环境变量 `HERMIX_RELAY_PORT`。

### 3. 登记凭据

```bash
cd /opt/hermix-relay

# 电脑侧：一条
node src/relayctl.js add-node  my-pc  "客厅那台"

# 手机侧：每台一条
node src/relayctl.js add-phone my-pc  "我的手机"
```

**明文 token 只打印一次**，丢了只能重签。记下来：

- 电脑侧的 token → 填进桌面端的 relay 配置
- 手机侧的 token → 填进 Hermix M 屏 1

```bash
node src/relayctl.js list my-pc     # 只回哈希前缀，可安全贴给别人看
node src/relayctl.js revoke <明文token>   # 立即生效，手机下次重连被拒
```

## 4. 桌面端接上

桌面端 hermix 的 relay 配置（配置面已就绪，见 `relay_client.rs`）：

| 字段 | 值 |
|---|---|
| `url` | `wss://relay.example.com/relay/node` |
| `node_id` | `my-pc` |
| 凭据 | `relayctl add-node` 打印的那条 |

**留空 = 不启用远程**，便携零安装产品的默认形态是「没开远程」，不是「开了但连不上」。

## 5. 手机接上

Hermix M 屏 1 填 relay 地址 `https://relay.example.com`，用 `add-phone` 的 token
配对。**配对页会显示 relay host + 证书指纹，必须核对后再确认。**

> ⚠️ 手机**设备 token**（`hmxd_*`，由 hermix 配对产生）和 **relay token**
> （`hrel_*`，由 `relayctl` 产生）是两个东西。手机端两条都要有：
> `hmxd_*` 交给 hermix 验身份，`hrel_*` 交给 relay 路由。

## 运维

```bash
# 健康检查
curl https://relay.example.com/health

# 审计（默认保留 30 天，可在 openAudit 参数调）
sqlite3 /var/lib/hermix-relay/relay.db \
  "SELECT datetime(ts/1000,'unixepoch'), event, node_id, role, detail FROM audit ORDER BY id DESC LIMIT 20"

# 手动清理过期审计
```

`/health` 里的 `audit_failures` 持续增长说明审计表写不进去（磁盘满 / 权限），
**不影响转发**（审计是旁路），但意味着出事查不到记录。

## 故障排查

| 现象 | 多半是 |
|---|---|
| 手机一直连不上 | DNS 没生效 / 证书签发失败 / Caddy 没 reload；先在手机上用浏览器打开 `https://relay.example.com/health` |
| 电脑侧一直重连 | `relayctl` 的 node token 不对，或 `node_id` 拼写不一致 |
| 连上了但收不到消息 | 对方没在线（看 `/health` 的 `node_connected` / `phones` 计数）；两端 token 角色搞反（手机 token 连了 `/relay/node`） |
| 配对成功但所有请求 401 | 手机系统时钟偏差超 ±300s（请求签名含时间戳） |
| 配对成功但 WS 握手被拒 | hermix 侧 `devices.db` 打不开，或 token 未在 hermix 配对（relay 通过 ≠ hermix 通过） |

## 换掉 relay 会怎样

**换地址是改配置，不动代码**——`relay_client` 只认配置里的 URL，协议层保持
base-URL 可替换。所以将来要换服务商 / 换服务器，桌面端和 App 各改一个地址即可。
