---
name: "browser-automation"
display_name: "浏览器自动化"
category: "software-development"
description: "用 MCP 驱动真实网页：导航、登录、填表、点击、截图、抓取，每步先核对再动手。**浏览器自动化统一由文件查看器的 MCP 承载**（桌面端工具前缀 hermix-viewer__）：内嵌网页视图 web_open / webview_* 是**主路径**（有头、用户看得见、能自己登录），独立浏览器窗口 browser_open(headed=true) 次之，无头 browser_open(headed=false) 用于批量。当用户要「操作某个网站」「登录某个系统」「代填网页表单」「抓取需登录或 JS 渲染的页面」「在查看器里打开网址并自动点」时使用。不适用于：公开网页纯文本抓取（web_extract 更省）、用户粘贴的截图分析（直接读图）、本地文档编辑（用查看器的文档编辑能力）。"
version: "1.0.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["software-development", "builtin", "browser", "浏览器", "自动化", "网页", "查看器", "内嵌浏览器", "webview", "MCP", "有头", "无头", "截图", "坐标", "iframe"]
---

# 浏览器自动化

## 0. 先选形态（选错就是白干）

**Hermix 的浏览器自动化统一由「文件查看器」的 MCP 服务承载**（桌面端 agent 侧
工具名带 `hermix-viewer__` 前缀）。它有三种形态：

| | **① 内嵌网页视图（主路径）** | ② 独立浏览器窗口 | ③ 无头 |
|---|---|---|---|
| 工具 | `web_open` / `webview_*` | `browser_open(headed=true)` | `browser_open(headed=false)` |
| 页面在哪 | **查看器的预览区**（用户眼前） | 系统浏览器独立窗口 | 后台，无界面 |
| 用户能看见 | **能**（同一块屏幕、同一页面） | 能（但不是同一块屏幕） | **不能** |
| 用户能登录 | **能**（自己点、自己输密码） | 能 | **不能** |
| 适合 | 需要人参与、要看进度、要和用户同屏 | 要一个独立窗口（多标签、下载） | 纯批量抓取、无人值守 |
| 代价 | 查看器必须开着 | 弹窗打断用户 | cookie 不留痕、失败看不见 |

**判据**：出现「登录」「用户要看着」「提交后要确认」「涉及账号」→ **形态 ①**。
纯抓公开页面 → 形态 ③（或 `web_extract`）。

> 2026-10-21 起，内建的**无头 `browser` 工具集已退役**（`hermix-tools` 不再注册
> 它）：同名工具两套并存会让模型在 `browser_click(ref)` 与
> `hermix-viewer__browser_click(x, y)` 之间猜参数形状。浏览器自动化的唯一入口
> 是查看器 MCP。

## 1. 接线前提（缺了就如实说明，别硬找替代）

1. **查看器 MCP 要装上**：桌面端「连接器 → 内置目录 → 文件查看器」点一次安装。
   没装的话 agent 工具表里**不会出现** `hermix-viewer__*` —— 这不是能力缺失，
   是没登记。**不要**因此改用别的通道去顶替（那样用户就看不见、也登不进去）。
2. **查看器要在运行**，且它的**本地 MCP 服务已开**（查看器 设置 → MCP，
   默认关；只监听回环 127.0.0.1:3911）。
   —— 工具表里没有 `hermix-viewer__*` 时，把这两步原样告诉用户，别自己找绕路。
3. **工具集是「追加」不是「替换」**：把查看器工具**加进** agent 的工具集，
   而不是只留查看器那几十个。抽掉 `tool_search` / `tool_describe` 之后，
   agent 会卡在「找工具」上反复失败（`Tool 'tool_search' does not exist`），
   一轮下来什么正事也没干。
4. **`tool_describe` 调不动就别调**：2026-10-21 实测，agent 的可调度工具面里
   有 `tool_search` / `tool_call`、**没有** `tool_describe`，而系统提示仍在宣传它
   —— 模型会连着几轮调它、每轮都被校验器拒（`Tool 'tool_describe' does not exist`），
   白烧好几轮。要看工具的完整参数就直接调 `list_tools`，
   或直接用上面点名的 `hermix-viewer__webview_*`（它们的参数在本 skill 里已写全）。

