# Decision Note: relay 服务端结构化重构——产品自治 + 路由注册表 + 原子发布

Status: implemented — 2026-10-11 落地（class: architecture）
Class: architecture
Date: 2026-10-11

## Decision

把 relay 从「一个 548 行的 server.js 什么都管」重构为**三条硬边界 + 一张路由注册表 + 一套原子发布流程**。核心原则：**每个产品自带自己的目录，路由从目录结构推导，发布 = 换目录 + 换符号链接**。以后任何产品（desktop/spiker/bbs/未来的）上线，都是「放目录、登记一行」，不再改 server.js 逻辑。

### 1. 目录边界：产品自治（products/ 为唯一发布面）

```
apps/relay/
  src/
    server.js          # 只做：中间件 + 挂注册表 + listen。≤150 行
    registry.js        # 产品注册表（唯一需要「登记」的地方）
    auth.js proto.js audit.js relayctl.js   # 现状保留（连接层，本就不乱）
    products/
      desktop.js       # /desktop/* 路由处理：manifest + latest.zip（读 DEPLOY_DIR）
      spiker.js        # /spiker/* 同构
      pages.js         # 官网页面路由（/、/static-mascot.png、apk 下载）
  public/              # 静态站点（现状保留，express.static 兜底）
  deploy/              # ★ 新增：远程部署面的本地镜像（gitignore）
    desktop/
      1/               # 版本化目录（原子发布的单位）
        latest.zip
        manifest.json
      current -> 1     # 符号链接，serve 只认 current
    spiker/
      1/ current -> 1
```

