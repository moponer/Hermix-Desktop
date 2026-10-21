---
name: "chaoxing-learning"
display_name: "超星学习通：按顺序学习 + 提交作业"
category: "lifestyle"
description: "在超星学习通（chaoxing.com）上按顺序完成课程学习进度并提交作业。当用户要求「上超星学习通学习」「超星作业」「学完课程提交作业」「继续上次的超星学习」时使用。核心要点：网页要在**查看器内嵌视图**里打开（用户在同一块预览区登录与旁观），agent 用 hermix-viewer__webview_* 工具驱动同一个页面；登录必须由用户本人完成；视频在**同源 iframe** 里要用 contentDocument 逐层下钻；播放器最高 2 倍速；每一步都要核对结果。"
version: "1.2.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["超星", "学习通", "chaoxing", "课程", "作业", "浏览器自动化", "内嵌网页", "登录交接", "视频", "iframe", "核对"]
---

# 超星学习通：按顺序学习 + 提交作业

## 0. 一句话流程

**用查看器的内嵌网页视图打开超星 → 用户本人登录 → agent 用 `webview_*`
驱动同一块预览区 → 按章节顺序刷学习进度（视频真看到 70%）→ 逐个提交作业 /
测验 → 把新学到的操作补回本 skill。**

## 1. 为什么必须在「查看器里」开浏览器

超星是重交互的 SPA：登录、目录树、视频播放器、作业编辑器全是前端状态。
- **系统浏览器通道**（`browser_*` 工具）弹一个**独立窗口**：用户与 agent 盯
  的**两个窗口**，登录完还要口头交接。
- **内嵌网页视图**（`web_*` / `webview_*`）渲染在**查看器的预览区**：用户和
  agent 盯的是**同一块屏幕、同一个页面、同一个登录态**。

2026-10-21 实测：内嵌视图开了 CDP 调试端口（`--remote-debugging-port=0`，
真实端口写在 profile 的 `DevToolsActivePort` 里，**在 `EBWebView` 子目录下**），
agent 用同一份 CDP 客户端就能驱动它。

## 2. 工具对照（桌面端 agent 侧带 `hermix-viewer__` 前缀）

| 步骤 | 工具 |
|---|---|
| 在查看器里打开超星 | `web_open`（参数 `url`）—— **默认切纯净模式** |
| 当前页面是啥 | `webview_state`（标题 + URL） |
| 读页面文字 | `webview_text`（比截图省 token，**优先用它**） |
| 执行 JS | `webview_eval`（参数 `expression`） |
| **按文字点击** | `webview_click_text`（参数 `text`）—— **优先用这个**，见 §6.0 |
| 悬停 | `webview_hover`（参数 `x` `y`，**真鼠标**） |
| 点坐标（兜底） | `webview_click`（参数 `x` `y`） |
| 跳地址 | `webview_navigate`（参数 `url`） |
| 截图 | `webview_screenshot`（image/jpeg 内容块 + 落盘路径） |
| 收摊 | `web_close` |
| 操作查看器本身 | `viewer_ui_state` / `viewer_action` |

## 3. 登录必须交给用户（硬规则）

1. `web_open` 打开 `https://chaoxing.com/`。
2. **不要**自己尝试登录，**不要**代填账号 / 密码 / 验证码 / 短信码。
3. 明确告诉用户：**请在查看器右侧的网页预览区里登录**，登录完回复「已登录」。
4. 用户确认后，用 `webview_state` / `webview_text` **验证确实登录了**
   （页面右上角出现**用户名**而不是「登录」按钮）。
5. 没登录成功就请用户再看一眼页面上的站点提示，**不要**猜密码或重试。

## 4. 进个人空间（悬停，不是点击）

登录后首页右上角是**用户名**（如「汪风」），**鼠标悬停**才弹出菜单
「进入空间 / 退出登录」。

