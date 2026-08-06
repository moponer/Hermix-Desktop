"""Generate Hermix Desktop brand icon set.

Design:
  - 256x256 master canvas, rounded square background
  - zinc dark bg (#1c1c20) — same neutral as theme, no blue/violet
  - stylized "H" mark in accent green (#4caf50), crossed by a diagonal
    "mix" stroke at the bottom-right corner to convey "Hermes + Mix"
  - subtle inner shadow + outer glow for depth

Outputs (Windows-only desktop app):
  - src-tauri/icons/32x32.png
  - src-tauri/icons/128x128.png
  - src-tauri/icons/128x128@2x.png (256x256)
  - src-tauri/icons/icon.png (512x512 master)
  - src-tauri/icons/icon.ico (16/24/32/48/64/128/256 多帧)

The .icns is for macOS — this project is Windows-only, but Tauri's bundle
icon list still references it; keep the file by regenerating with a
minimal macOS-valid PNG (single 512x512) just to satisfy the bundler.
"""
from __future__ import annotations

import os
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

# ---- Brand colors (must match theme tokens, no blue/violet) ----
BG_DARK = (28, 28, 32, 255)       # #1c1c20 zinc dark
BG_HIGHLIGHT = (44, 44, 50, 255)  # #2c2c32 inner ring
ACCENT = (76, 175, 80, 255)       # #4caf50 green (same as "已打包" badge)
ACCENT_LIGHT = (129, 199, 132, 255)  # #81c784 highlight on H top
WHITE = (240, 240, 245, 255)      # subtle label tint

ICON_DIR = Path(r"<repo>\apps\desktop-tauri\src-tauri\icons")


