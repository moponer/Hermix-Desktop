/* ─────────────────────────────────────────────────────────────────
   FileTypeIcon — Material Design / Google Drive style file icons.
   ─────────────────────────────────────────────────────────────────

   住在共享包 @hermix/preview（2026-10-19 从 desktop 的
   src/components/ 搬过来）：文件类型图标是"文件长什么样"的公共知识，
   desktop 与 viewer 都要用——复制一份就是两套图标体系必然分叉
   （与 2026-10-13 抽包同一条理由：预览层只留一份实现）。

   走独立子路径出口 `@hermix/preview/FileTypeIcon` 而非 barrel：
   viewer 的首屏 bundle 靠 tree-shaking 才没把 exceljs / pdf.js /
   satteri 拖进去，图标这种两端都要用的轻组件单独出口最稳。

   Each icon is a small SVG that fills the entire viewBox:
     • Square body  (rounded corners)
     • Top-right dog-ear corner (a triangle in lighter shade)
     • Single character / short label centered in white

   This matches the visual style of Google Drive / Material Symbols
   "description" / "article" icons.  Every file type is identified
   by a single uppercase character (M for Markdown, = for doc, P for
   PDF, X for xls, etc.) plus an extension badge for binary docs.

   Why inline SVGs (not an icon font / external sprite)?
     • Theme control: colors come from `currentColor` or a per-type
       hex, so we get the same look in light + dark without a
       runtime cost.
     • Tiny: this file is < 12 KB for ~100 file types.
     • Crisp at any size (14px list rows, 30px modal header,
       60px preview tab).

   Layout (24×24 viewBox — fully filled, no top/bottom padding):
     • Body  : rounded rect 0.5..23.5 × 0.5..23.5  (fills box)
     • Ear   : triangle 16..23.5 × 0.5..7.5  (folded corner)
     • Label : centered text, 9-11px font, 700 weight, white

   ─────────────────────────────────────────────────────────────── */

import { useId, useMemo } from "react";
import type { CSSProperties } from "react";

/** Per-extension visual: brand color + 1-3 char label.
 *
 *  `bg` is the icon's solid fill color.  We pick colors that
 *  match the brand conventions of each tool so the icons read
 *  instantly:
 *    • PDF  : Adobe red    (#dc2626)
 *    • Word : Microsoft blue (#2b579a)
 *    • Excel: Microsoft green (#1d6f42)
 *    • PPT  : Microsoft orange (#d24726)
 *    • MD   : Markdown's muted blue
 *    • Code : GitHub Linguist palette
 *    • Image: emerald (universal "media" green)
 *
 *  `label` is what shows centered.  We prefer short codes (PDF,
 *  DOC, XLS) for binary docs and a single character for code
 *  (M for .m, = for .doc, etc.).  All labels are upper-case and
 *  ≤ 4 chars so they fit at 14px without scaling. */
