# Decision Note: 浏览器自动化能力分两层内置 —— 通用操作能力 skill + 站点 skill，查看器 MCP 进内置目录

Status: implemented — 2026-10-21 落地：内置 skill `browser-automation` 重写为双通道
（含查看器有头主路径），`chaoxing-learning` 迁入内置位置并瘦身为站点知识，
查看器 MCP 作为第一方条目进后端「内置目录」

## Decision

浏览器自动化的知识分两层，**都是产品内置**（随 `hermix-server` 打包，启动时
seed 进 `~/.hermix/skills/`）：

1. **能力层**：`crates/hermix-server/src/dashboard/skills/browser-automation.md`
   —— 通道选择（查看器内嵌浏览器 = 有头主路径；`browser_*` = 无头）、
   接线前提、截图读小字、DOM 点击、同源 iframe 下钻、每步核对、
   提交后读标准答案再重做。**与具体站点无关**。
2. **站点层**：每个站点一个 skill（如 `chaoxing-learning`）—— 只记该站点的
   URL 形态、DOM 选择器、播放器 API、账号课程清单。**引用能力层，不复述它**。

同时：

3. **查看器 MCP 进后端「内置目录」**（`mcp_catalog.rs` + `catalog_fallback.json`），
   作为**第一方**条目 `hermix-viewer`（`http://127.0.0.1:3911/mcp`），
   用户在桌面端「连接器 → 内置目录」点一次安装即可被 agent 使用。
4. **`mcp_scan` 的 `plain_http_remote` 红线收窄**：只对**非回环**地址硬拒绝；
   回环地址（`localhost` / `127.x` / `::1`）降级为「需确认」提示
   （`plain_http_loopback`），不豁免成安全。
5. **浏览器自动化的唯一入口是查看器 MCP**。内建的无头 `browser` 工具集
   （`hermix-tools` 的 `os_auto::browser`）**退役**：两套同名工具并存会让模型在
   `browser_click(ref)` 与 `hermix-viewer__browser_click(x, y)` 之间猜参数形状
   （`hermix-tools/src/tools/os_auto/mod.rs` 与 `essential.rs` 同步退役该注册）。
   查看器 MCP 侧的 `browser_open(headed=…)` 覆盖了原无头语义：
   `headed=false` = 无头 + 独立 profile，`headed=true` = 弹真实窗口。

## 为什么

**为什么 skill 必须重写而不是补丁**：旧的 `browser-automation` 全篇只讲无头
`browser_*`。agent 照着它做，会把网页甩到用户看不见、也登不进去的独立窗口里 ——
而超星这类任务（登录、看视频、答测验）**必须**用户在同一条链路上。
用户已经明确：以后其他浏览器自动化任务仍旧要走「MCP 驱动查看器的有头浏览器」。
→ 能力层 skill 必须把有头通道写成**主路径**，无头写成备选。

**为什么不能留在仓库根的 `skills/`**：核查过 —— 根 `skills/` 目录**不被任何
东西消费**（`scripts/`、CI、构建脚本、`BUILTIN_SKILLS` 全都没有引用；
`skills/context-engineering/*` 那 12 个同样如此）。出货通道只有一条：
`crates/hermix-server/src/dashboard/skills/*.md` → `seed_builtin_skills_default()`
→ `~/.hermix/skills/<category>/<name>/SKILL.md`。所以 `chaoxing-learning`
从 `skills/lifestyle/` **迁**进内置位置（`git mv` 保留历史），否则它就是
「仓库里有、产品里没有」。

**为什么进「内置目录」而不是直接写进配置**：桌面端 agent 的外部工具面**只**来自
`$HERMIX_HOME/mcp_servers.json`，之前查看器那条是我**手工**加的 —— 换台机器
就没有这个能力了。「内置目录 → 点安装」是本仓库既定的接入口径
（`mcp_catalog.rs` 模块头写明：目录是候选清单，装不装由用户决定）。

**为什么回环明文 HTTP 可以放行**：那条红线的理由是「对话内容会裸奔在网络里」。
回环地址的流量**不出本机**，前提不成立 → 硬拒绝是误伤。但同机进程能监听，
所以**降级为需确认**而不是豁免。

## Alternatives considered

- **把查看器 MCP 直接 auto-seed 进 `mcp_servers.json`**：省掉一次点击，但违反
  `mcp_catalog.rs` 明写的「仍不做 auto-seed」；更要紧的是查看器 MCP 的工具面
  里有 `desktop_click` / `desktop_set_text`（**桌面级**控制），
  默认打开等于默认把点击权交给本机任意进程。**保持默认关** + 目录一键安装。
- **给 `skills/` 根目录加一个同步脚本**：多一份真相 + 多一道工序，
  还要处理「脚本没跑 skill 就没更新」。否决。
- **单开一个「查看器浏览器」skill**：agent 面前会有两个浏览器 skill 要选，
  而它们的工具前缀（`hermix-viewer__*`）本来就是同一件事。否决 ——
  合并进能力层，站点差异才不会被当成能力差异。
- **保留内建无头 `browser` 工具集**：它是能用的，但两套同名工具并存会让模型
  猜参数形状（`browser_click(ref)` vs `hermix-viewer__browser_click(x, y)`）。
  查看器 MCP 的 `browser_open(headed=false)` 覆盖同样的无头语义（独立进程 +
  独立 profile、CDP 同构），所以退役内建那套是**减法**而不是能力损失。
- **翻默认：把查看器 MCP 改成默认开**：见上，安全面太大。否决。

## Consequences

- agent 拿到浏览器自动化能力时，**先看到有头通道**（用户看得见、能自己登录），
  无头 `browser_open(headed=false)` 退为「批量/无需人看」时的选择。
- 能力层与站点层不再互相复述：站点结构变了只改站点 skill，
  操作方式变了只改能力层。**上一笔提交里两处混写的知识已拆开。**
- 能力层 skill 写的是**目标架构**（浏览器自动化统一归查看器 MCP）。内建无头
  `browser` 工具集的退役由 `hermix-tools` 侧同一批改动完成 —— 两者必须一起
  落地：只退役不更新 skill，agent 会去找一套已经不存在的工具。
- 工具表里没有 `hermix-viewer__*` 时，agent 会**如实报告前置条件**
  （查看器在跑 + 查看器 设置 → MCP 开启 + 连接器里装上），而不是偷偷改走无头通道。
- 内置目录多了一条**第一方**条目：`official: true` 在此表示「随产品一起发布的
  第一方组件」，而非「registry 官方登记」—— 这一点写进了目录 JSON 的 comment。
- `mcp_scan` 的红线**变窄了**：公网明文 HTTP 仍然硬拒绝；回环明文 HTTP
  从「拒绝」变成「需确认」。新增两个测试把这条边界钉死（含
  `127.0.0.1.evil.com` 这类「看着像回环的域名」不得放行）。
- 接线仍需用户点两下（查看器设置开 MCP、连接器装条目）—— 这是有意保留的：
  把桌面级控制默认打开的代价，比多点两下大。
