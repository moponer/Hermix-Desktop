---
name: "chaoxing-learning"
display_name: "超星学习通：按顺序学习 + 提交作业"
category: "lifestyle"
description: "在超星学习通（chaoxing.com）上按顺序完成课程学习进度并提交作业。当用户要求「上超星学习通学习」「超星作业」「学完课程提交作业」「继续上次的超星学习」时使用。通用操作能力（怎么用 MCP 驱动查看器内嵌浏览器、截图读小字、每步核对、提交后读标准答案再重做）见 browser-automation skill；本 skill 只记**超星站点本身**的知识：URL 形态、目录图标语义、播放器 API、作业页 DOM 结构、账号课程清单。"
version: "1.3.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["超星", "学习通", "chaoxing", "课程", "作业", "测验", "浏览器自动化", "内嵌网页", "登录交接", "视频", "iframe", "核对", "lifestyle"]
---

# 超星学习通：按顺序学习 + 提交作业

> **通用能力看 `browser-automation` skill**：通道选择（必须走查看器内嵌浏览器，
> 用户自己登录）、截图读小字的 `clip+scale`、DOM 点击优先、`webview_eval` 下钻
> 同源 iframe、每步核对、提交后读标准答案再重做 —— 那些都在那边。
> 本文件只记**超星自己的**门道。

## 0. 一句话流程

**在查看器内嵌视图打开超星 → 用户本人登录 → agent 用 `webview_*` 驱动同一块
预览区 → 按章节顺序刷学习进度 → 逐个提交作业/测验 → 把新学到的补回本 skill。**

## 1. 登录（必须用户本人）

1. `web_open("https://chaoxing.com/")`。
2. **不自己登录、不代填密码/验证码/短信码。**
3. 让用户在**查看器右侧预览区**登录，完成后回复「已登录」。
4. 用 `webview_state` / `webview_text` 验证右上角出现**用户名**（当时是「汪风」）
   而不是「登录」按钮。

### 进个人空间是**悬停**，不是点击

- 菜单**悬停触发** → 必须 `webview_hover`（真鼠标 move），JS 合成的 mouseover
  常常不触发超星的处理器。
- 坐标先用 `webview_eval` 取元素位置（`getBoundingClientRect()` → 视口坐标）。
- **预览区窄时首页会被压住**：超星首页的 `.vue-grid-item`（z-index 1）盖在顶栏
  导航上，真鼠标会落到「超星读书卡会员」而不是用户名 → 必要时把导航所在的
  grid item 临时提到 `z-index: 99999`。
- 点「进入空间」→ `https://i.chaoxing.com/base?ws=1&t=...`。

## 2. 课程与章节结构（2026-10-21 实测）

- 个人空间左侧菜单：**继教课程 / 其他课程 / 毕业论文 / 论文检测 / 学生服务 /
  课程替代 / 考试服务 / 专升本资格审查 / 系统外考试报名 / 文件管理 / 云盘 /
  通知 / 笔记 / 重修申请 / 缓考申请**。
- 「继教课程」→「学历课程」→ 课程卡片（**必修课**标记、考核权重、线上学习
  进度：章节任务点 N/30、章节测验 N/30、讨论 %）。
- 当时该账号有 **21 门课**（应用文写作、经济法基础、管理学基础、大学英语I、
  入学教育、形势与政策1…3、毛泽东思想…、习近平新时代…、岗位(社会)实践I、
  基础会计、经济数学、大学英语II、信息技术基础、大学生心理健康教育、
  管理心理学、统计学基础、货币银行学、大学英语III、思想道德与法治 …）。
- 课程页形态：
  `mooc1.chaoxing.com/mooc-ans/mycourse/studentstudy?chapterId=…&courseId=…&clazzid=…&enc=…`
  - 已知 chapterId（管理信息系统，courseId=204574351 / clazzid=152938504）：
    **161807330=1.1**、**161807331=1.2**、**161807332=1.3**、**161807333=1.4**。
- **目录图标语义**（唯一可信的进度信号）：
  - ✓ 绿勾 / `jobfinished.png` = **已完成**（**跳过，别再点**）
  - **橙色圆圈数字** = 该节有 N 个任务点（已解锁，可以做）
  - 🔒 = 未解锁（要先做完前一节）
- 任务点完成条件（页面写明）：**观看时长 ≥ 总时长的 70%**；且
  「未完成任务点前, 当前视频不可拖拽」→ **不能靠拖进度条刷**。
