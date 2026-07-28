# 主题设计规范（Theme System Spec v1）

> 适用对象：`apps/desktop-tauri`（React 19 + Vite + Tailwind 4）  
> 作用范围：整个 desktop app 的可配置主题、token 命名、组件用色约束、暗色可读性下限。  
> 起点：当前 `styles/tokens.css` 中的 light + dark 双主题。  
> 兼容性：light/dark 切换已通过 `data-theme="light|dark"` + `:root.dark` 双轨运行，本规范以 `data-theme` 为唯一真源，逐步下沉到 CSS 变量。

## 1. 设计目标

1. **可配置性**：新增一个主题 = 在 `apps/desktop-tauri/src/styles/themes/<name>.css` 新增一个文件并接入切换器，不需修改组件。
2. **暗色可读性**：所有 UI 文本在 `text-primary / text-secondary / text-muted / text-faint` 四级中必须落在 WCAG AA (4.5:1) 对比带内。低于 AA 的色值不得用于 body / label / button 文本。
3. **组件一致性**：不准在 `.tsx` 里写死颜色或硬编码 rgba。所有色值必须落在本规范定义的 token 上。
4. **可枚举**：每个组件的状态在每个主题下都必须给出明确可验证的颜色，而不是"在暗色下看着差不多"。

## 2. Token 命名与分组

Token 一律以 `--color-` 开头，禁止嵌套主题。

| 组 | 命名样式 | 含义 | 例子 |
|---|---|---|---|
| Surface | `bg-{app,primary,secondary,tertiary,hover,active,card,card-hover,elevated,code,code-header,code-border}` | 层级化背景 | `--color-bg-app` |
| Border | `border-{primary,secondary,strong,subtle}` | 边框层级 | `--color-border-subtle` |
| Text | `text-{primary,secondary,tertiary,muted,faint,on-code,warning}` | 4–5 级可读文字 | `--color-text-muted` |
| Accent | `accent, accent-hover, accent-soft, accent-soft-strong, accent-on-text` | 主题强调色（主操作） | `--color-accent` |
| Link | `link, link-hover, link-soft, link-soft-hover` | 文本中的链接 | `--color-link` |
| Semantic | `success(-soft), error(-soft), warning(-soft), text-warning, info(-soft)` | 语义状态 | `--color-error-soft` |
| Chip | `chip-{expert,workspace,permission,model}-{bg,fg,border}` | 弹层芯片（v3 中已蓝绿红米，但当前是硬编码） | `--color-chip-expert-bg` |
| Button | `btn-primary-{bg,text,hover}, btn-secondary-{bg,text,hover}, btn-ghost-{bg,text,hover}` | 按钮三档 | `--color-btn-primary-bg` |
| Overlay | `overlay-scrim, modal-outline, toast-shadow` | 模态遮罩／Toast 边框/投影 | `--color-overlay-scrim` |
| Shadow | `shadow-{card,pop,modal}` | 投影 | `--shadow-modal` |

### Text 4–5 级语义铁律

- `text-primary`   ≥ 7 : 1   body / heading / title
- `text-secondary` ≥ 5 : 1   strong body / nav items / labels
- `text-tertiary`  ≥ 4.5:1   description / chips / icons-only 行
- `text-muted`     ≥ 4.5:1   placeholder / hint / meta
- `text-faint`     ≥ 3 : 1   禁用图标 / 装饰横线 (NOT used for 正文)

任何主题都必须验证；当前 `.dark` 中 `text-faint=#76767e` 在 `bg-primary=#1c1c20` 上约 5.1:1，已满足。

## 3. 主题文件结构

```
apps/desktop-tauri/src/styles/
├─ tokens.css            # 默认 = light，本规范 true source
├─ tokens.dark.css       # :root.dark 覆盖 = dark
├─ themes/
│  ├─ high-contrast.css  # 高对比（可选第三方主题示例）
│  └─ solarized.css      # 第三方主题示例
└─ ...
```

每个主题文件以 `@layer themes { :root[data-theme="<name>"] { ... } }` 形式挂上，不破坏 `tokens.dark.css` 中 `:root.dark` 的回退（保持向后兼容）。

## 4. 切换器合约

`ThemeToggle` 已写在 `src/components/ThemeToggle.tsx`。对外行为不变：

- `<html data-theme="light|dark">`：主题选择器唯一来源。
- `:root.dark`：仅作为"未初始化 JS 之前"的 SSR 安全网（与 data-theme 互斥）。
- 切换后写入 `localStorage.hermix.desktop.theme`，刷新保持。
- 跟随系统：监听 `prefers-color-scheme`，仅在用户没选过主题时生效。

代码层禁止内联 `style={{ color: "..." }}`，必须使用 token + className。

## 5. 组件用色约束（禁止硬编码）

1. **绝对禁止** `.tsx` 里写 `'#xxxxxx'`、`'rgba(0,0,0,...)'`、`rgb(...)`，除非是 SVG `currentColor` 着色（如 `FileTypeIcon.tsx` 中 GitHub linguist 品牌色，例外清单见 §8）。
2. **优先** Tailwind class：`bg-bg-card`, `text-text-muted`, `border-border-subtle`, `hover:bg-bg-hover` 等。Tailwind 4 在 `@theme` 中已经把 `--color-*` 映射为 utilities。
3. **次优** CSS 变量：`style={{ background: "var(--color-bg-card)" }}`。仅在动态计算（如动画 ramp）时允许。
4. `**color-mix(...)`** 是允许的，用来在已有 token 上派生软色，例如 `color-mix(in srgb, var(--color-accent) 14%, transparent)`。这个用法 **不引入新色值**，仍视为 token 化。

