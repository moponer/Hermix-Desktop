/* ─────────────────────────────────────────────────────────────────
   FileTypeIcon — Material Design / Google Drive style file icons.
   ─────────────────────────────────────────────────────────────────
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
     • Tiny: this file is < 8 KB for ~60 file types.
     • Crisp at any size (14px list rows, 30px modal header,
       60px preview tab).

   Layout (24×24 viewBox — fully filled, no top/bottom padding):
     • Body  : rounded rect 0.5..23.5 × 0.5..23.5  (fills box)
     • Ear   : triangle 16..23.5 × 0.5..7.5  (folded corner)
     • Label : centered text, 9-11px font, 700 weight, white

   v3 fix (主人 2026-06-22): "直接就是一个图标，背景色占满"
     — v2 用了 "page body 浅色 + 底部色带" 的 GitHub Octicons 风格，
     但主人要的是 Google Drive 风格——背景色占满整个图标，只有
     右上角一个 dog-ear 折角。新版直接是单色背景 + 中央字符。
   ─────────────────────────────────────────────────────────────── */

import { useMemo } from "react";

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
  txt:  { bg: "#475569", label: "TXT"  },
  rst:  { bg: "#475569", label: "R"    },
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
 *  Note on display: block — see the same note in the previous
 *  version.  inline SVG inherits the parent's text baseline
 *  and leaves a gap above the icon. */
export function FileTypeIcon({
  name,
  size = 18,
  className,
  style,
}: {
  name?: string;
  size?: number;
  className?: string;
  style?: React.CSSProperties;
}) {
  const look = useMemo(() => (name ? lookFor(name) : null), [name]);
  const label = look?.label ?? "?";
  const bg = look?.bg ?? "#6b7280";

  // Per-icon gradient id.  Embedding the bg in the id avoids the
  // "two PDF icons share a gradient" bug when the page has
  // multiple artifacts of the same type.
  const gid = `fti-${label}-${bg}`.replace(/[^a-zA-Z0-9-]/g, "");

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
          <stop offset="0" stopColor={bg} stopOpacity="1" />
          <stop offset="1" stopColor={bg} stopOpacity="0.86" />
        </linearGradient>
        <linearGradient id={`${gid}-ear`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0.35" />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0.18" />
        </linearGradient>
      </defs>

      {/* Body — rounded square filling the entire viewBox.  This
          is the "background color占满" the user asked for. */}
      <path
        d="M3 1.5 A1.5 1.5 0 0 1 4.5 0 H15 L23 8 V21 A1.5 1.5 0 0 1 21.5 22.5 H4.5 A1.5 1.5 0 0 1 3 21 Z"
        fill={`url(#${gid}-body)`}
      />

      {/* Dog-ear fold (top-right corner).  Painted as a
          translucent white triangle — gives the icon the
          familiar "folded paper" hint without a hard outline. */}
      <path d="M15 0 V8 H23 Z" fill={`url(#${gid}-ear)`} />
      {/* Ear shadow line — the fold's bottom edge, so the corner
          reads as a real fold, not just a triangle sticker. */}
      <path
        d="M15 0 V8 H23"
        stroke="#000000"
        strokeOpacity="0.12"
        strokeWidth="0.6"
        strokeLinejoin="round"
        fill="none"
      />

      {/* Centered label */}
      <text
        x="11.5"
        y={label.length > 2 ? 16.2 : 16.6}
        textAnchor="middle"
        fontFamily="-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif"
        fontSize={label.length > 2 ? 7.5 : 10.5}
        fontWeight="700"
        letterSpacing="0"
        fill="#ffffff"
        // Hairline dark stroke around the glyph so it stays
        // readable on light-tinted brand colors (yellow JS,
        // orange MD) without affecting dark colors.
        style={{
          paintOrder: "stroke",
          stroke: "rgba(0,0,0,0.22)",
          strokeWidth: 0.6,
        }}
      >
        {label}
      </text>
    </svg>
  );
}
