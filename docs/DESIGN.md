# Hermix Desktop — Design System v5

> **No cards. No borders. No warmth in chrome.** Interface reads like editorial software — closer to **Linear / Things / Vercel** than to a chat app. Three shades of cool gray do all the layering. Color lives only on icons and status dots.

---

## 1. Visual Theme & Atmosphere

**Editorial Software. The screen is paper.**

- **Tone**: clinical, paper-soft, monochrome chrome
- **Keywords**: paper · monochrome · cool-gray · flat · tabular · quiet
- **Light**: no shadows on cards/buttons/inputs — modal only
- **Texture**: anti-aliased font hinting, `cv11`/`ss01` on, `tnum` on numbers

---

## 2. Color Palette & Roles

### Surfaces — the ONLY way to layer (cool gray, never warm)

```css
:root {
  /* Three cool gray stops, the entire layering system */
  --bg:      #FFFFFF;   /* page, modal interior, code block */
  --l1:      #F5F5F5;   /* sidebar bg, toolbar bg, composer bg, file row hover */
  --l2:      #EDEDED;   /* active row, active nav, hover-on-l1, focused composer */
  --l3:      #E5E5E5;   /* pressed, disabled, scrollbar thumb */

  /* No border variable. There is no border. */
}
```

> Sidebar uses `--l1` (`#F5F5F5`). Page uses `--bg` (`#FFFFFF`). The 6-point brightness gap is large enough to read clearly without ever looking red or warm. This is **cool neutral gray**, not beige.

### Ink (text & monochrome icons)

```css
--ink-900: #0A0A0A;   /* primary text, primary buttons, active accent */
--ink-700: #262626;   /* nav text, labels */
--ink-500: #525252;   /* secondary text, tooltips */
--ink-400: #737373;   /* timestamps, hint text */
--ink-300: #A3A3A3;   /* disabled, file paths, placeholder */
--ink-200: #D4D4D4;   /* rare — icon disabled */
```

### Icon Palette (the only place color exists)

10 hues. Solid fill only. No gradient. No outline. Single-color silhouettes.

```css
--icon-blue:    #2563EB;  /* chat, message bubble, markdown */
--icon-indigo:  #4F46E5;  /* extension, app */
--icon-violet:  #7C3AED;  /* agent, brain, think */
--icon-pink:    #DB2777;  /* automation, flow */
--icon-amber:   #D97706;  /* folder, workspace */
--icon-emerald: #059669;  /* search, find, success-heavy */
--icon-rose:    #E11D48;  /* timer, clock, recents */
--icon-cyan:    #0891B2;  /* product, cube, image */
--icon-slate:   #475569;  /* file (generic), text */
--icon-ink:     #0A0A0A;  /* terminal, code, settings */
```

**Icon usage rule**: same icon = same color, every time. Don't randomly recolor between sessions.

### Status (dots only — never pills)

```css
--ok:  #10B981;   /* success */
--warn:#F59E0B;
--err: #EF4444;
--run: #3B82F6;   /* running */
--idle:#A3A3A3;
```

### Code Block

```css
--code-bg:   #0A0A0A;
--code-fg:   #E5E5E5;
--code-link: #60A5FA;   /* paths inside code, highlighted token */
--code-mute: #737373;   /* line numbers, comments */
```

---

## 3. Typography

One family for UI, one family for code.

```css
--font: "Inter", -apple-system, BlinkMacSystemFont, "PingFang SC",
        "Microsoft YaHei", sans-serif;
--mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;

body { font-feature-settings: "cv11","ss01","tnum"; }
```