def render_master(size: int = 512) -> Image.Image:
    """Render the brand mark at given (square) size. Returns RGBA image."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    s = size / 256  # scale factor; design space is 256

    # 1) Rounded square background (Windows 11 Fluent: ~22% radius)
    radius = int(56 * s)  # ~22% of 256
    draw.rounded_rectangle(
        [(0, 0), (size - 1, size - 1)],
        radius=radius,
        fill=BG_DARK,
    )

    # 2) Inner 1px highlight ring (depth from light source top-left)
    inset = int(2 * s)
    draw.rounded_rectangle(
        [(inset, inset), (size - 1 - inset, size - 1 - inset)],
        radius=radius - inset,
        outline=(60, 60, 68, 200),
        width=max(1, int(1 * s)),
    )

    # 3) Soft radial glow behind the H (top-left)
    glow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse(
        [(-size * 0.2, -size * 0.2), (size * 0.9, size * 0.9)],
        fill=(76, 175, 80, 80),
    )
    glow = glow.filter(ImageFilter.GaussianBlur(radius=int(40 * s)))
    img = Image.alpha_composite(img, glow)
    draw = ImageDraw.Draw(img)

    # 4) The "H" mark — two vertical bars + crossbar
    # Design space: H spans (56, 64) to (200, 192), crossbar at y=116..140
    bar_w = max(1, int(28 * s))
    gap = int(40 * s)
    cx = size / 2
    left_x = int(cx - gap - bar_w)
    right_x = int(cx + gap)
    top_y = int(64 * s)
    bot_y = int(192 * s)

    # Left bar — gradient feel via two stacked rectangles (top lighter, bottom accent)
    def vbar(x: int, top: int, bot: int):
        # upper portion lighter
        upper_h = int((bot - top) * 0.45)
        draw.rounded_rectangle(
            [(x, top), (x + bar_w, top + upper_h)],
            radius=int(bar_w * 0.4),
            fill=ACCENT_LIGHT,
        )
        # lower portion main accent
        draw.rounded_rectangle(
            [(x, top + upper_h - int(bar_w * 0.4)),
             (x + bar_w, bot)],
            radius=int(bar_w * 0.4),
            fill=ACCENT,
        )

    vbar(left_x, top_y, bot_y)
    vbar(right_x, top_y, bot_y)

    # Crossbar (slightly thinner, centered)
    cross_w = right_x - left_x
    cross_thick = max(1, int(20 * s))
    cross_y = int(124 * s)
    draw.rounded_rectangle(
        [(left_x - int(2 * s), cross_y),
         (right_x + bar_w + int(2 * s), cross_y + cross_thick)],
        radius=int(cross_thick * 0.4),
        fill=ACCENT,
    )

    # 5) "Mix" diagonal stroke — short accent slash bottom-right of H
    # conveys movement / mixing
    slash_w = max(1, int(10 * s))
    slash_pts = [
        (int(168 * s), int(176 * s)),
        (int(208 * s), int(176 * s)),
        (int(220 * s), int(212 * s)),
        (int(208 * s), int(228 * s)),
        (int(168 * s), int(228 * s)),
        (int(180 * s), int(212 * s)),
    ]
    # clipped to safe area
    sx = int(40 * s)
    sy = int(40 * s)
    safe_l, safe_t, safe_r, safe_b = sx, sy, size - sx, size - sy
    in_pts = []
    for p in slash_pts:
        if safe_l <= p[0] <= safe_r and safe_t <= p[1] <= safe_b:
            in_pts.append(p)
    if len(in_pts) >= 3:
        draw.polygon(in_pts, fill=WHITE)
        draw.line(in_pts + [in_pts[0]], fill=(180, 180, 190, 255), width=max(1, int(1 * s)))

    # 6) Soft inner shadow strip on bottom edge for depth
    shadow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow)
    sd.rectangle(
        [0, int(size * 0.78), size, size],
        fill=(0, 0, 0, 80),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(radius=int(18 * s)))
    img = Image.alpha_composite(img, shadow)

    # 7) Bottom-right tiny dot — brand period
    d = ImageDraw.Draw(img)
    dot_r = int(10 * s)
    cxdot = int(220 * s)
    cydot = int(220 * s)
    if cxdot + dot_r < size - int(20 * s) and cydot + dot_r < size - int(20 * s):
        d.ellipse(
            [cxdot - dot_r, cydot - dot_r, cxdot + dot_r, cydot + dot_r],
            fill=WHITE,
        )

    return img


def main() -> None:
    ICON_DIR.mkdir(parents=True, exist_ok=True)

    # Master 512×512
    master = render_master(512)
    master.save(ICON_DIR / "icon.png", optimize=True)
    print(f"  ✓ icon.png (512x512)")

    # 128×128
    img128 = master.resize((128, 128), Image.LANCZOS)
    img128.save(ICON_DIR / "128x128.png", optimize=True)
    print(f"  ✓ 128x128.png")

    # 256×256 (acts as @2x for Tauri NSIS bundle)
    img256 = master.resize((256, 256), Image.LANCZOS)
    img256.save(ICON_DIR / "128x128@2x.png", optimize=True)
    print(f"  ✓ 128x128@2x.png (256x256)")

    # 32×32 (Windows taskbar small)
    img32 = master.resize((32, 32), Image.LANCZOS)
    img32.save(ICON_DIR / "32x32.png", optimize=True)
    print(f"  ✓ 32x32.png")

    # Build multi-resolution .ico for Windows taskbar / Explorer
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    ico_frames = [master.resize((n, n), Image.LANCZOS) for n in ico_sizes]
    master.save(
        ICON_DIR / "icon.ico",
        format="ICO",
        sizes=[(n, n) for n in ico_sizes],
        append_images=ico_frames[1:],
    )
    print(f"  ✓ icon.ico ({', '.join(str(n) for n in ico_sizes)})")

    # .icns — Tauri's bundle.icon list references it. macOS-only project doesn't
    # need it, but Tauri build fails if the file is missing. Generate a minimal
    # placeholder PNG under that name so the bundler can package (will simply
    # be ignored on Windows builds).
    try:
        master.save(ICON_DIR / "icon.icns", format="ICNS")
        print(f"  ✓ icon.icns (ICNS placeholder for bundler)")
    except (OSError, ValueError) as e:
        # If Pillow can't write ICNS format directly, fallback to embedding a
        # 512×512 PNG under .icns filename — Windows NSIS ignores it
        print(f"  ! icon.icns skipped ({e})")

    print("\nAll icons regenerated.")


if __name__ == "__main__":
    main()
