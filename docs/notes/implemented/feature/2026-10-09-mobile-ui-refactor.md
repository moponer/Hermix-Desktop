# Decision Note: 手机端（Hermix M）启动故障修复 + 界面重构

Status: implemented — 2026-10-09

## Decision

分两段做：**先让失败可见，再重构界面**。两者在同一次改动里落地，但因果上
前者是后者的前提——一个连白屏都定位不了的界面，没法判断重构有没有做对。

### 一、启动故障的三层成因

用户报的现象是「启动后转圈 → 过一会空白 → 出现一块半透明黑色蒙版」。这不是
一个 bug，是三层各自独立的缺陷叠出来的结果。

**第一层：`splashscreen` 配成了一副死锁。**

```json
"alwaysShowBeforeRender": true,   // 首屏没渲染出来就不许关
"waiting": true,                  // 关之前一直转圈
"autoclose": true,
```

uni-app 的语义是：`alwaysShowBeforeRender` 让启动图**等首屏渲染完成**才关闭。
只要首屏渲染不成功，`autoclose` 就永远不触发 → 永久转圈 → 系统放弃后
webview 空白 → `waiting` 那层半透明蒙版残留。这就是三个现象的完整因果链。
改为 `alwaysShowBeforeRender: false` / `waiting: false`，让首屏永远立即接管。

**第二层：全局零错误处理。** 改造前 `main.js` 只有 6 行，没有 `errorHandler`；
`App.vue` 没有 `onError` / `onUnhandledRejection`。于是**任何** setup 异常都
直接变成白屏，用户拿不到一行错误信息，也没有任何恢复路径。现在三层收口到
`core/boot.ts`：`main.js` 的 `errorHandler`、`App.onError`、
`onUnhandledRejection`，并**同时写 memory 与 storage**——写 storage 是因为
「boot 页自己也崩了」是唯一还能兜住这种情况的地方。

**第三层（真正的触发点）：配对屏的探针没有超时，且把「读不到」当成「连不上」。**

- `fingerprintOf()` 走裸 `fetch`，**没有超时**。弱网 / DNS 黑洞时能挂到系统层
  超时，配对屏的 `onLoad` 里 `await` 它，首屏就一直停在 loading。
- 更糟的是它 `return null`，调用方一律渲染成「连不上这个 relay。检查域名解析
  与证书。」——但 `null` 有两种完全不同的来源：**连不上**，和**连上了但这个
  环境不让 JS 读那个头**。`x-hermix-cert-sha256` 不是 CORS 简单响应头，
  relay 也没发 `Access-Control-Expose-Headers`，所以 H5 下**每次**都是第二种，
  而用户被引导去查一个根本不存在的证书问题。

修法是新增 `core/http.ts` 作为唯一出网口：`uni.request`（全端都有）、**显式
超时（默认 8s）**、**永不 reject**（用 `reachable` 字段表达失败，因为网络失败
是常态不是异常），并把指纹结果拆成三态 `verified / invisible / unreachable`。
读不到指纹时不再静默放行也不硬阻断，而是让用户**显式勾选确认**——保留人工
决策点，但不因为一个本环境根本拿不到的值把人挡在门外。

### 二、界面重构

**信息架构：新增 `pages/boot` 作为首屏 + 抽屉式导航。**

原来 `pages.json` 第一页是 `pages/pairing`，于是**每次**冷启动都落在配对屏，
而配对屏要发网络请求——「决定去哪一页」和「干活」被焊死在一起。现在 boot 页
**只读本地存储、不发任何请求**，因此不存在卡首屏的可能；它先渲染出像素再分流，
白屏时间等于零。

> **2026-10-09 晚间再改：boot 不再分流，一律进主界面。** boot 原本按「配对了
> 去对话、没配对去配对」分流，等于用「还没连」这件事把整个 App 锁住——用户只想
> 看一眼任务列表也得先过配对屏。现在 `boot` 只有**一个**出口：无条件进对话页，
> 未连接是主界面里的一张卡片（标题「还没有连接电脑」+ 去连接 / 设置两个按钮），
> 输入框置灰并提示「未连接电脑，暂时发不了任务」。**「不能做什么」和「进不来」
> 是两回事**，后者对用户是纯粹的阻碍。
>
> 配对屏相应收窄职责：relay 地址移出，只留「核对身份 + 输码」，地址改在设置页。
> 主界面里的「去连接」用 `navigateTo`（不是 reLaunch），返回时该回到对话。
>
> 安全性未因此降低：指纹核对仍卡在**输码之前**，即凭据入口之前；只是浏览主界面
> 不再需要先配对，而未配对时本来也发不出任何任务。

导航采用**抽屉**而非 tabBar：WorkBuddy 文档里明确所有功能入口都挂在侧边栏，
且 tabBar 需要一套图标资源（当前只有一张 mascot.png），造不出来就别造。

**砍掉的东西**（对照 WorkBuddy 文档逐条评估，只留我们真有的）：