const LOOK: Record<string, { bg: string; label: string }> = {
  // ── Documents (Microsoft Word) ────────────────────────────
  doc:  { bg: "#2b579a", label: "W"    },
  docx: { bg: "#2b579a", label: "W"    },
  odt:  { bg: "#2b579a", label: "O"    },
  rtf:  { bg: "#2b579a", label: "R"    },
  // ── PDF (Adobe red) ────────────────────────────────────────
  pdf:  { bg: "#dc2626", label: "PDF"  },
  // ── Spreadsheets (Microsoft Excel green) ──────────────────
  xls:  { bg: "#1d6f42", label: "X"    },
  xlsx: { bg: "#1d6f42", label: "X"    },
  ods:  { bg: "#1d6f42", label: "X"    },
  csv:  { bg: "#1d6f42", label: "CSV"  },
  tsv:  { bg: "#1d6f42", label: "TSV"  },
  // ── Presentations (Microsoft PowerPoint orange) ───────────
  ppt:  { bg: "#d24726", label: "P"    },
  pptx: { bg: "#d24726", label: "P"    },
  odp:  { bg: "#d24726", label: "O"    },
  key:  { bg: "#d24726", label: "K"    },
  // ── Markdown / text ───────────────────────────────────────
  md:   { bg: "#083fa1", label: "M"    },
  mdx:  { bg: "#083fa1", label: "M"    },
  markdown: { bg: "#083fa1", label: "M" },
  txt:  { bg: "#6b6b6b", label: "TXT"  },
  rst:  { bg: "#6b6b6b", label: "R"    },
  // ── Code — branded colors ────────────────────────────────
  ts:   { bg: "#3178c6", label: "TS"   },
  tsx:  { bg: "#3178c6", label: "TX"   },
  js:   { bg: "#f1e05a", label: "JS"   },
  jsx:  { bg: "#f1e05a", label: "JX"   },
  mjs:  { bg: "#f1e05a", label: "JS"   },
  cjs:  { bg: "#f1e05a", label: "JS"   },
  rs:   { bg: "#dea584", label: "RS"   },
  py:   { bg: "#3572a5", label: "PY"   },
  go:   { bg: "#00add8", label: "GO"   },
  java: { bg: "#b07219", label: "J"    },
  kt:   { bg: "#a97bff", label: "K"    },
  c:    { bg: "#555555", label: "C"    },
  h:    { bg: "#555555", label: "H"    },
  cc:   { bg: "#f34b7d", label: "CC"   },
  cpp:  { bg: "#f34b7d", label: "C+"   },
  cxx:  { bg: "#f34b7d", label: "C+"   },
  hpp:  { bg: "#f34b7d", label: "C+"   },
  hxx:  { bg: "#f34b7d", label: "C+"   },
  m:    { bg: "#438eff", label: "M"    },
  mm:   { bg: "#438eff", label: "M"    },
  swift: { bg: "#ffac45", label: "S"   },
  rb:   { bg: "#701516", label: "R"    },
  php:  { bg: "#4f5d95", label: "P"    },
  sh:   { bg: "#89e051", label: "SH"   },
  bash: { bg: "#89e051", label: "SH"   },
  zsh:  { bg: "#89e051", label: "Z"    },
  ps1:  { bg: "#012456", label: "PS"   },
  css:  { bg: "#563d7c", label: "CSS"  },
  scss: { bg: "#c6538c", label: "S"    },
  sass: { bg: "#c6538c", label: "S"    },
  less: { bg: "#1d365d", label: "L"    },
  html: { bg: "#e34c26", label: "<>"   },
  vue:  { bg: "#41b883", label: "V"    },
  svelte: { bg: "#ff3e00", label: "S"  },
  lua:  { bg: "#000080", label: "L"    },
  sql:  { bg: "#e38c00", label: "SQL"  },
  ino:  { bg: "#00979d", label: "I"    },
  // ── Data ──────────────────────────────────────────────────
  json: { bg: "#292929", label: "{}"   },
  yaml: { bg: "#cb171e", label: "Y"    },
  yml:  { bg: "#cb171e", label: "Y"    },
  toml: { bg: "#9c4221", label: "T"    },
  xml:  { bg: "#0060ac", label: "X"    },
  ini:  { bg: "#6b7280", label: "I"    },
  conf: { bg: "#6b7280", label: "C"    },
  env:  { bg: "#ecd53f", label: "E"    },
  // ── Images ────────────────────────────────────────────────
  png:  { bg: "#059669", label: "PNG"  },
  jpg:  { bg: "#059669", label: "JPG"  },
  jpeg: { bg: "#059669", label: "JPG"  },
  gif:  { bg: "#059669", label: "G"    },
  webp: { bg: "#059669", label: "W"    },
  bmp:  { bg: "#059669", label: "B"    },
  ico:  { bg: "#059669", label: "I"    },
  tiff: { bg: "#059669", label: "T"    },
  tif:  { bg: "#059669", label: "T"    },
  svg:  { bg: "#ffb13b", label: "SVG"  },
  // ── Audio / video ─────────────────────────────────────────
  mp3:  { bg: "#0d9488", label: "M"    },
  wav:  { bg: "#0d9488", label: "W"    },
  m4a:  { bg: "#0d9488", label: "M"    },
  flac: { bg: "#0d9488", label: "F"    },
  mp4:  { bg: "#0d9488", label: "V"    },
  mov:  { bg: "#0d9488", label: "V"    },
  mkv:  { bg: "#0d9488", label: "V"    },
  webm: { bg: "#0d9488", label: "V"    },
  // ── Archives ──────────────────────────────────────────────
  zip:  { bg: "#6b7280", label: "ZIP"  },
  rar:  { bg: "#6b7280", label: "R"    },
  "7z": { bg: "#6b7280", label: "7"    },
  tar:  { bg: "#6b7280", label: "T"    },
  gz:   { bg: "#6b7280", label: "G"    },
  // ── Executable / binary ───────────────────────────────────
  exe:  { bg: "#374151", label: "EXE"  },
  dll:  { bg: "#374151", label: "D"    },
  so:   { bg: "#374151", label: "S"    },
  dylib: { bg: "#374151", label: "D"  },
  // ── Config / lockfiles ────────────────────────────────────
  lock: { bg: "#6b7280", label: "L"    },
  // ── 更多常见类型（2026-10-19 补齐 viewer 文件列表用） ────
  // 现代图片格式
  avif: { bg: "#059669", label: "AVIF" },
  heic: { bg: "#059669", label: "HEIC" },
  heif: { bg: "#059669", label: "HEIF" },
  jfif: { bg: "#059669", label: "JFIF" },
  apng: { bg: "#059669", label: "APNG" },
  psd:  { bg: "#31a8ff", label: "PSD" },
  ai:   { bg: "#ff9a00", label: "AI"  },
  sketch: { bg: "#f2b21c", label: "SK" },
  fig:  { bg: "#a259ff", label: "FIG" },
  // 电子书
  epub: { bg: "#2e7d32", label: "EPUB" },
  mobi: { bg: "#2e7d32", label: "MOBI" },
  azw3: { bg: "#2e7d32", label: "AZW3" },
  // 安装包 / 镜像
  iso:  { bg: "#6b7280", label: "ISO" },
  msi:  { bg: "#374151", label: "MSI" },
  apk:  { bg: "#3ddc84", label: "APK" },
  deb:  { bg: "#d70a53", label: "DEB" },
  rpm:  { bg: "#a51c30", label: "RPM" },
  // 数据库
  sqlite: { bg: "#0f6fbf", label: "SQL" },
  sqlite3: { bg: "#0f6fbf", label: "SQL" },
  db:  { bg: "#0f6fbf", label: "DB"  },
  // 日志 / 临时 / 补丁
  log: { bg: "#6b7280", label: "LOG" },
  bak: { bg: "#9ca3af", label: "BAK" },
  tmp: { bg: "#9ca3af", label: "TMP" },
  patch: { bg: "#6b7280", label: "PATCH" },
  diff: { bg: "#6b7280", label: "DIFF" },
  properties: { bg: "#6b7280", label: "CFG" },
  gitignore: { bg: "#f05033", label: "GIT" },
  // 笔记本 / 构建
  ipynb: { bg: "#da5b0b", label: "IPYNB" },
  dockerfile: { bg: "#0f7bc4", label: "DOCK" },
  makefile: { bg: "#6b7280", label: "MAKE" },
  cmake: { bg: "#0f6fbf", label: "CMAKE" },
  gradle: { bg: "#02303a", label: "GRAD" },
  // ── 字体（2026-10-20 补：此前一个字体图标都没有，全是灰问号） ──
  ttf:  { bg: "#5c6bc0", label: "TTF" },
  otf:  { bg: "#5c6bc0", label: "OTF" },
  woff: { bg: "#5c6bc0", label: "WOFF" },
  woff2: { bg: "#5c6bc0", label: "WOF2" },
  eot:  { bg: "#5c6bc0", label: "EOT" },
  // ── 音视频补齐（Chromium 解不了的容器也给图标，落地时走元信息） ──
  avi:  { bg: "#0d9488", label: "V"    },
  wmv:  { bg: "#0d9488", label: "V"    },
  flv:  { bg: "#0d9488", label: "V"    },
  m4v:  { bg: "#0d9488", label: "V"    },
  mpg:  { bg: "#0d9488", label: "V"    },
  mpeg: { bg: "#0d9488", label: "V"    },
  "3gp": { bg: "#0d9488", label: "3GP" },
  ogg:  { bg: "#0d9488", label: "OGG" },
  opus: { bg: "#0d9488", label: "OPUS" },
  aac:  { bg: "#0d9488", label: "AAC" },
  wma:  { bg: "#0d9488", label: "WMA" },
  mid:  { bg: "#0d9488", label: "MID" },
  aiff: { bg: "#0d9488", label: "AIFF" },
  // ── Office 变体（此前只有 docx/pptx/xlsx） ─────────────────
  docm: { bg: "#2b579a", label: "W"    },
  dotx: { bg: "#2b579a", label: "W"    },
  dotm: { bg: "#2b579a", label: "W"    },
  xlsm: { bg: "#1d6f42", label: "X"    },
  pptm: { bg: "#d24726", label: "P"    },
  ppsx: { bg: "#d24726", label: "P"    },
  ppsm: { bg: "#d24726", label: "P"    },
  potx: { bg: "#d24726", label: "P"    },
  potm: { bg: "#d24726", label: "P"    },
  // ── 更多语言与配置（与共享预览层 detectLanguage 对齐） ──────
  pyw:  { bg: "#3572a5", label: "PY"   },
  kts:  { bg: "#a97bff", label: "K"    },
  sc:   { bg: "#e45649", label: "SC"   },
  ex:   { bg: "#6e4a7e", label: "EX"   },
  exs:  { bg: "#6e4a7e", label: "EX"   },
  hs:   { bg: "#5e5086", label: "HS"   },
  nim:  { bg: "#ffc200", label: "NIM"  },
  zig:  { bg: "#ec915c", label: "ZIG"  },
  jl:   { bg: "#a270ba", label: "JL"   },
  r:    { bg: "#198CE7", label: "R"    },
  sol:  { bg: "#3f3f3f", label: "SOL"  },
  proto: { bg: "#4285f4", label: "PR"  },
  psm1: { bg: "#012456", label: "PS"   },
  cmd:  { bg: "#89e051", label: "CMD"  },
  fish: { bg: "#89e051", label: "FISH" },
  hh:   { bg: "#f34b7d", label: "H+"   },
  razor: { bg: "#512be4", label: "RAZ" },
  pug:  { bg: "#a86454", label: "PUG"  },
  jade: { bg: "#a86454", label: "JADE" },
  hbs:  { bg: "#f7931e", label: "HBS"  },
  json5: { bg: "#292929", label: "J5"  },
  jsonp: { bg: "#292929", label: "JP"  },
  graphql: { bg: "#e10098", label: "GQL" },
  gql:  { bg: "#e10098", label: "GQL"  },
  adoc: { bg: "#73a0c5", label: "ADOC" },
  asc:  { bg: "#73a0c5", label: "ASC"  },
  wiki: { bg: "#6b7280", label: "WIKI" },
  tex:  { bg: "#3d6117", label: "TEX"  },
  make: { bg: "#6b7280", label: "MAKE" },
  nginx: { bg: "#009639", label: "NGX" },
  vim:  { bg: "#019733", label: "VIM"  },
  viml: { bg: "#019733", label: "VIM"  },
  vimrc: { bg: "#019733", label: "VIM" },
  http: { bg: "#0d9488", label: "HTTP" },
  regex: { bg: "#6b7280", label: "RE"  },
  mermaid: { bg: "#8b5cf6", label: "MER" },
  git:  { bg: "#f05033", label: "GIT"  },
  cfg:  { bg: "#6b7280", label: "CFG"  },
  props: { bg: "#6b7280", label: "PROPS" },
  map:  { bg: "#292929", label: "MAP"  },
  srt:  { bg: "#0d9488", label: "SRT"  },
  vtt:  { bg: "#0d9488", label: "VTT"  },
  m3u:  { bg: "#0d9488", label: "M3U"  },
  m3u8: { bg: "#0d9488", label: "M3U8" },
  npmrc: { bg: "#cb3837", label: "NPM" },
  editorconfig: { bg: "#6b7280", label: "ECFG" },
};