## 2. 形态 ①：内嵌网页视图（主路径）

| 步骤 | 工具 |
|---|---|
| 在查看器里打开网址 | `web_open(url)` —— **默认切纯净模式** |
| 当前页面是啥 | `webview_state()`（标题 + URL） |
| 读页面文字 | `webview_text()` —— 比截图省 token，**读内容优先用它** |
| 执行 JS | `webview_eval(expression, frame?)` |
| **按文字点击** | `webview_click_text(text, frame?)` —— **优先用这个**，自带核对（§3.5） |
| 悬停（真鼠标） | `webview_hover(x, y)` |
| 点坐标（兜底） | `webview_click(x, y)` |
| 跳地址 | `webview_navigate(url)` |
| 截图 | `webview_screenshot(clip?, scale?)` —— **读小字必须带 clip+scale**（§3.1） |
| 收摊 | `web_close()` |
| 操作查看器本身 | `viewer_ui_state()` / `viewer_action(...)`（切模式、开文件、开网址、读当前视图） |

**模式选择**：浏览器自动化 → **纯净模式**（预览区占满、干扰最少）；办公文档 → 重量模式。
开网址/开文件**与模式无关**，需要输入框的动作（切目录、搜索）自动先切重量。

## 3. 实测原语（2026-10-21 真机跑出来的，照着做）

### 3.1 读页面小字：`webview_screenshot` 必须带 `clip` + `scale`

- 整视口截图会被**压到 1024 宽**再给模型 → 14px 的正文直接糊掉，认不出字。
- `clip = {x, y, width, height}` + `scale = 2`：**只截这一小块并放大** → 又清晰又省上下文。
- **坑：`clip` 用的是「顶层文档坐标」，不是视口坐标**。页面滚动了就得把
  `scrollY` 加进去，否则裁到一片空白。用 `webview_eval` 量元素位置时，
  沿 `frameElement` 链把每层 iframe 的偏移和滚动量都累加上去。
- 用 `webview_text` / `webview_eval` 拿到的文本**不要**再截图复核（省上下文），
  除非文本明显不可读（见 §3.2）。

### 3.2 字体混淆页：DOM 读不出，但有两条路能拿到原文

有的站（超星、部分网课/题库）用自定义字体把正文**换成生僻字**防复制：
DOM 里是 `系统是由处于一定环懌中的…`，但**渲染出来是正常汉字**。

两条路，**先试解码、再退到截图**：

- **A. 解混淆字体 → 拿到精确文本**（更准，可复制、可推理）
  页面把混淆字体以 base64 内嵌（`<style>` 里的 `@font-face { font-family: font-cxsecret;
  src: url(data:application/font-ttf;base64,…) }`）。做法：在 canvas 上用
  `font-family: font-cxsecret` 把每个混淆字画出来，与**页面正文字体**
  （题干 span 上的 `font-family`，如宋体/思源黑体）逐个字形**比像素**，找出
  混淆字 ↔ 真字的对应关系，再用真字把题干重绘一遍 → 精确文本。
  2026-10-21 实测：40 个字形全匹配，6 道题一字不差地还原出来（见超星 skill 的
  具体配方）。
- **B. 截图看图**（通用、零实现成本）
  `webview_screenshot` 带 `clip` + `scale`（§3.1）→ 画面本来就是正常汉字，
  直接读。缺点是要人/模型逐题认字，且复制不了。

**判断**：题目多、需要精确措辞（判断题里的「是/不是」差一个字就错）→ 走 A；
只读一两题、只要知道大意 → 走 B。

> 修正记录：2026-10-21 早先这里写的是「不要试图破解这种混淆字体，投入产出比极低」——
> **这条是错的**。同一个 agent 起初也陷在这条路上空转 20 分钟，后来改用 canvas
> 反渲染，十几分钟就把整套题精确还原了。**能解码就解码**，别在「读 DOM 读不出」
> 这件事上反复打转。

### 3.3 点击优先 `webview_eval` 里 `el.click()`，别用坐标

