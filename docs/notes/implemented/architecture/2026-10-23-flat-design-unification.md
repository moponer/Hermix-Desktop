---
class: architecture
date: 2026-10-23
status: implemented
---

# 扁平化统一：无描边 / 彩色分类图标 / 统一动效

## Decision

全站（主程序 + 文件查看器 + 共享预览层）统一到一套**扁平化**视觉语言，
七条规则：

1. **层次只由底色差 + 间距 + 阴影承担，不用描边。**
   中性结构描边从 27 处清到 15 处（`check-design-primitives` 的 G10 基线
   同步下调），剩下的 15 处是物理上无法替换的：复选框方框、spinner
   圆环、图片边框、状态横幅的语义描边、少数浮层内部分隔。
2. **焦点不用描边。** 全局 `:focus-visible` 从 `outline: 2px solid` 改为
   强调色淡晕 `--shadow-focus-flat`（color-mix 到透明，无硬边线）。
   鼠标点击不留任何焦点外观；键盘焦点仍可见（WCAG 2.4.7）。
3. **分类图标全部上色，8 档色相。** 新增 `--color-tone-{slate,blue,cyan,
   green,amber,orange,rose,violet}` 一套**身份色**，与语义四色
   （success/warning/error/info）物理隔离、不共用名字。
4. **图标底色由使用点现算**（`color-mix(in srgb, var(--cat-tone) 14%,
   var(--color-bg-primary))`），不额外发一套 soft 令牌 —— 一条式子亮暗
   两套主题都成立。
5. **彩色图标的实现放在共享包**（`packages/preview/src/styles/icon-tone.css`），
   主程序与查看器共用一份。
6. **动效统一到 `styles/motion.css`**：只动 transform / opacity，三档时长，
   ease-out 无回弹，reduced-motion 由 base.css 全局压掉。
7. **查看器与主程序同色同字**：中性轴改纯灰、强调色改松墨绿、字体栈补
   中文回退链；菜单栏文案压到 2–4 字。

### 分类色板的语义分工

| 档位 | 用于 |
|---|---|
| slate | 通用 / 系统 / 设备 / 归档 |
| blue | 网络 / 连接器 / 后端服务 |
| cyan | 数据 / 检索 / 运行时 / 诊断 |
| green | 文件 / 文档 / 工作日志 |
| amber | 自动化 / 定时任务 / 安全 |
| orange | 媒体 / 语音 / 截图 |
| rose | 记忆 / 人格 / 外观 / 待定夺 |
| violet | 智能 / 技能 / 模型 |

## Alternatives considered

- **给每个分类发一整套「柔和底 + 文字色」令牌。**
  否决：8 档 × 2 支 = 16 条令牌要进主题契约，每套主题（dark / ink-green）
  都要重写一遍，必然漂移（历史上 `--color-accent-soft` 就走过这条弯路）。
  现算 color-mix 只要 8 条本体。
- **分类色直接复用语义四色**（成功绿当文件色、警告黄当自动化色）。
  否决：语义四色表达「状态」，复用会让用户把「数据库」读成「成功」。
  这也是 2026-10-05 删掉旧 `toneForCategory` 时确立的纪律，本次保留。
- **焦点直接 `outline: none` 了事。**
  否决：WCAG 2.4.7 要求键盘焦点可见，整站会无法用键盘操作。淡晕是
  「可见」与「无描边」的唯一交点。
- **彩色图标各 app 各写一份。**
  否决：查看器是独立进程且不装 Tailwind，两份实现两周内必然分叉 ——
  这正是本仓库反复出现的事故模式。
- **入场动画用弹性缓动 / 更大幅度位移。**
  否决：回弹在工具类界面里显得廉价；位移超过 6px 会在流式输出里晃。
  160ms / 6px 是「能看出是新来的」的最小值。
- **启动屏保留位图吉祥物。**
  否决：位图要等一次磁盘往返，慢时先闪一个空格子；且 112px 照片 +
  径向渐变 + 6px 流光进度条是四套装饰叠在一起。改为纯矢量「墨点落纸」，
  动画只有环自绘与墨点落下两条，都是一次性的（不循环 —— 启动屏停留
  时长不可控，循环动画停在半途最尴尬）。

## Consequences

- **新增契约令牌**：`--color-tone-*` 8 条 + `--shadow-focus-flat` 1 条。
  dark.css 与 ink-green.css 已各自补齐（light 走 tokens.css 默认值）。
  新增主题时要照抄这两组。
- **G10 基线从 27 下调到 15**。新代码画中性描边会让门禁变红。
- **`h-enter` 是全局入场动画**（25 处复用），改它等于全站入场统一 ——
  不要为单个组件另开一套 keyframe。
- **共享层维系成本**：两侧都必须在 `:root` 定义 8 档 `--color-tone-*`，
  缺一侧则整条声明被丢弃（表现为「图标突然全灰但不报错」）。
- **仍保留的内联样式**只有两类：运行时几何（PDF 标注定位、表格列宽、
  进度百分比）与数据自带色（专家色板、渠道品牌色）。静态配色类内联
  样式已全部换成 `btn-flat` / `chip-flat` / `cat-icon` 类族。
- **查看器菜单文案缩短**后，`menubar-pop` 的 `min-width: 236px` 仍成立
  （最长项「系统浏览器」5 字 + 快捷键，未触底）。

## 验证

- `tsc --noEmit`（desktop-tauri / viewer）均通过
- `check-design-guard` / `check-design-primitives` / `check-theme-tokens`
  （含 contrast 全部达 AA）/ `check-preview-layer` 均 clean
- `cargo check --workspace --all-targets` 通过
- `./scripts/ci-gate.sh` 通过