function lookFor(name: string): { bg: string; label: string } | null {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (!ext || ext === name.toLowerCase()) return null;
  return LOOK[ext] ?? null;
}

/** Renders a Material / Drive-style file-type icon.
 *
 *  Visual breakdown (24×24 viewBox, no top/bottom padding):
 *  ┌──────────────────┐
 *  │              ▟── │  ← dog-ear fold (lighter shade)
 *  │  ██████████████  │
 *  │  ██████████████  │  ← body filled with brand color
 *  │  ████  PDF  ████ │     white centered label
 *  │  ██████████████  │
 *  │  ██████████████  │
 *  └──────────────────┘
 *
 *  No "label band at the bottom" — the label is centered in
 *  the body.  The dog-ear is just a 2-tone effect on the
 *  top-right corner; the body still fills the full box.
 *
 *  `size` defaults to 18 (used in list rows).  Modal header
 *  uses 30.  Both are crisp because everything is vector.
 *
   *  Note: display:block — an inline SVG inherits the parent's text
   *  baseline and leaves a gap above the icon. */
export function FileTypeIcon({
  name,
  size = 18,
  className,
  style,
  variant = "solid",
}: {
  name?: string;
  size?: number;
  className?: string;
  style?: CSSProperties;
  /** "solid" = 品牌实底 + 白字（列表行 / 文件管理器）；"soft" = 淡染底 + 文字色字（正文）。 */
  variant?: "solid" | "soft";
}) {
  const look = useMemo(() => (name ? lookFor(name) : null), [name]);
  const label = look?.label ?? "?";
  // 未知类型的兜底灰——文件类型图标是官方品牌色系（见 FILE_LOOKS），
  // 不随主题。design-guard: allow
  const bg = look?.bg ?? "#6b7280";
  // soft = 正文形态：底色只留同色相的淡染，字形走 currentColor（跟正文）。
  // 实色方块 + 白色粗体字母在段落里就是一块块彩色积木（用户实机反馈：
  // "一部分重点加粗带背景色的特殊词汇……界面有点乱"）。
  const soft = variant === "soft";

  // Per-instance gradient id.  曾经按 (variant,label,bg) 生成——同一目录里
  // 十个 PDF 就是十个相同 id 的 <linearGradient>，url(#…) 全解析到文档里
  // 第一个实例；预览态下大图标网格被 display:none，那个「第一个实例」
  // 不再渲染，渐变引用整体失效 → 列表图标只剩白色字母（浅色主题下 =
  // 「图标看不见」）。id 必须每实例唯一，useId 保证。
  // useId 返回值含 ':'（HTML id 合法，拼 URL 片段前保险去掉）。
  const uid = useId().replace(/:/g, "");
  const gid = `fti-${uid}-${variant}-${label}`.replace(/[^a-zA-Z0-9-]/g, "");

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      style={style}
      display="block"
      aria-hidden="true"
    >
      <defs>
        {/* Subtle vertical gradient on the body — slightly lighter
            at the top so the icon has some depth.  Same gradient
            is used on the body and the dog-ear (the dog-ear is
            just a clip-path, painted with a lighter stop). */}
        <linearGradient id={`${gid}-body`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor={bg} stopOpacity={soft ? "0.22" : "1"} />
          <stop offset="1" stopColor={bg} stopOpacity={soft ? "0.14" : "0.86"} />
        </linearGradient>
        <linearGradient id={`${gid}-ear`} x1="0" y1="0" x2="0" y2="1">
          {/* 折角高光：叠在任意 body 色上的恒定白色渐变，不随主题。 */}
          {/* design-guard: allow */}
          <stop offset="0" stopColor="#ffffff" stopOpacity="0.35" />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0.18" />
        </linearGradient>
      </defs>

      {/* Body — rounded square filling the entire viewBox so the icon
          reads as a single solid color block. */}
      <path
        d="M3 1.5 A1.5 1.5 0 0 1 4.5 0 H15 L23 8 V21 A1.5 1.5 0 0 1 21.5 22.5 H4.5 A1.5 1.5 0 0 1 3 21 Z"
        fill={`url(#${gid}-body)`}
      />

      {/* Dog-ear fold (top-right corner).  Painted as a
          translucent white triangle — gives the icon the
          familiar "folded paper" hint without a hard outline.
          soft 形态下白色折角压在淡底上会糊掉，改用同色相（body 渐变）。 */}
      <path
        d="M15 0 V8 H23 Z"
        fill={soft ? `url(#${gid}-body)` : `url(#${gid}-ear)`}
        opacity={soft ? 0.9 : undefined}
      />
      {/* Ear shadow line — the fold's bottom edge, so the corner
          reads as a real fold, not just a triangle sticker. */}
      <path
        d="M15 0 V8 H23"
        stroke={soft ? "currentColor" : "#000000"}
        strokeOpacity={soft ? "0.24" : "0.12"}
        strokeWidth="0.6"
        strokeLinejoin="round"
        fill="none"
      />

      {/* Centered label */}
      <text
        x="11.5"
        y={label.length > 2 ? 16.2 : 16.6}
        textAnchor="middle"
        fontFamily="var(--font-sans)"
        fontSize={label.length > 2 ? 7.5 : 10.5}
        fontWeight="700"
        letterSpacing="0"
        fill={soft ? "currentColor" : "#ffffff"}
        // Hairline dark stroke around the glyph so it stays
        // readable on light-tinted brand colors (yellow JS,
        // orange MD) without affecting dark colors.
        // soft 形态的字形就是文字色（与纸面的对比度由正文保证），不需要描边。
        style={
          soft
            ? undefined
            : {
                paintOrder: "stroke",
                stroke: "rgba(0,0,0,0.22)",
                strokeWidth: 0.6,
              }
        }
      >
        {label}
      </text>
    </svg>
  );
}

