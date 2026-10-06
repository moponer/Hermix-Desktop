---
name: "impeccable-design"
display_name: "界面与视觉设计规范"
category: "design"
description: "用于：界面设计、UI 重设计、美化、审查反模式、配色、排版、布局优化，以及 PPT/幻灯片、海报等平面视觉产物的 HTML/CSS 实现。务必在以下场景使用：用户说「界面设计」「做个页面」「美化一下」「配个色」「排个版」「做 PPT」「检查 UI」「优化视觉」。不适用于纯后端或非视觉任务。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["design", "builtin"]
---

# 界面与视觉设计规范（impeccable）

做设计类任务时按以下纪律执行。

## 设计原则

1. **大胆、完整、有立场**：交付物必须完整，不留"以后再补"的脚手架；
   尊重用户明确的审美与品牌要求（brief 优先），不把个人口味凌驾其上。
2. **细化保留，重设计替换**：用户要"美化/优化"时保留现有身份、行为与
   文案；用户要"重设计"时才把旧视觉当作待替换的证据，不折中成两边不靠。
3. **层级靠字号/颜色/间距表达**，不靠卡片套卡片；装饰性元素必须有信息
   或功能理由，否则删掉。

## 反模式检测（HTML/CSS/TSX 产物必跑）

写完或修改界面代码后，对目标路径运行：

```bash
npx -y impeccable@4.1.0 detect <目标路径>
```

- 退出码 `0` = 干净；`2` = 有发现；`1` = 有目标无法扫描。
- 高频反模式与修法：
  - **side-tab**：卡片单侧 3px 实色彩条（AI 生成 UI 的最典型标志）。
    改为整圈 1px 细边框、染底、或图标色承担语义。
  - **layout-transition**：动画 `width/height/max-height/padding/margin`
    每帧触发布局重排。改 `transform`（scaleX 进度条）、`opacity`，折叠
    类收合改瞬时 + 淡出；`grid-template-rows` 可做高度动画。
  - **bounce-easing**：弹跳/弹性缓动显得廉价。改 ease-out 类缓动；
    scale+opacity 脉冲动画若被按名误中，重命名去掉 bounce 字样。
  - **broken-image**：`<img>` 空缺 src 会以碎图示人；动态解析的图标在
    拿到真实 src 前渲染兜底图形，而不是挂一个空 img。
- 确属误报（注释里的字样、测试桩、品牌硬要求）时，在文件头加单行豁免：
  `// impeccable-disable <规则名>: <原因>`（CSS/HTML 用对应注释语法）。

## 收尾标准

改完界面代码必须重跑一次 detect：修到清零，或对每条残留给出豁免理由，
再向用户交付。报告"已修好"之前，检测结果就是证据。

## 边界

- detect 只覆盖**代码型产物**（网页 / 应用 UI / HTML 幻灯片）；
  纯图片产物（导出的 PNG/PPTX）不适用，靠设计原则约束。
- PPT/幻灯片若以 HTML/CSS 实现，同样必跑 detect。
