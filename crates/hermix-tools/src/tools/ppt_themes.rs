//! PPT theme template system — inspired by oh-my-ppt's design approach.
//!
//! Each theme defines:
//! - **Color palette**: background, title color, body color, accent colors
//! - **Font scheme**: title font + body font (with fallbacks)
//! - **Layout grid**: 16:9 canvas (25.4cm × 14.29cm), with safe margins
//! - **Slide layouts**: predefined element positions for common slide types
//!
//! The LLM picks a theme by name, then generates content JSON. The
//! `create_presentation` tool maps the JSON + theme into office_cli
//! batch operations, producing a .pptx with consistent visual design.
//!
//! ## Design principles (from oh-my-ppt analysis)
//! 1. **Fixed 16:9 canvas** — all positions in EMU/cm, no fluid layout
//! 2. **Content height budget** — each text element has a max char count
//!    based on its box size, preventing overflow
//! 3. **Consistent accent system** — 5 accent colors that cycle through
//!    shapes/icons for visual rhythm
//! 4. **Typography hierarchy** — title 36pt, subtitle 20pt, body 18pt,
//!    caption 14pt — no more than 4 levels

use serde::{Deserialize, Serialize};

/// Canvas dimensions (16:9, standard PowerPoint widescreen).
pub const CANVAS_WIDTH_CM: f64 = 25.4;
pub const CANVAS_HEIGHT_CM: f64 = 14.29;
pub const MARGIN_CM: f64 = 1.5;
pub const CONTENT_WIDTH_CM: f64 = CANVAS_WIDTH_CM - 2.0 * MARGIN_CM; // 22.4
pub const CONTENT_HEIGHT_CM: f64 = CANVAS_HEIGHT_CM - 2.0 * MARGIN_CM; // 11.29

/// A complete PPT theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PptTheme {
    /// Machine name (used in tool params).
    pub name: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// One-line description for the LLM.
    pub description: &'static str,
    /// Background color (hex RGB, no alpha).
    pub background: &'static str,
    /// Title text color.
    pub title_color: &'static str,
    /// Body text color.
    pub body_color: &'static str,
    /// Subtle/muted text color (captions, footers).
    pub muted_color: &'static str,
    /// 5 accent colors for shapes, icons, highlights.
    pub accents: [&'static str; 5],
    /// Title font family.
    pub title_font: &'static str,
    /// Body font family.
    pub body_font: &'static str,
    /// Whether the theme is dark (affects default shape fills).
    pub is_dark: bool,
}

/// Predefined themes. Each is hand-tuned for visual quality.
pub static THEMES: &[PptTheme] = &[
    PptTheme {
        name: "midnight",
        display_name: "Midnight Blue",
        description: "深蓝底+白字，科技/商务风，适合产品发布、技术分享",
        background: "0F172A",
        title_color: "F8FAFC",
        body_color: "CBD5E1",
        muted_color: "64748B",
        accents: ["3B82F6", "8B5CF6", "06B6D4", "10B981", "F59E0B"],
        title_font: "Microsoft YaHei",
        body_font: "Microsoft YaHei",
        is_dark: true,
    },
    PptTheme {
        name: "aurora",
        display_name: "Aurora",
        description: "渐变紫底+白字，创意/设计风，适合品牌展示、创意提案",
        background: "1E1B4B",
        title_color: "F5F3FF",
        body_color: "C7D2FE",
        muted_color: "818CF8",
        accents: ["A78BFA", "F472B6", "34D399", "FBBF24", "60A5FA"],
        title_font: "Microsoft YaHei",
        body_font: "Microsoft YaHei",
        is_dark: true,
    },
    PptTheme {
        name: "clean",
        display_name: "Clean White",
        description: "白底+深灰字，极简/学术风，适合论文答辩、数据报告",
        background: "FFFFFF",
        title_color: "1E293B",
        body_color: "475569",
        muted_color: "94A3B8",
        accents: ["2563EB", "7C3AED", "059669", "DC2626", "D97706"],
        title_font: "Microsoft YaHei",
        body_font: "Microsoft YaHei",
        is_dark: false,
    },
    PptTheme {
        name: "warm",
        display_name: "Warm Sand",
        description: "暖米底+棕字，文艺/教育风，适合课件、读书分享",
        background: "FEF3C7",
        title_color: "78350F",
        body_color: "92400E",
        muted_color: "B45309",
        accents: ["D97706", "059669", "DC2626", "7C3AED", "2563EB"],
        title_font: "Microsoft YaHei",
        body_font: "Microsoft YaHei",
        is_dark: false,
    },
    PptTheme {
        name: "carbon",
        display_name: "Carbon",
        description: "纯黑底+亮字，高端/极客风，适合技术架构、安全报告",
        background: "000000",
        title_color: "00FF88",
        body_color: "A0A0A0",
        muted_color: "404040",
        accents: ["00FF88", "00AAFF", "FF6600", "FF0066", "AA00FF"],
        title_font: "Consolas",
        body_font: "Consolas",
        is_dark: true,
    },
    PptTheme {
        name: "ocean",
        display_name: "Ocean",
        description: "深青底+白字，清新/环保风，适合可持续发展、海洋主题",
        background: "042F2E",
        title_color: "CCFBF1",
        body_color: "94A3B8",
        muted_color: "2DD4BF",
        accents: ["2DD4BF", "0EA5E9", "F59E0B", "EF4444", "8B5CF6"],
        title_font: "Microsoft YaHei",
        body_font: "Microsoft YaHei",
        is_dark: true,
    },
];