**关键事实**：现在 `latest.zip`/`manifest.json` 躺在 `/www/wwwroot/hermix-relay/public/desktop/` 这个**网页静态目录**里——更新载荷和官网页面住同一家，这正是「每次更新怕出错」的根源：scp 错一个路径就是要么官网 404 要么更新失效。重构后**载荷永远不进 public/**。

### 2. 路由注册表：一行一个产品，加产品不改核心

`registry.js`：

```js
export const PRODUCTS = [
  { name: 'desktop', mount: '/desktop', deployDir: 'deploy/desktop/current' },
  { name: 'spiker',  mount: '/spiker',  deployDir: 'deploy/spiker/current'  },
];
```

server.js 遍历注册表挂路由，每个 product 模块导出同一个形状：`register(app, { deployDir })`。新增第三个产品的操作 = 建目录 + 注册表加一行 + 新建一个 ≤40 行的 product 模块。**注册表是产品清单的唯一真相源**——manifest 版本号、zip 校验和的逻辑全部长在 product 模块里，core 不认识任何具体产品名。

现状的硬编码消除对照：

| 现状（server.js 硬编码）                             | 重构后                                             |
| --------------------------------------------- | ----------------------------------------------- |
| `app.get('/desktop/manifest.json')` 专属函数 ×2 组 | products/ 下同构模块，注册表驱动                           |
| `/bbs` 反代一行 `app.use` 混在中间件区                  | registry 加 `{ type: 'proxy', target: 3901 }` 一行 |
| `/spiker/` 路由（并行会话维护的 250 行起）                 | products/spiker.js，边界不变、可继续独立改                  |

### 3. 原子发布：版本目录 + current 链接 + upload-then-swap

现在 `scp latest.zip` 是**原地覆写**：传一半时用户下载到截断文件；manifest 先传完的窗口期版本号与 zip 不一致（现在靠「manifest 最后传」的纪律人肉保证）。

重构后发布流程（release.sh 改造，服务端零手工步骤）：

```
1. scp zip → deploy/desktop/2.latest.zip（版本目录，写到新编号，不碰 1/）
2. scp manifest → deploy/desktop/2/manifest.json（同目录，天然同批）
3. ssh "ln -sfn 2 deploy/desktop/current.tmp && mv -Tf current.tmp current"
   —— symlink swap 是单 rename，原子的：所有用户的下一次请求要么看到完整旧版要么看到完整新版
4. ssh "rm -rf deploy/desktop/1"（保留上上个版本做回滚，删更早的）
```

- **回滚** = `ln -sfn 1 current`，一条命令。
- **「manifest 最后传」纪律退役**——同目录同批，结构上不可能错位。
- **校验内建**：product 模块 serve manifest 前现算 zip 的 sha256 与 manifest 对账，不一致直接 500 并打日志，坏载荷永远不会被客户端消费。

### 4. 职责分层全景（重构后的心智模型）

```
┌─ 静态层 public/        官网页面、mascot、apk 引导（改动少，独立）
├─ 产品层 products/      各产品的 manifest/zip/页面路由（每产品一个文件）
├─ 连接层 auth/proto     手机端⇄桌面端 WebSocket 配对与转发（不动）
└─ 反代层 registry:proxy /bbs → 3901、/spiker-app → 独立进程（一行登记）
```

排错路径唯一化：网页错 → public/；下载/更新错 → deploy/<产品>/；连接错 → auth/proto；bbs 错 → bbs 进程。**不再需要在 548 行里找哪一段管这个 URL**。

### 5. BBS 并入：从「野进程」变成注册表里的一个产品

现状：BBS 是 `target/release/bbs-deploy/` 里手工部署的 Node+SQLite 进程，relay 用一行 `app.use('/bbs', proxy→3901)` 反代它。它不在任何注册表里——端口、启动方式、部署位置全靠 DEPLOY.md 文档人肉记忆。这正是「每次更新怕出错」的第二个来源：**更新 relay 不用动 BBS，更新 BBS 却要记住三条暗知识**（进程在哪、怎么重启、端口不能撞）。

重构后 BBS 以 `type: 'proxy'` 产品身份进注册表：

```js
{ name: 'bbs', mount: '/bbs', type: 'proxy',
  target: 'http://127.0.0.1:3901',
  health: '/bbs/api/health' }        // registry 统一做健康探针
```

- **部署面同构**：BBS 的静态资源与数据目录也进 `deploy/bbs/<版本>/current` 语义（代码目录走同一套 upload-then-swap；SQLite 数据文件不进版本目录，走 `data/` 常驻目录——**版本化的是代码，不是用户数据**）。
- **进程看护统一**：bbs 进程从手工 `node src/server.js` 改为与 relay 同一个 systemd 单元树（`hermix-relay.service` 拉起，或独立 `hermix-bbs.service` 但由同一份 deploy 脚本管理），启动命令长在 deploy 目录里（`deploy/bbs/start.sh`），不再活在文档里。
- **端口暗知识显式化**：3901 写进注册表 `target` 字段——注册表成为端口/路由对应关系的唯一真相源，排错时 `cat registry.js` 即得全景。
- 反代语义不变：`/bbs/*` 仍走 relay 同端口反代（遵守「不新开域名/端口」约束），用户侧零感知。

### 6. 更新信息单一真相源：releases.json（一份动态信息喂三个消费端）

现状第三处乱源：**更新说明是三份人肉副本**——`manifest.notes`（客户端更新弹窗读）、`changelog.html`（网页手改）、安装器/官网各写各的版本描述。发一次版要记得改 N 处，漏一处就是「升级包说 A、网页说 B」。

重构为**一份 `releases.json`，三个消费端各取所需**：

```
deploy/desktop/releases.json        # 唯一真相源，随版本目录一起原子发布
[{
  "version": "0.1.10",
  "date": "2026-10-11",
  "notes": ["修复本地索引反复扫描", "安装器许可页显示完整正文", "欢迎语去除时间前缀"],
  "sha256": "c2bfe21d…",
  "min_version": null
}]
```

- **写**：release.sh 从 `apps/relay/release-notes/<版本>.md`（或提交信息自动汇总）生成，一处手写、机器分发。
- **消费端 1（客户端更新检查）**：`/desktop/manifest.json` 由 product 模块**从 releases.json 最新条目现算**——manifest 不再是独立维护的文件，notes/sha256/min_version 天然与 zip 同批一致。
- **消费端 2（网页 changelog.html）**：页面加载时 `fetch('/desktop/releases.json')` 动态渲染——网页的更新信息从「发版要手改」变成「发版自动出现」。
- **消费端 3（安装器许可/说明页）**：安装器 payload 里嵌的 manifest 来自同一条流水线，无需单独改动（现状已读 manifest，只需 notes 结构从 string 变 string[] 时同步解析）。
- **一致性保证是结构性的**：zip、manifest、releases.json 三者住在**同一个版本目录**、被**同一次 symlink swap** 原子切换——不存在「zip 新了 manifest 还是旧的」的窗口期；manifest 由 product 模块现算还多一层保险（serve 时校验 sha256 与 zip 实际值一致，不一致 500）。
- **兼容**：`manifest.json` 字段保持现状形状（version/notes/pub_date/url/sha256/min_version），老版本客户端零感知；releases.json 是新增文件，只有新消费端读。

## Alternatives considered

1. **拆成多个 Node 进程/端口**（desktop-relay、spiker-relay 各一个，Nginx 分流）——否决：违反「不新开域名/端口」的既定约束（见摘要 constraints），运维面从 1 个进程变 N 个，跨产品共享的 auth/proto 还得抽包，复杂度净增。
2. **纯路径约定不写注册表**（`/x/manifest.json` 永远映射 `deploy/x/current/`，零代码登记）——部分采纳：registry 本质就是这个约定 + proxy 例外。但纯约定无法表达 bbs 反代、apk 下载这类非载荷路由，留 registry 一处收敛比散在 server.js 各处好。
3. **保持现状 + 只写发布脚本文档化**（checklist 化「manifest 最后传」）——否决：文档能记住步骤，防不住 scp 手滑和传输窗口期的不一致；错误类别应该由结构消灭而不是靠细心。
4. **引入框架**（Fastify 插件化 / Nest 模块化）——否决：2334 行的代码库引框架是过度工程，两条硬边界（目录 + 注册表）用原生 express 三十行就能立住。

## Consequences

- **正向**：加产品 = 目录+一行+一个同构模块；发布原子化，错位/截断不可能；回滚一条命令；server.js 回归纯装配层，任何产品的修改不触碰别人的代码。
- **代价**：一次迁移（估 ~200 行改动 + release.sh 改 ~30 行 + 远端目录迁一次）；多一层 deploy/ 结构需要 README 讲清楚 current 链接语义。
- **风险与对策**：
  - *符号链接在远端主机（宝塔/Nginx 静态托管）可能不跟随* → 对策：确认 express.static/读文件都走 Node 进程内路径（现状就是 Node serve，不经过 Nginx 静态），链接在 Node 侧解析，无此风险；迁移前在服务器上 `ln -s` 实测一发。
  - *迁移瞬间远端新旧目录并存* → 对策：发布脚本首跑时自动把 `/public/desktop/latest.zip` 迁入 deploy/desktop/1/ 再建链接，客户端 URL（`/desktop/latest.zip`）不变，无感切换。
  - *并行会话正在维护 /spiker/ 路由* → 对策：重构 PR 不动 <u>spiker</u> 的内部逻辑，只把它包进 products/spiker.js 的接口壳里，搬移与改写不同时做。

## Reintroduction conditions

（architecture 类不适用；若未来产品数增长到 >6 个且出现跨产品共享状态的需求，再评估拆进程/框架方案 1、4。）