| WorkBuddy 有 | 我们的处置 | 理由 |
|---|---|---|
| 云端工作 / 连接电脑 双模式 | **砍** | agent 引擎只在 PC，模式开关是假的 |
| 模型选择面板（能力标签 + 积分倍率） | **砍** | 缺 PC 侧模型列表接口；积分体系不存在 |
| 技能面板 / 专家体系 / SkillHub | **砍** | 云端市场，我们只有 PC 本地 skill 目录 |
| 自动化定时任务 / 记忆库 | **砍** | 缺 PC 侧调度器与存储 |
| 输入栏 `+` 附件面板 | **砍** | 手机端没有任何上传管线，做个入口是空壳 |
| 长按 AI 回复→重新生成 | **砍** | 协议里没有这个帧 |
| 底部固定输入栏 + 发送键运行中变停止 | **留** | 已有 `frames.ts` 状态机支撑 |
| 工具轨迹步骤卡（可展开 + 完成态） | **留** | 已有 runningTools / doneTools |
| 抽屉（身份区 / 主导航 / 底部设置） | **留** | 纯 UI |
| 任务列表（运行中高亮 + 位次） | **留** | 纯 UI |

**清掉全部假数据。** chat / tasks / approval / call 四页原来都塞着编出来的
演示内容（`把 tools 那一层的 dispatch 拆出来…`、三条假任务、两条假审批、
`elapsed = 252` 秒）。那不是占位示意，那是在界面上**造了一段不存在的历史**：
用户分不清是真跑过还是演示，出了问题也无从复现。现在初始就是真空态。

**顺带修掉的死代码：** `approval` 与 `call` 两页的 `startTicker()` **定义了却
从没被调用**，计时器根本不转；且里面用 `window.setInterval`，而 App / 小程序
端没有 `window`，一旦真被调用就是 ReferenceError。

### 三、取消描边

`uni.scss` 移除 `$line` / `$edge` 两支边框色，全站零 `border`（已 grep 复核，
剩下的命中全部是 `border-radius` 与 `box-sizing: border-box`）。层次改由
**底色差**（`#161719` 页 < `#1d1f22` 卡 < `#24262a` 浮层，这条抬升序列本来
就是主题文件里的既有定义）、**阴影**（只给抽屉等浮层）、**间距**三者承担。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 保留 splashscreen，只加 watchdog 计时器 | 治标。首屏仍然要等网络才能决定去哪，弱网下就是用户看到的那几十秒。boot 页不发请求是根治。 |
| 保留旧首屏（配对）作为首屏，只在 `onLoad` 里加超时 | 能修卡死，但每次冷启动都闪一下配对屏，已配对的设备也一样。分流是首屏该干的活。 |
| 配对屏读不到指纹时**直接放行** | 静默降级安全控制。保留人工勾选确认，是既有 TOFU 决策的最小改动，不是新机制。 |
| 加 tabBar（对齐 WorkBuddy 声称的底部导航） | 需要一套图标资源，我们没有；且文档里所有入口本就在抽屉。造不出图标就别造 tabBar。 |
| 把设置页做成 WorkBuddy 那样的大而全 | 主题只发布暗色一档，做主题开关是假的；记忆库、分享日志、意见反馈我们都没有。 |
| 修 `relay` 补 CORS 头 | **应该修，但本次没动**：`apps/relay/src/server.js` 正在被在线更新的并行开发改动，避免冲突。需求写进本笔记的 Consequences。 |

## Consequences

- **得到**：任何启动失败都变成一屏可读的错误 + 一个「重试」，不再有静默白屏；
  首屏不依赖网络；界面层次规则统一且无描边；七页真机可达（原先 tasks /
  approval / call / settings 没有任何入口）。
- **得到（顺带）**：`core/http.ts` 是全端统一出网口，后续接 relay REST 全部
  走它，超时与错误分类不用每处重写。
- **仍然欠着（并行开发可接手）**：relay 的 `/health` 补
  `Access-Control-Expose-Headers: x-hermix-cert-sha256`，让自报指纹在允许
  读取该头的环境下真正可见。这**不是 H5 的事**——relay 代码注释里要求它是为了
  屏 1 的指纹核对能生效，属服务端一行配置。
- **仍然欠着**：凭据明文落盘（`uni.setStorageSync`）这个未决项**没有**因为
  本次重构而消失，`core/pairing.ts` 文件头的不变。设置页的「清空凭据并重新
  配对」是用户目前唯一的自助手段。
- **风险**：boot 页的 fatal 错误屏在 H5 无头环境里未做交互验证（该环境的
  点击事件根本不派发到 JS，见验证记录）；逻辑本身经 `vue-tsc` 与构建通过。
- **回滚**：本次只改 `apps/hm/`，与桌面端、`apps/relay` 无交集，`git checkout
  -- apps/hm` 即可整体回退。

## 验证记录

- `npm run build:h5` —— 通过，7 个页面 chunk 全部产出（`dist/build/h5/assets/
  pages-{boot,chat,pairing,tasks,approval,call,settings}-index.*.js`），
  easycom 自动引入的 `hm-shell` / `hm-empty` / `hm-step` 也各自成 chunk。
- `npm test` —— 42/42 通过（签名一致性、帧状态机、E2EE 往返未受影响）。
- `npm run typecheck`（`vue-tsc --noEmit`）—— 无错误。
- 浏览器实测（H5 构建 + 本地静态服务）：启动 → boot 直达主界面；未配对时主界面
  渲染「还没有连接电脑」卡片与置灰的输入栏；配对屏探针在超时内**收敛到确定状态**
  「连不上这个 relay」而不再无限转圈（这正是本次修的核心症状）；chat / tasks /
  approval / call / settings 五页均正常渲染真空态，无控制台错误。
- **未能验证**：抽屉展开、发送按钮等**点击交互**。该环境下裸
  `<button>` + `addEventListener('click')` 同样不触发（已用独立探针页确认），
  属测试环境限制而非代码缺陷；编译产物中 `@tap` 已正确落为 `onClick` 绑定。