| Token | Size / Weight | LH | LS | Used for |
|---|---|---|---|---|
| `t-display` | 20 / 600 | 1.3 | -0.01em | empty-state title |
| `t-h1` | 16 / 600 | 1.4 | -0.005em | modal title |
| `t-body-strong` | 14 / 500 | 1.5 | 0 | active nav, primary button |
| `t-body` | 14 / 450 | 1.5 | 0 | default text |
| `t-small` | 13 / 450 | 1.5 | 0 | file name, breadcrumb |
| `t-meta` | 12 / 450 | 1.4 | 0 | timestamps, hint row |
| `t-caption` | 11 / 500 | 1.3 | 0.06em uppercase | section label |
| `t-button` | 13 / 500 | 1 | 0 | button label |
| `t-code` | 12.5 / 450 | 1.65 | 0 | code block, model name |

Weights 450 / 500 / 600 only. Numbers use `tabular-nums`.

---

## 4. Component Stylings

> **Master rule**: if you're about to write `border`, stop and use a fill instead. There is no border in this app except a 3px black active indicator and the modal shadow.

### Buttons

```css
.btn-primary {
  background: var(--ink-900); color: #fff;
  height: 36px; padding: 0 14px;
  border: none; border-radius: 8px;
  font: 500 13px/1 var(--font);
  display: inline-flex; gap: 6px; align-items: center;
  cursor: pointer;
}
.btn-primary:hover { background: #000; }
.btn-primary:disabled { background: var(--l3); color: var(--ink-300); cursor: not-allowed; }

.btn-send {
  background: var(--ink-900); color: #fff;
  height: 28px; padding: 0 12px;
  border: none; border-radius: 6px;
  font: 500 12.5px/1 var(--font);
  display: inline-flex; gap: 6px; align-items: center;
}
.btn-send:disabled { background: var(--l3); color: var(--ink-300); }

.btn-ghost {
  height: 26px; padding: 0 8px;
  background: transparent; color: var(--ink-500);
  border: none; border-radius: 5px;
  font: 450 12.5px/1 var(--font);
  display: inline-flex; gap: 6px; align-items: center;
}
.btn-ghost:hover { background: var(--l2); color: var(--ink-900); }
.btn-ghost.is-on { background: var(--l2); color: var(--ink-900); }

.btn-pill {
  height: 26px; padding: 0 10px;
  background: var(--l2); color: var(--ink-700);
  border: none; border-radius: 5px;
  font: 450 12px/1 var(--mono);
  display: inline-flex; gap: 6px; align-items: center;
}
.btn-pill:hover { background: var(--l3); }
```

### Sidebar Nav

```css
.nav {
  background: var(--l1);       /* cool gray, never warm */
  padding: 8px;
}
.nav-section { padding: 12px 0 4px; }
.nav-label {
  height: 22px; padding: 0 10px;
  font: 500 11px/1 var(--font);
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--ink-300);
  display: flex; align-items: center; justify-content: space-between;
}
.nav-item {
  display: flex; align-items: center; gap: 10px;
  height: 30px; padding: 0 10px;
  background: transparent;
  border: none; border-radius: 6px;
  font: 450 13px/1 var(--font);
  color: var(--ink-700);
  cursor: pointer;
}
.nav-item:hover  { background: var(--l2); color: var(--ink-900); }
.nav-item.active { background: var(--l2); color: var(--ink-900); font-weight: 500; }
.nav-item .icon { width: 16px; height: 16px; flex-shrink: 0; }
```

### Conversation Content — every type, with zero border

```css
/* LAYOUT — no card, no border. The thread IS the page. */
.thread        { padding: 24px 0 32px; }
.msg-row       { padding: 16px 24px; display: flex; gap: 12px; }
.msg-row + .msg-row { /* separator by whitespace alone */ }
.msg-avatar    { width: 24px; height: 24px; border-radius: 6px; flex-shrink: 0; display: flex; align-items: center; justify-content: center; }
.msg-user .msg-avatar { background: var(--l2); color: var(--ink-700); }
.msg-assistant .msg-avatar { background: var(--icon-violet); color: #fff; }
.msg-body      { flex: 1; min-width: 0; }
.msg-name      { font: 500 13px/1.4 var(--font); color: var(--ink-900); }
.msg-time      { font: 450 11px/1.4 var(--mono); color: var(--ink-400); margin-left: 8px; }
.msg-content   { padding-top: 4px; font: 450 14px/1.65 var(--font); color: var(--ink-900); }
.msg-user .msg-content .bubble {
  display: inline-block; max-width: 80%;
  background: var(--l2); color: var(--ink-900);
  padding: 10px 14px; border-radius: 12px;
}
```