### 必须迁移到 token 的当前硬编码

| 位置 | 当前硬编码 | 应该替换为 |
|---|---|---|
| `ChatView.tsx` expert/workspace/permission/model chip | `text-[#0F6E56]`、`bg-[#E1F5EE]`、`text-[#185FA5]`、`bg-[#E6F1FB]`、`text-[#993C1D]`、`bg-[#FAECE7]`、`bg-[#F1EFE8]` | `--color-chip-{expert,workspace,permission,model}-{fg,bg,border}` |
| `RoomMemberDialog.tsx:358` | `var(--color-error-bg, rgba(239,68,68,0.10))` | `--color-error-soft`（已存在） |
| `ImageLightbox.tsx` 内 `rgba(255,255,255,...)`、App modal 内 `rgba(0, 0, 0, 0.65)` scrim | 硬编码 scrim | `--color-overlay-scrim` |
| `toast--*` 1px solid `--color-error` 等 | 边框色和背景同色 | 使用 `--color-{success,error,warning}-solid`（border 用 solid，bg 用 soft） |
| `InlinePathPreview.tsx:249` | `background: "#fff"` | `--color-bg-primary` |
| `Avatar.tsx` 中 `var(--avatar-user-bg, #09090b)` | fallback 硬编码 | 移除 fallback（token 必须存在） |
| `ArchiveRefsGraph.tsx` `var(--color-accent, #475569)` 等 | 兜底硬编码 | 同上 |

迁移完成后，`rg -n "'#[0-9a-fA-F]{3,8}'" apps/desktop-tauri/src --glob "*.tsx"` 在排除清单之外应返回 0 条。

## 6. 状态芯片语义（v3 默认主题）

| 语义 | fg | bg | border | 用途 |
|---|---|---|---|---|
| `chip-expert` | `--color-accent` | `--color-accent-soft` | `--color-accent-soft-strong` | 弹层专家标签 |
| `chip-workspace` | `--color-success-fg` | `--color-success-soft` | `--color-success-soft-strong` | 弹层工作区标签 |
| `chip-permission` | `--color-warning-fg` | `--color-warning-soft` | `--color-warning-soft-strong` | 弹层权限标签 |
| `chip-model` | `--color-info-fg` | `--color-info-soft` | `--color-info-soft-strong` | 弹层模型标签 |

> token 命名：`-fg` / `-bg` / `-border` 用于语义色三件套；`-soft-strong` = 0.22 alpha，`-soft` = 0.12–0.16 alpha。  
> 对应 light 主题默认值：`--color-success-fg=#15803d`、`--color-warning-fg=#b45309`、`--color-info-fg=#0f6e56`（与现有 `ChatComposerInput.tsx:158` 中 `rgba(15,110,86,0.08)` 语义一致，但改为 token）。  
> dark 主题默认值需通过 §2 的对比度公式验证，确保 `text on soft-bg` ≥ 4.5:1。

## 7. 验证流程（CI / 本地）

1. **静态**：`scripts/check-theme-tokens.mjs`  
   - `rg` 全仓扫描 `*.tsx` 中硬编码 `#xxx/rgb/rgba`，排除 `FileTypeIcon.tsx` 等品牌色例外；任何命中即失败。  
   - 校验 `*.css` 中 `[data-theme="..."]` 仅出现在 `tokens.dark.css` 与主题文件中。  
2. **运行时**：在浏览器运行 `tools/theme-audit.html`（半成品后补），脚本扫 DOM、计算 token 实际渲染颜色与背景的对比度，输出 AA pass/warn 表。

## 8. 例外清单（E1–E2）

| ID | 文件 | 原因 |
|---|---|---|
| E1 | `FileTypeIcon.tsx` | GitHub linguist 品牌色，跨平台用户熟悉度优先，不随主题变化 |
| E2 | ArcNode icon SVG | 使用 `currentColor` 仍然继承主题 |

例外清单放在 `apps/desktop-tauri/src/styles/themes/EXCEPTIONS.md`，新增例外需要在此文件登记。

## 9. 迁移路线图

1. **Phase 1（已完成）**：light/dark 切换已可用，token 表基本完整。  
2. **Phase 2（本轮）**：  
   - 补 `--color-chip-*` 与 `--color-overlay-*` 等缺项；  
   - 替换 4 个 ChatView 弹层 chip 的硬编码色；  
   - 把 `--color-error-bg` 等占位符改为 token；  
   - 移除 `rgba(255,255,255,...)` / `rgba(0, 0, 0, ...)` 内联。  
3. **Phase 3**：  
   - 新增 `themes/high-contrast.css` 示例；  
   - 增加 theme-specific 覆盖。  
4. **Phase 4**：  
   - CI 加入 `scripts/check-theme-tokens.mjs`；  
   - 文档同步到 README。

## 10. 主题开发者快速流程

新增一个主题 = 两个文件：

```css
/* apps/desktop-tauri/src/styles/themes/happy.css */
@layer themes {
  :root[data-theme="happy"] {
    --color-bg-app: #fff7ec;
    --color-bg-primary: #fffaf0;
    /* 8 surfaces + 4 borders + 6 text + 4 accent + 4 link + 12 semantic */
    /* + 4 chip * 3 + btn + overlay + shadow = 全量必填 */
  }
}
```

```tsx
// ThemeToggle.tsx 添加一项
{ id: "happy", label: "农活火活" }
```

不要修改任何组件。所有 UI 都从 token 读取，新增主题不需 review 组件代码。
