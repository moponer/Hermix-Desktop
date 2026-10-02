# Decision Note: 设置外壳重构 + 系统能力（通知 / 自启 / 托盘 / 壁纸）

Status: implemented — L2 + L3.1~L3.5 + L3.7/3.8 + L4 部分 + L3.6 落地

## Decision

按 `2026-09-30-doc-cleanup-and-optimization-plan.md` 的 L 系列执行，落地以下四组，
每组各自独立可回滚：

1. **L3.1/L3.2 基础件先行**：`ui/Switch.tsx` + `ui/SettingsRow.tsx`。新 `Switch`
   走 `--color-accent`、36×20 胶囊；根目录既有 `ToggleSwitch`（44px、写死 success
   绿、带 ✓/✕）**保留不动**，两者语义不同：老的用于"启用某能力"的强提示场景。
2. **L2 外壳**：`SettingsShell.tsx` 取代平铺 7 分类，改为 4 组 15 页 + 搜索 +
   「← 返回应用」。四个工具子 tab 从横条提升为一级导航。`SystemConfigPanel` 退化为
   薄转发，保住 `App.tsx` / `ChatView` / `Composer` 三处 import。
3. **L3.3~L3.5 外观/通用**：模式选择卡接 `useDomainMode`（后端 `domain_override`
   链路本就通）；`useTheme` 新增 `SYSTEM_THEME` 哨兵实现明暗三选；策展强调色调色板
   （accent + on-accent 成对注入）；数据目录行、发送快捷键。
4. **L4/L3.6 系统能力**：新增 `system_settings.rs`（8 个 tauri command），接入
   `tauri-plugin-notification` / `tauri-plugin-autostart` / tauri 内置 `tray-icon`。

## Alternatives considered

- **壁纸铺在 `.app-shell`（最底）——否决**。侧边栏、顶栏、对话面板、弹窗全是各自
  不透明实色，铺最底只会在接缝露出几像素：看得见但等于没挂。改为挂在
  `.chat-surface`（`[data-wallpaper="on"]` 门控），聊天滚动区是应用里最大的整面，
  也是本来就该允许个性化的位置。蒙版用 `color-mix` 混**当前主题底色**而非半透明
  黑——黑蒙版在浅色主题下会把壁纸压成灰脏色。
- **`useWallpaper` 每组件一份 state——否决**。App 根与设置页都要用，两份实例会
  各持旧 url：设置页改完后 App 根那份任何一次重渲染都可能把 CSS 变量写回旧值，
  表现是"设置页说换好了、主界面没变"，且时有时无（与 ModelSelector 踩过的双权威
  同源）。改为模块级共享 store，所有 DOM 写入收敛到唯一 `applyToDom()`。
- **通知/自启走插件的 JS API——否决**。那需要在 `capabilities/default.json` 开放
  `notification:*` / `autostart:*` 权限，把前端能直接触发的面扩大到插件全集。改为
  自有 `#[tauri::command]` 包一层，**capabilities 零新增**——前端只能调这 8 个命令，
  插件其余能力仍然够不着。
- **壁纸存原路径而非复制进数据目录——否决**。用户挪走/删掉原图后界面变成一片空白
  且无从解释。改为复制到 `assets/wallpaper.<ext>`，另存 `.source` 供设置页如实
  展示"这张图来自哪"。
- **自由取色器——否决**（沿用 L1 方针定稿）。`--color-on-accent` 无法用
  `color-mix` 推导，必须随 accent 明度切换，故强调色只能是策展清单。
- **L5 差异项按原计划补做——部分撤销**。核实后发现「始终授权」与「模型选择器
  provider 标注」**已存在**（`Composer.tsx:394-442` 的权限 chip、
  `ModelSelector.tsx:164-176` 的 `byProvider` 分组），此前按 ZCode 措辞检索导致
  误判为缺失。代码块「运行」按钮需新增 HTTP 端点并接入审批引擎，属新能力而非
  UI 缺口，不在本批。

## Consequences

- **对比度滑杆（L3.5 后半）明确不做**。`--color-text-*` 在 `tokens.css` 的
  `@theme` 内，改名会断掉 Tailwind 工具类生成；不改名的唯一路径是在根元素挂
  `filter: contrast()`，而 filter 会创建包含块、打断所有 fixed 定位的弹窗与
  toast。两条路都伤及他处，留待能一次改对时再做。
- **`--wallpaper-image` 在 `base.css` 声明为 `none`**，由 `useWallpaper` 在
  documentElement 行内样式覆盖。这样 `check-theme-tokens` 的 orphan 检查能过，
  且"没设壁纸"是自解释的规则而非"变量不存在"。切勿改成空串——与 L1 里 accent
  踩过的 `var(x, fallback)` 不回退是同一个坑。
- **壁纸文件名必须带扩展名**。webview 靠扩展名判 MIME，无扩展名文件被当未知类型
  直接拒绝渲染，界面表现是"设了没变"且无报错。故按白名单保留源扩展名，换扩展名
  时删旧文件（否则 `current_wallpaper_file` 会看到两张，"换完没换"且行为取决于
  字典序）。
- **`resolve_data_dir` 提为 `pub(crate)`**。壁纸落盘必须与设置页"数据目录"显示
  走同一条解析链；复制一份会在便携模式 / `HERMIX_HOME` 覆盖下走到不同分支。
- **主题 token 例外新增 E17 / E18**：`accentPalette.ts` 的策展色值是"可选值的
  数据"而非"主题取值"；三选卡缩略图是"另一套主题"的示意，token 无法跨作用域
  取值。两条都写了重新引入条件。
- **系统级三项每次进设置页重新回读**，不落 localStorage——用户去系统里改掉开机
  自启后，设置页必须显示真实状态。切换失败要把开关**弹回**原值，不能停在乐观值。
- **依赖来源**：`~/.cargo/config.toml` 已配 rsproxy.cn 镜像，新增 Tauri 插件
  可正常拉取。此前"离线拿不到插件"的判断有误（当时只查了本地 cache 目录）。

## Reintroduction conditions

- **对比度滑杆**：待有人能一次性完成 `@theme` → `-base` 槽重构（且同步
  `check-theme-tokens` 的 CONTRACT 派生），或找到不破坏 fixed 定位的对比度
  实现方案时，可重新提案。
- **代码块运行按钮**：待后端提供受审批管控的代码执行端点（而非绕过 approval
  engine 直接调用 `code_execution`）时，可重新提案。
- **壁纸多张管理**：待用户确有"多壁纸轮换/图库"需求时再引入列表与缩略图管理；
  当前只支持单张是刻意的范围控制。