### Conversation Components — the 7 content types

**Type 1 · Plain text**
```css
.text { font: 450 14px/1.65 var(--font); color: var(--ink-900); }
```

**Type 2 · User message bubble** — single `l2` fill, no border, no chevron, no shadow
```html
<div class="msg msg-user">
  <div class="msg-body" style="text-align:right">
    <span class="bubble">写一段代码，随便一个循环，写到py</span>
  </div>
</div>
```

**Type 3 · Tool call (running → done)** — gray fill, no border, status dot not pill
```css
.tool {
  background: var(--l1); border-radius: 10px;
  margin: 8px 0; overflow: hidden;     /* no border */
}
.tool-head {
  padding: 10px 14px;
  display: flex; align-items: center; gap: 10px;
  font: 500 13px/1 var(--font); color: var(--ink-900);
}
.tool-icon { width: 14px; height: 14px; color: var(--icon-ink); }
.tool-name { font: 450 12.5px/1 var(--mono); color: var(--ink-900); }
.tool-status { margin-left: auto; display: flex; gap: 8px; align-items: center;
               font: 450 12px/1 var(--font); color: var(--ink-500); }
.dot { width: 6px; height: 6px; border-radius: 50%; background: var(--ok); }
.dot.run  { background: var(--run); animation: pulse 1.4s ease-in-out infinite; }
.dot.err  { background: var(--err); }
.dot.idle { background: var(--idle); }
@keyframes pulse { 50% { opacity: .35; } }
.tool-body { padding: 12px 14px; background: var(--bg); }  /* white inside gray */
.tool-row  { display: flex; gap: 10px; padding: 4px 0; }
.tool-row + .tool-row { /* separator is gap, not line */ }
.tool-row .icon { width: 14px; height: 14px; color: var(--icon-{kind}); }
.tool-row .text { font: 450 13px/1.5 var(--font); color: var(--ink-700); }
.tool-row .path { font: 450 12px/1.5 var(--mono); color: var(--icon-blue); }
```

**Type 4 · Code block** — full-bleed black, internal highlight in `code-link` blue
```css
.code {
  background: var(--code-bg); color: var(--code-fg);
  padding: 12px 14px; border-radius: 10px;
  font: 450 12.5px/1.65 var(--mono);
  overflow-x: auto; margin: 8px 0;
}
.code .path  { color: var(--code-link); }
.code .com   { color: var(--code-mute); }
.code-head   { display:flex; align-items:center; gap:8px; padding:8px 14px;
               background: #141414; color: var(--code-mute);
               font: 450 11px/1 var(--mono); border-radius: 10px 10px 0 0; }
.code-body   { border-radius: 0 0 10px 10px; margin-top: -1px; }
```

**Type 5 · Markdown rendered** — h1/h2/h3, lists, blockquote — all borderless
```css
.md h1 { font: 600 18px/1.4 var(--font); color: var(--ink-900); margin: 14px 0 8px; }
.md h2 { font: 600 15px/1.4 var(--font); color: var(--ink-900); margin: 14px 0 6px; }
.md p  { margin: 6px 0; font: 450 14px/1.65 var(--font); color: var(--ink-900); }
.md ul { padding-left: 22px; margin: 6px 0; }
.md li { font: 450 14px/1.7 var(--font); color: var(--ink-900); list-style: disc; }
.md code { font: 450 12.5px/1.4 var(--mono); padding: 1px 5px;
           background: var(--l1); border-radius: 4px; color: var(--ink-900); }
.md blockquote { padding-left: 12px; color: var(--ink-500); font-style: italic; }
.md a { color: var(--icon-blue); text-decoration: none; }
.md table { border-collapse: collapse; margin: 10px 0; font: 450 13px/1.5 var(--font); }
.md th, .md td { padding: 6px 12px; text-align: left; }
.md th { background: var(--l1); color: var(--ink-900); font-weight: 500; }
.md tr:nth-child(even) td { background: var(--l1); }   /* zebra by fill, not line */
```