- 菜单是**悬停触发**的 → 必须用 `webview_hover`（真鼠标 move），JS 派发的
  合成 mouseover 不一定能触发超星的处理器。
- 坐标先用 `webview_eval` 取元素位置：`getBoundingClientRect()` → 视口坐标
  （`rect.left + rect.width/2`）。
- **预览区窄的时候首页会被压住**：超星首页的 `.vue-grid-item`（z-index 1）
  盖在顶栏导航上，真鼠标会落到「超星读书卡会员」而不是用户名 → 必要时把导航
  所在的 grid item 临时提到 `z-index: 99999`。
- 点「进入空间」→ `https://i.chaoxing.com/base?ws=1&t=...`（个人空间）。

## 5. 课程与章节结构（2026-10-21 实测）

- 个人空间左侧菜单：**继教课程 / 其他课程 / 毕业论文 / 论文检测 / 学生服务 /
  课程替代 / 考试服务 / 专升本资格审查 / 系统外考试报名 / 文件管理 / 云盘 /
  通知 / 笔记 / 重修申请 / 缓考申请**。
- 「继教课程」→「学历课程」→ 课程卡片（**必修课**标记、考核权重、线上学习
  进度：章节任务点 N/30、章节测验 N/30、讨论 %）。
- 当时该账号有 **21 门课**（应用文写作、经济法基础、管理学基础、大学英语I、
  入学教育、形势与政策1…3、毛泽东思想…、习近平新时代…、岗位(社会)实践I、
  基础会计、经济数学、大学英语II、信息技术基础、大学生心理健康教育、
  管理心理学、统计学基础、货币银行学、大学英语III、思想道德与法治 …）。
- 课程页 `mooc1.chaoxing.com/mooc-ans/mycourse/studentstudy?chapterId=…&courseId=…&clazzid=…&enc=…`。
- **目录图标语义**（唯一可信的进度信号）：
  - ✓ 绿勾 / `jobfinished.png` = **已完成**（**跳过，别再点**）
  - **橙色圆圈数字** = 该节有 N 个任务点（已解锁，可以做）
  - 🔒 = 未解锁（要先做完前一节）
- 任务点完成条件（页面写明）：**观看时长 ≥ 总时长的 70%**；且
  「未完成任务点前, 当前视频不可拖拽」→ **不能靠拖进度条刷**。
- 任务点不一定是视频：1.1「数据、信息和知识的关系」是**单选题**（共 2 题、
  33.2 分），1.2「信息的属性」才是视频。
- 后端接口（页面里能直接调，省得渲染）：`/studyApp/studied`（200）、
  `/studyApp/getXskc`（200）、`/studyApp/fjxjhkc`（200）；其余
  `/studyApp/*`（task / zuoye / homework / todo / getWork …）**404**。

## 6. 刷学习进度（视频节）的正确打法

### 6.0 先立规矩：每一步都要**核对**，不许闷头往下走

2026-10-21 实测的教训：agent 点「1.2 信息的属性」**点了但页面没跳**，它却
以为点成功了，接着在错误的章节上干活 —— **而且不会自己发现**。所以：

1. **点击一律用 `webview_click_text`（按文字）**：它按文字找到真正可点的元素、
   点完自动比对 URL 与正文指纹，回 `changed: true/false`。`changed: false` =
   **没点上**，必须换一种点法（文字更精确 / 先滚到可见 / 用坐标点），
   **不要重复同一个动作**。
2. **章节推进前先读目录状态**：✓ 已完成的**跳过**，橙色数字的才是当前可做的。
3. **每节做完核对两件事**：目录图标变成 ✓ 了没有？「章节任务点 N/30」数字
   涨了没有？数字没涨 = 没生效，别当成做完了。
4. 点完「下一节」要核对 `chapterId` 变了没有（变了才算真的跳了）。

### 6.1 视频在**同源 iframe** 里：用 `contentDocument` 逐层下钻