/**
 * 文件夹图标：与文件图标同一套视觉语言（实心 + 折角 + 顶边高光），
 * 用暖琥珀色把「目录」和「文件」一眼分开。展开态用斜前板区别。
 */
export function FolderTypeIcon({
  size = 18,
  open = false,
  className,
  style,
}: {
  size?: number;
  open?: boolean;
  className?: string;
  style?: CSSProperties;
}) {
  // 品牌识别色系：目录色不随主题切换。
  // design-guard: allow
  const back = "#d97706";
  // design-guard: allow
  const front = "#f59e0b";
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      style={style}
      display="block"
      aria-hidden="true"
    >
      {/* 背板 + 标签（目录的"提手"） */}
      <path
        d="M3 7a2 2 0 0 1 2-2h3.3a2 2 0 0 1 1.5.7L11.2 7H19a2 2 0 0 1 2 2v2H3z"
        fill={back}
      />
      {open ? (
        /* 打开态：前板斜着翻下来，形状一眼区别于闭合态 */
        <path
          d="M6.6 11.5h15.6l-3 8.4a1.7 1.7 0 0 1-1.6 1.1H4.4a1.7 1.7 0 0 1-1.6-2.1z"
          fill={front}
        />
      ) : (
        <path
          d="M3 11.5h18v7.6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"
          fill={front}
        />
      )}
    </svg>
  );
}