**Type 6 · Image / Screenshot preview** — inline thumbnail, no border, gray placeholder while loading
```css
.media { margin: 10px 0; }
.media img {
  max-width: 100%; max-height: 320px;
  border-radius: 10px;
  display: block;
}
.media-cap { padding-top: 6px; font: 450 11px/1.4 var(--mono); color: var(--ink-400); }
.media-skel {
  width: 100%; max-width: 320px; height: 180px;
  background: var(--l1); border-radius: 10px;
}
```

**Type 7 · Inline citation / file ref** — pill-shaped but filled, not bordered
```css
.ref {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 2px 8px; border-radius: 999px;
  background: var(--l1); color: var(--ink-700);
  font: 450 12px/1.4 var(--mono);
}
.ref .icon { width: 12px; height: 12px; }
.ref:hover { background: var(--l2); }
```

### Composer (no border — gray fill only)

```css
.composer {
  background: var(--l1);                  /* cool gray fill, NOT border */
  border-radius: 14px;
  padding: 8px 8px 8px 12px;
  transition: background .15s;
}
.composer:focus-within { background: var(--l2); }
.composer-input {
  width: 100%; min-height: 44px;
  padding: 6px 4px 0;
  border: none; outline: none;
  background: transparent;
  font: 450 14px/1.5 var(--font);
  color: var(--ink-900);
  resize: none;
}
.composer-input::placeholder { color: var(--ink-300); }
```

### Right Panel — File List

```css
.panel       { background: var(--bg); padding: 12px 8px; }
.panel-head  { display: flex; align-items: center; gap: 8px;
               padding: 4px 10px 8px; }
.panel-title { font: 500 13px/1 var(--font); color: var(--ink-900); }
.panel-tabs  { display: flex; gap: 2px; padding: 0 8px; margin-bottom: 4px; }
.tab         { padding: 6px 10px; font: 450 12.5px/1 var(--font);
               color: var(--ink-500); background: transparent; border: none;
               border-radius: 5px; cursor: pointer; }
.tab.is-on   { background: var(--l2); color: var(--ink-900); font-weight: 500; }

.file {
  display: flex; gap: 10px; align-items: center;
  padding: 7px 10px;
  border-radius: 6px; cursor: pointer;
  position: relative;
}
.file:hover  { background: var(--l1); }
.file.active { background: var(--l2); }
.file.active::before {
  content: ""; position: absolute; left: -8px; top: 50%; transform: translateY(-50%);
  width: 3px; height: 16px; border-radius: 2px;
  background: var(--ink-900);
}
.file .icon { width: 14px; height: 14px; flex-shrink: 0; }
.file .name { font: 450 13px/1.4 var(--font); color: var(--ink-900);
              overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.file .path { font: 450 11px/1.4 var(--mono); color: var(--ink-300);
              overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
```

### Modal — the only shadow in the app

```css
.modal-mask {
  position: fixed; inset: 0;
  background: rgba(10,10,10,.32);
  backdrop-filter: blur(2px);
  z-index: 30;
}
.modal-card {
  background: var(--bg); border-radius: 12px;
  box-shadow: 0 8px 24px rgba(0,0,0,.12), 0 2px 6px rgba(0,0,0,.06);
  /* no border — shadow defines the edge */
}
```

### Titlebar

```css
.titlebar { height: 44px; padding: 0 16px;
            display: flex; align-items: center; gap: 12px;
            background: var(--l1); }
.titlebar .crumb { font: 450 12.5px/1 var(--font); color: var(--ink-500); }
.titlebar .crumb.active { color: var(--ink-900); font-weight: 500; }
```

