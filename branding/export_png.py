import os
import cairosvg

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "png")
os.makedirs(OUT, exist_ok=True)


def render(name, src, size):
    out = os.path.join(OUT, f"{name}-{size}.png")
    cairosvg.svg2png(
        url=os.path.join(HERE, src),
        write_to=out,
        output_width=size,
        output_height=size,
        background_color=None,
    )
    print("wrote", out)


# Logo icon at common app-icon sizes + master
for s in (1024, 512, 256, 128, 64, 32):
    render("hermix-logo", "hermix-logo.svg", s)

# Mascot preview
render("hermix-mascot", "hermix-mascot.svg", 512)

# Lockup (keep aspect ratio ~ 720x200)
out = os.path.join(OUT, "hermix-lockup.png")
cairosvg.svg2png(
    url=os.path.join(HERE, "hermix-lockup.svg"),
    write_to=out,
    output_width=1440,
    output_height=400,
)
print("wrote", out)