**坑 1（最坑）**：`webview_eval` 的 `frame` 参数**在超星这里没用** —— 顶层
页面里**没有** URL 含 ananas 的 iframe（ananas 播放器嵌在 `knowledge/cards`
里面，而 cards 又嵌在顶层）。**`frame` 参数只扫当前 target 的 frame tree。**

**正确做法**：顶层 eval + `contentDocument` 逐层下钻（超星这几个 iframe
**同源**，所以能直接拿）：

```js
// 第 1 层：顶层 → knowledge/cards（同源）
const f = document.querySelector('iframe[src*="knowledge/cards"]');
const d = f.contentDocument;
// 第 2 层：cards → ananas 播放器（也同源）
const p = d.querySelector('iframe[src*="ananas/modules/video"]');
const pd = p.contentDocument;
const v = pd.querySelector('video');
```

**坑 2：倍速要用播放器自己的 API**：`el.player.playbackRate(2)`（超星播放器
封装的），直接写 `v.playbackRate = 2` 玩家不认（或者被 UI 覆盖回 1x）。
另外 `v.muted = true`（否则有声）、`v.play()`。

**坑 3：播放器最高 2 倍速**（设 16 会被夹回 2）。

**坑 4：看完要点「下一节」**（页面顶部「上一节 / 下一节」）。不点 → 进度不涨、
下一节不解锁（章节会一直卡在橙色）。

**实测节奏（2026-10-21，1.2「信息的属性」）**：视频总时长 9:08（548.72s），
实际看到 8:01（481s，88%），2 倍速下**墙钟约 21 秒**（3 秒一次轮询，
`v.ended` 为止）→ **一节视频 ≈ 30 秒**，一门 30 节 ≈ 20 分钟，21 门 ≈ 7 小时。
→ **按课程分批做**，每节回报一次。

## 7. 提交作业 / 测验

- 章节任务点可能是**单选题**（1.1：共 2 题、33.2 分），也可能是视频。
  单选题在左侧内容区，直接选答案提交；页面上会显示「正确答案 A / 我的答案 A」。
- 提交前用 `webview_text` **复核**填了什么（别把「已提交」当成「填好了」）。
- 提交是**外部可见动作**：用户已明确要求"提交作业"就按这个授权执行，但
  **每提交一份都回报**（作业名 + 提交了什么），出错要能立刻看出来。

## 8. 每次做完都要更新这个 skill

新踩的坑直接改这个文件（改 `description` 里的触发词，让下次能被自动匹配
到），常见要补的：

- 超星页面结构变了（目录 / 播放器 / 作业页的入口名或 DOM 选择器）；
- 某个步骤的可靠做法（例如「同源 iframe 用 contentDocument 逐层下钻」、
  「倍速用 player.playbackRate(2)」）；
- 超星的新提示（验证码、异地登录、限时答题）。

## 9. 已知坑汇总

- **查看器必须在窗口模式**：`web_open` / `webview_*` 需要查看器有窗口
  （MCP 走 HTTP 端点时就在 GUI 进程里；MCP stdio 模式没有窗口 → 工具会
  报「需要查看器窗口模式」）。
- **端口文件在 `EBWebView` 子目录**：`<profile>/EBWebView/DevToolsActivePort`。
- **别用 PrintWindow 截图判断"页面有没有渲染"**：它抓不到 WebView2 的
  合成层，一律是空白。要看渲染结果就用**屏幕实拍**或 `webview_screenshot`。
- **桌面端 agent 自己的 `browser_*`（hermix-tools 那套）是无头 Edge**：
  用户看不见、也不能登录。超星这种要人登录的活，一律走 `webview_*`。
- **agent 那一轮可能很长**（一节视频 30 秒、几门课就是几十分钟）：长请求下
  客户端连接可能被掐断 → **用 session 接口轮询进度**（`GET
  /v1/sessions/{id}/messages`），别只靠一次同步请求的返回。