---

## 5. Layout Principles

- **Spacing base**: 4px. Stops: 4 · 6 · 8 · 10 · 12 · 14 · 16 · 20 · 24 · 28 · 32 · 40
- **Shell grid**: `260px sidebar · 1fr main · 340px detail`
- **Main column**: thread occupies full width; body content max-width 720px centered
- **Container padding-x**: 24px main / 8px aside / 12px right panel
- **Title bar height**: 44px
- **Section gaps**: 16px between messages, 8px inside tool card, 8px between file rows

**Whitespace philosophy**: the screen breathes. 32px gutter between the conversation thread and any chrome.

**Layering philosophy**: zero borders. Layering = `bg` → `l1` → `l2` → `l3`. Three cool gray stops. When two adjacent surfaces need contrast, swap fill or add gap.

---

## 6. Depth & Elevation

Almost flat. Modal only.

```css
--s-modal: 0 8px 24px rgba(0,0,0,.12),
           0 2px 6px rgba(0,0,0,.06);
```

| Layer | Token | Used by |
|---|---|---|
| 0 | `bg` `#FFFFFF` | page, tool body interior, modal, code block |
| 1 | `l1` `#F5F5F5` | sidebar bg, titlebar, composer bg, file hover |
| 2 | `l2` `#EDEDED` | active nav, active file, hover-on-l1, bubble, focused composer |
| 3 | `l3` `#E5E5E5` | pressed, disabled, scrollbar thumb |
| Overlay | `rgba(10,10,10,.32) + blur(2px)` | modal mask |

**Z-index**: 0 default · 10 sticky · 20 popover · 30 modal · 40 toast.

**No backdrop-filter** on normal surfaces. Only modal mask.

---

## 7. Do's and Don'ts

### Do

- Use cool gray fills (`l1` / `l2`) to create separation. Never reach for `border`.
- Sidebar = `l1`. Page = `bg`. Keep the 6-step brightness gap so it never reads warm.
- Use colored icons (10-hue palette). Pick once, stick to it.
- Status as 6px dots in semantic colors — never as colored pills.
- Active file: 3px black indicator bar — bold and unambiguous.
- `font-variant-numeric: tabular-nums` on every number.
- Inter 450 / 500 / 600 only.

### Don't

- **No `border`** anywhere except modal shadow + 3px active indicator.
- **No gradients** anywhere — including on icons.
- No colored primary buttons. Primary = `--ink-900`.
- No shadows on cards / buttons / inputs / list rows. Modal only.
- No warm gray, beige, or cream tones in chrome. Cool neutral only.
- No emoji in UI chrome.
- No border-radius > 14px on cards, > 12px on bubbles, > 8px on buttons.
- No outline-style icons. Solid silhouette only, flat single color.
- No two adjacent components can share the same gray fill. Either swap or add gap.

---

## 8. Responsive Behavior

- **Breakpoints**: 640 / 768 / 1024 / 1280 / 1536
- **Touch target**: minimum 32×32
- **Below 1024px**: right panel collapses into overlay drawer
- **Below 768px**: sidebar collapses into overlay drawer
- **Below 640px**: composer full-width; hint row simplifies

Font sizes are constant; only density changes.

---

## 9. Agent Prompt Guide

### Quick Reference

```
Sidebar (cool gray):   #F5F5F5  -- cool, never warm
Page bg:               #FFFFFF
Active / hover:        #EDEDED
Pressed / disabled:    #E5E5E5
Ink:                   #0A0A0A → #A3A3A3 (6 steps)
Icon palette:          10 hues, solid fill only
Border:                FORBIDDEN. Use fill + gap.
Shadow:                modal only
Font:                  Inter (450 / 500 / 600)
Spacing:               4px base · 8 / 12 / 16 / 24 / 32
Radius:                6 / 8 / 10 / 12 / 14
Status:                6px dot only
Active file:           3px #0A0A0A indicator bar
```