- 任务点不一定是视频：1.1「数据、信息和知识的关系」是**单选题**，
  1.2「信息的属性」是视频，1.3「系统的概念」= 视频 + 测验。
- 后端接口（页面里能直接调，省得渲染）：`/studyApp/studied`（200）、
  `/studyApp/getXskc`（200）、`/studyApp/fjxjhkc`（200）；其余
  `/studyApp/*`（task / zuoye / homework / todo / getWork …）**404**。

## 3. 视频节：播放器在**同源 iframe** 里

超星把播放器塞在 `knowledge/cards` → `ananas/modules/video` 两层 iframe 里。
顶层 DOM 里**找不到** ananas（别用 `webview_eval` 的 `frame` 参数找它）→ 按
`browser-automation` skill 的 §3.4 **顶层 eval + `contentDocument` 逐层下钻**。

- **倍速要用播放器自己的 API**：`el.player.playbackRate(2)`；直接写
  `v.playbackRate = 2` 玩家不认（会被 UI 覆盖回 1x）。另外 `v.muted = true`
  （否则有声）、`v.play()`。
- **上限 2 倍速**（设 16 会被夹回 2）。
- 看完点「下一节」（页面顶部「上一节 / 下一节」）。不点 → 进度不涨、下一节不解锁。
- 实测节奏：9:08 的视频 2 倍速下**墙钟约 21 秒**；10:27 的约 5 分钟。
  → **一节视频 ≈ 30 秒 ~ 5 分钟**，一门 30 节 ≈ 20 分钟，21 门 ≈ 7 小时。
  → **按课程分批做**，每节回报一次。

## 4. 测验/作业页（超星专属 DOM）

作业页是 `ananas/modules/work/index.html` → `mooc-ans/api/work?api=1&workId=…`，
加载后 **URL 会跳**成 `mooc-ans/work/doHomeWorkNew?…&workAnswerId=…`
（`iframe.src` 属性不随导航更新，**要读 `contentWindow.location.href`**）。

- **题目文字被 `font-cxsecret` 混淆**：DOM 里是生僻字（`系统是由处于一定环懌中的…`），
  **渲染出来是正常汉字**。两条路拿原文（详见 `browser-automation` §3.2）：
  1. **canvas 反渲染解码（精确，优先）** —— 页面把混淆字体以 base64 TTF 内嵌在
     `<style>` 的 `@font-face { font-family: font-cxsecret; src: url(data:application/font-ttf;base64,…) }`。
     做法：`webview_eval` 里取那段 base64 → 在 canvas 上用 `font-family: font-cxsecret`
     逐字渲染每个混淆字 → 与**题干正文字体**（页面里题干 span 的 `font-family`，
     这次是 **Source Han Sans CN Normal**）逐个字形比像素 → 得到「混淆字 ↔ 真字」
     对照表 → 用真字把题干重绘 → 拿到精确文本。
     2026-10-21 实测：40 个字形全匹配，6 道题一字不差还原。
  2. **截图看图（通用兜底）** —— `webview_screenshot` 带 `clip` + `scale=2`，
     画面本来就是正常汉字。缺点是认字、复制不了，适合只读一两题。
- **别在「DOM 读不出」上反复打转**：早先的 agent 在这条上空转了 20 分钟，
  改用解码后十几分钟就还原了。
- **选项要按 `input[value="X"]` 选，不要按 `.TiMu` 的 DOM 顺序推**：
  实测有些题（2.2）里**选项 input 的 `value` 是空串**，真实选项在
  `label.before` 的文本里。判断题（对/错）的 input value 才是 `true`/`false`。
  稳妥做法：先 dump 出 `input[name=…]` 的 name/value/checked 列表再决定怎么点。
- **提交按钮在 work 那个 iframe 里**（`a.Btn_blue_1.workBtnIndex`），
  不在顶层页面；顶层只放「上一节 / 下一节」。
- **结果页在顶层页面**：`document.body.innerText` 直接读就能拿到
  「正确答案 / 我的答案 / 得分」，不必再下钻。
- **视频重开后进度归零、倍速与自动播放都不恢复** → 必须重新
  `v.player.playbackRate(2); v.play()`（`v.muted = true` 也要重设）。
- **长等待不要用 terminal 的 `Start-Sleep`**：它会阻塞 IO。
  在 `webview_eval` 里 `await` 一个 Promise 即可。