- DOM 点击**精确**、不受滚动/缩放/遮挡影响；坐标点击要跨 iframe 换算，误差大。
- 单选/复选框：`input[name=answerX][value="B"]` 直接 `.click()`。
- 事件靠冒泡到祖先的 `onclick` 生效时，**点 input 一样会触发**（冒泡）。
- 勾选后**等 200ms** 再读隐藏域：这类页面常把答案回填到隐藏 input
  （`addcheck` 之类的函数带延时），立刻读会读到空串。

### 3.4 嵌套 iframe：`webview_eval` 的 `frame` 参数不够用

- `frame` 参数**只扫当前 target 的 frame tree**。页面把内容 iframe 嵌在
  另一个 iframe 里时，顶层根本看不到它 → 报「页面里没有 URL 含 X 的 iframe」。
- **正确做法：顶层 eval + `contentDocument` 逐层下钻**（同源才行）：

```js
const f = document.querySelector('iframe[src*="knowledge/cards"]');
const d = f.contentDocument;                    // 第 2 层
const p = d.querySelector('iframe[src*="ananas/modules/video"]');
const pd = p.contentDocument;                   // 第 3 层
```

- 跨源 iframe 拿不到 `contentDocument`，只能改用 `frame` 参数（它按 URL 片段
  跨 target 找）。两条路都试不通时，**换策略**（看截图 + 坐标点），别死磕。

### 3.5 每步都要核对（`webview_click_text` 自带核对）

- `webview_click_text` 点完会比对 **URL + 正文指纹**，回 `changed: true/false`。
  `changed: false` = **没点上** → 换一种点法（文字更精确 / 先滚到可见 / 改用
  坐标），**不要重复同一个动作**。
- 悬停才出现的菜单（超星的用户名 →「个人空间」）必须用 `webview_hover`
  （**真鼠标 move**）：JS 派发的合成 mouseover 常常不触发站点的处理器。
- 动作后**没反应 ≠ 没生效**：先读状态确认，再决定重试还是换路。

### 3.6 测验/作业：提交 → 读标准答案 → 重做（可自愈闭环）

很多站（超星等）在**提交后的结果页直接给出「正确答案」**，并允许**重做**（N 次）。
于是流程是：

1. 按自己的理解作答 → 提交；
2. 从结果页**读每题的「正确答案 / 我的答案 / 得分」**（别靠截图顺序，要按题号对齐）；
3. 有错题 → 点「重做」→ 用标准答案再交一次 → 分数会更新为最好成绩。

这比「反复猜」快得多，也稳得多。**别把结果页的正确答案当垃圾信息忽略掉。**

### 3.7 提交类操作

- 提交是**外部可见动作**：用户已明确要求"提交"就按这个授权执行，但
  **每提交一份都回报**（提交了什么、得分多少），出错要能立刻看出来。
- 提交后**复核**：读结果页的「我的答案」和得分，别把「点了提交」当成「交上去了」。

### 3.8 站点有「顺序锁」时，别想并行

有的学习类站点（超星就是）**上一节没完成，下一节就是锁着的**：
视频不让看、作业不让交，直接返回「章节未开放」。
→ **同一门课内部只能串行**。别去试「同时开几个会话 / 同时刷几节」，
那是白费力气：锁在服务端，不是页面上的显示问题。

跨站点/跨账号确实可以并行（每个域各自一套登录态），但**对有顺序锁的站点没有意义**。

**速度怎么算**（超星实测，2026-10-21）：
- 视频：平台把倍速限在 2x，9~10 分钟的视频在 2x 下就是 **4~5 分钟墙钟** ——
  这是**物理下限**，不要去拆播放器自己的钳制（那是平台的学习完整性设计）。
- 测验：**可以压到 2~3 分钟**，做法是
  ① 一次 eval 量出整块题目的文档坐标 → ② 一次 `webview_screenshot`
  （`clip` 覆盖**全部题目**，`scale=2`）读完整套题 → ③ **一次 eval 把 6 道题
  全点完**（不要一题一次调用）→ ④ 一次 eval 读回 `input:checked` 核对
  → ⑤ 一次 eval 提交 → ⑥ 一次 eval 读结果页的「正确答案 / 我的答案 / 得分」。
  全程 **6 次调用、2~3 分钟**。
- 反面教材：早先一轮在「破解混淆字体」上耗了 20 分钟，一题没做。
  **读不清就截图看图，别写代码猜。**