/// Get a theme by name. Returns the first match or the default (midnight).
pub fn get_theme(name: &str) -> &'static PptTheme {
    THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .unwrap_or(&THEMES[0])
}

/// List all theme names with descriptions (for the LLM).
pub fn list_themes() -> Vec<(&'static str, &'static str, &'static str)> {
    THEMES
        .iter()
        .map(|t| (t.name, t.display_name, t.description))
        .collect()
}

/// Slide layout type — determines element positioning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SlideLayout {
    /// Title slide: large centered title + subtitle.
    Title,
    /// Section divider: accent bar + section name.
    Section,
    /// Content with title: title at top, body below.
    Content,
    /// Two-column: title + left content + right content.
    TwoColumn,
    /// Bullet points: title + 3-5 bullet items.
    Bullets,
    /// Image + text: title + image on one side + text on other.
    ImageText,
    /// Data/charts: title + chart area + caption.
    Data,
    /// Quote: large centered quote + attribution.
    Quote,
    /// Closing: centered "Thank You" + contact info.
    Closing,
}

/// A single slide's content in the intermediate JSON representation.
/// The LLM generates this; `create_presentation` maps it to office_cli ops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlideSpec {
    /// Layout type.
    pub layout: SlideLayout,
    /// Slide title (required for most layouts).
    #[serde(default)]
    pub title: String,
    /// Subtitle or section name.
    #[serde(default)]
    pub subtitle: String,
    /// Body text. For Bullets layout, each item is one bullet.
    #[serde(default)]
    pub body: Vec<String>,
    /// Image path (for ImageText layout). Optional.
    #[serde(default)]
    pub image: Option<String>,
    /// Quote text (for Quote layout).
    #[serde(default)]
    pub quote: String,
    /// Quote attribution.
    #[serde(default)]
    pub attribution: String,
    /// Accent color override (hex). If empty, cycles through theme accents.
    #[serde(default)]
    pub accent: String,
    /// Speaker notes.
    #[serde(default)]
    pub notes: String,
}

/// A complete presentation specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentationSpec {
    /// Theme name.
    pub theme: String,
    /// Output file path (.pptx).
    pub file: String,
    /// Presentation title (for the file metadata).
    #[serde(default)]
    pub title: String,
    /// Slides.
    pub slides: Vec<SlideSpec>,
}

/// Content height budget — max characters per text box size.
/// Derived from: font_size_pt × 0.5cm_per_pt × chars_per_line × lines.
/// This is the "content height budget algorithm" from oh-my-ppt.
pub mod budget {
    /// Max characters for a title (36pt, ~22cm wide).
    pub const TITLE_MAX_CHARS: usize = 40;
    /// Max characters for a subtitle (20pt, ~20cm wide).
    pub const SUBTITLE_MAX_CHARS: usize = 80;
    /// Max characters per bullet item (18pt, ~20cm wide).
    pub const BULLET_MAX_CHARS: usize = 60;
    /// Max bullet items per slide.
    pub const MAX_BULLETS: usize = 6;
    /// Max characters for body text (18pt, ~20cm wide, ~8cm tall).
    pub const BODY_MAX_CHARS: usize = 200;
    /// Max characters for a quote (28pt, centered, ~18cm wide).
    pub const QUOTE_MAX_CHARS: usize = 120;
}

/// Format a cm value as office_cli expects (e.g. "2.5cm").
pub fn cm(v: f64) -> String {
    format!("{:.1}cm", v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_theme_by_name() {
        let t = get_theme("midnight");
        assert_eq!(t.name, "midnight");
        assert!(t.is_dark);
    }

    #[test]
    fn test_get_theme_case_insensitive() {
        let t = get_theme("CLEAN");
        assert_eq!(t.name, "clean");
        assert!(!t.is_dark);
    }

    #[test]
    fn test_get_theme_fallback() {
        let t = get_theme("nonexistent");
        assert_eq!(t.name, "midnight"); // default
    }

    #[test]
    fn test_list_themes_not_empty() {
        let list = list_themes();
        assert!(list.len() >= 5);
    }

    #[test]
    fn test_canvas_dimensions() {
        // 16:9 aspect ratio
        let ratio = CANVAS_WIDTH_CM / CANVAS_HEIGHT_CM;
        assert!((ratio - 16.0 / 9.0).abs() < 0.01);
    }

    #[test]
    fn test_budget_limits() {
        assert!(budget::TITLE_MAX_CHARS > 0);
        assert!(budget::MAX_BULLETS <= 7); // cognitive limit
    }
}