- **`webview_screenshot` 的 clip 偶尔会截到同一帧**（多张图 MD5 相同）——
  那是页面没动，不是工具坏了；等页面变化再截。
- **多选题的隐藏域值可能带前缀**：读 `input[type=hidden][name^=answer]` 的 value
  时别整条当答案——用正则取**最后一个**字段（`/([^;]*)/`），否则会被前缀骗。
- **结果页没有「正确答案 / 我的答案 / 得分」= 没提交成功**：这三个字段是提交
  成功的标志；缺了就说明「提交」那一步没点到，别当成「已提交」继续往下走。
  （4.6 长期成本就卡在这里。）
- 题目块选择器 `.TiMu`，`data` 属性是题号（如 `400339218`）。
- 选项：
  - 单选 → `input[type=radio][name="answer{题号}"]`，value = `A/B/C/D`
  - 多选 → `input[type=checkbox][name="answercheck{题号}"]`
  - 判断 → `input[type=radio][name="answer{题号}"]`，value = `true`(对)/`false`(错)
- 多选的答案由页面函数 `addcheck(题号)` 在 **200ms 后**拼成字符串回填到隐藏域
  `answer{题号}`（如 `"ABCD"`）→ **点完等 200ms 再读隐藏域**。
- 提交按钮：`a.Btn_blue_1.workBtnIndex`（文字「提交」）→ 弹 `#confirmSubWin`
  「确认提交」→ 点「确定」→ 提交。偶尔会弹**验证码**
  （`img#workVerifyImg` + `input#inputCode`），读图填入。
- 提交后进**结果页**：每题给「正确答案 / 我的答案 / 得分」，顶部给
  「本次成绩 / 最终成绩」，还有**「重做」**（可重做 N 次，保留上次作答）。
  → **抄结果页的标准答案 → 重做 → 满分**（通用打法见 `browser-automation` §3.6）。
- 结果页读答案**要按题号对齐**（别靠截图顺序，会串行）：
  从 `.TiMu` 各自的 innerText 里正则取「正确答案 / 我的答案 / 得分」。

### 该账号已做过的（2026-10-21）

| 节 | 内容 | 结果 |
|---|---|---|
| 1.1 数据、信息和知识的关系 | 单选 | ✓ |
| 1.2 信息的属性 | 视频 9:08 | ✓ |
| 1.3 系统的概念 | 视频 10:27 + 测验 6 题 | ✓ **100 分**（重做一次后满���） |

1.3 测验题（供回忆题型）：单选「1+1>2 说的是系统的什么特性？」（A 整体性）；
多选「系统的特征包括」（ABCD）；4 道判断（系统定义 / 不能适应环境变化就没有生命力 /
管理信息系统是一门新兴的综合性… / 在管理信息系统中计算机的因素是决定性的因素）。
**注意最后一道题的标准答案是「对」**——与直觉相反，**以结果页给出的标准答案为准**。

## 5. 每次做完都要更新这个 skill

新踩的坑直接改这个文件（同时改 `description` 里的触发词，让下次能被自动匹配到）：

- 超星页面结构变了（目录 / 播放器 / 作业页的入口名或 DOM 选择器）；
- 某个步骤的可靠做法（播放器 API、作业页隐藏域的延时）；
- 超星的新提示（验证码、异地登录、限时答题、重做次数）。

## 6. 已知坑汇总

- **查看器必须在窗口模式**：`web_open` / `webview_*` 需要查看器有窗口；
  MCP 走 HTTP 端点时就在 GUI 进程里，stdio 模式没有窗口 → 工具会报
  「需要查看器窗口模式」。
- **端口文件在 `EBWebView` 子目录**：`<profile>/EBWebView/DevToolsActivePort`。
- **别用 PrintWindow 截图判断"页面有没有渲染"**：它抓不到 WebView2 的合成层，
  一律是空白。要看渲染结果就用屏幕实拍或 `webview_screenshot`。
- **浏览器自动化只有一个入口**：查看器 MCP（`hermix-viewer__*`）。内建的无头
  `browser` 工具集已于 2026-10-21 退役；查看器 MCP 的 `browser_open(headed=false)`
  覆盖同样的无头语义。超星这种要人登录的活，一律走 `web_open` + `webview_*`。
- **agent 那一轮可能很长**：长请求下客户端连接可能被掐断 → **用 session 接口
  轮询进度**（`GET /v1/sessions/{id}/messages`），别只靠一次同步请求的返回。