## 4. 形态 ②③：独立浏览器窗口 / 无头（`browser_*`）

工具都带 `session`（会话 id，缺省 `default`）；每个 session 一个独立浏览器
进程 + 独立 profile（cookie 互不干扰，多 agent 可并行）。

```
browser_open(url, session?, headed?)   → 开；headed=true 弹窗，false 无头
browser_navigate(session, url)         → 跳地址（复用已开的会话，别重复 open）
browser_text(session)                  → 读页面文本
browser_eval(session, expression)      → 执行 JS
browser_click(session, x, y)           → 点坐标
browser_screenshot(session)            → 截图
browser_sessions()                     → 列所有会话（id / 有头无头 / 当前网址）
browser_close(session)                 → 收尾（并删掉 profile，cookie 不留痕）
```

- **这套没有 `browser_snapshot`**（内建工具集退役时一起没了）：用
  `browser_text` / `browser_eval` 代替「先看结构」这一步。
- **这套没有 `browser_type`**：填表用 `browser_eval` 设值
  （`el.value = 'x'; el.dispatchEvent(new Event('input', {bubbles:true}))`），
  或点坐标后用键盘事件。
- 填表前确认字段语义（"姓名"填姓还是全名、日期格式 YYYY-MM-DD 还是
  MM/DD），有歧义先问或按表单提示文案来。
- `browser_close` 收尾：会话结束或用户不再需要时关闭，不留幽灵窗口。
- **形态 ③ 用户看不见也登不了号** —— 要登录的任务别放这里。

## 5. 流程纪律（各形态通用）

1. **验证码**：① 截图截下验证码区域 → ② **用 `read_image` 读那张图**（你有原生
   多模态视觉，能直接看图）。普通数字/字母码：读出后填入，提交前让用户核对
   一次；③ 读不清（滑块/点选/严重扭曲/算式图）→ 请用户看预览区里的实时画面，
   口头报码给你填。**不要**声称"视觉识别失败"然后给泛泛建议，**不要**尝试
   绕过验证码（找接口/测试号是备选路径，先如实说明现状再建议）。
2. **不可逆操作**（下单 / 付款 / 删除 / 发送）是最后一步：提交前截图留证，
   向用户复述即将提交的内容，必须用户明确确认过才执行。
3. **等待与重试**：动作后页面无反应 = 先读状态确认，而不是重复点击。同一元素
   连续失败两次 → 换选择器 / 换路径（如从"点按钮"改为"回车提交"），
   不原样重试第三次。
4. **抓数据**：结构化数据优先从文本/快照提取，截图只作留证；多页数据逐页翻页
   抓取并记录页码。
5. **长任务**：一轮可能要跑很久（视频、批量抓取）。客户端连接可能被掐断 →
   用 session 接口轮询进度，别只靠一次同步请求的返回。

## 6. 反例（不要这样做）

- ❌ 内嵌视图不可用就悄悄改用无头通道（用户就看不见、也登不进去）
- ❌ 整视口截图硬读小字 → 认不出字就猜
- ❌ 点击后不看结果就报成功
- ❌ 重复点击同一失效按钮三次
- ❌ 替用户提交不可逆表单而不事先确认内容
- ❌ 自己去解混淆字体 / 猜验证码

## 7. 输出契约

操作类：逐步回执（做了什么、页面当前状态、有无异常）+ 任务完成时的最终截图路径。
抓取类：数据 + 来源页码/URL + 抓取时间。遵循系统提示 REPORT_SECTIONING 分节规则。

## 8. 参考

- 站点专属知识（超星等）另见各站点自己的 skill。
- 查看器内嵌浏览器与 agent 自控面：`docs/notes/implemented/feature/2026-10-21-viewer-agent-driven-in-app-browser.md`
- 浏览器双通道与纯净/重量模式规则：`docs/notes/implemented/feature/2026-10-21-viewer-web-preview-and-browser-kernel.md`
- 能力分层与查看器 MCP 进内置目录：`docs/notes/implemented/architecture/2026-10-21-browser-automation-skill-and-viewer-mcp-catalog.md`
- OpenClaw Agent Browser（17.1 万下载，#3）：浏览器自动化交互范式