### Component Prompts

1. **Sidebar nav row** — 30px, 6px radius, no border. Inactive `#262626` on `bg`, hover `l2`, active `l2` + weight 500. **Colored 16px icon on the left** — solid fill from the 10-hue palette. No badge, no chevron, no dot. Just icon + label.

2. **Conversation message row** — 16px vertical padding, two-column flex (avatar 24px + body). No border, no card, no shadow. Avatar = 6px radius square: user gets `l2` + ink-700, assistant gets colored fill (violet). Time in 11px mono `#737373` after name.

3. **Tool call card** — outer `l1` fill, 10px radius, **no border**. Head: 14px colored icon + 12.5px mono tool name + right-aligned **6px status dot + 12px gray label**. Body: white `bg` fill inside the gray card. Result rows separated by 8px gap. No dividers.

4. **Code block** — full-bleed black `#0A0A0A`, 10px radius, internal mono 12.5px `#E5E5E5`. Header row `#141414` with file path. Inside code, **paths highlighted in `#60A5FA`** (blue link color).

5. **Composer** — 14px radius `l1` fill, no border. On focus: `l2`. Toolbar: ghost buttons (26px, 5px radius) on left, mono model pill (12px, `l2` fill, 5px radius) on right, then black 28px send button.

6. **File list row** — 7px padding, 6px radius, no border. Active row gets `l2` fill + 3px `#0A0A0A` indicator bar flush to left edge. Name 13px Inter, path 11px mono `#A3A3A3`. Icon on left: 14px, colored by file type.

7. **Empty state** — centered 20px Inter 600 title, 13px `#525252` description, 36px black primary button below. **32px colored icon above title**. No illustration, no mascot, no decorative bg.

### Iteration Guide

1. If two surfaces look mashed together, swap one of their fills — never add a border.
2. If chrome reads warm or beige, the gray is wrong. Use `#F5F5F5` / `#EDEDED` / `#E5E5E5` only.
3. If you want to draw the eye, change weight from 450 to 500 or 600. Don't change chrome color.
4. If status needs emphasis, bump dot from 6px to 8px. Never add a colored fill.
5. If numbers look misaligned, add `font-variant-numeric: tabular-nums`.
6. If a tool card feels heavy, lighten its head fill from `l1` to `bg`, let the inner body be the accent.
7. If you want to add color to a button — don't. Change to black, or to `l2` ghost.
8. Never introduce a new font family. Inter + ui-monospace only.
9. Never introduce a new hue for chrome. Color belongs to icons only.
10. **Sidebar must always be `l1`. If it's red, the gray is wrong.**

---

## Token Quick Reference

```css
:root {
  /* Surfaces — cool gray only */
  --bg: #FFFFFF;
  --l1: #F5F5F5;
  --l2: #EDEDED;
  --l3: #E5E5E5;

  /* Ink */
  --ink-900:#0A0A0A; --ink-700:#262626; --ink-500:#525252;
  --ink-400:#737373; --ink-300:#A3A3A3; --ink-200:#D4D4D4;

  /* Icon palette */
  --icon-blue:#2563EB; --icon-indigo:#4F46E5; --icon-violet:#7C3AED;
  --icon-pink:#DB2777;  --icon-amber:#D97706;   --icon-emerald:#059669;
  --icon-rose:#E11D48;  --icon-cyan:#0891B2;    --icon-slate:#475569;
  --icon-ink:#0A0A0A;

  /* Status */
  --ok:#10B981; --warn:#F59E0B; --err:#EF4444; --run:#3B82F6; --idle:#A3A3A3;

  /* Code */
  --code-bg:#0A0A0A; --code-fg:#E5E5E5;
  --code-link:#60A5FA; --code-mute:#737373;

  /* Type */
  --font:"Inter",-apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei",sans-serif;
  --mono:ui-monospace,"SF Mono",Menlo,Consolas,monospace;
}
```
