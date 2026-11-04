#!/usr/bin/env python3
"""Draw the 1024x1024 app icon (src-tauri/app-icon.png). Regenerate platform
icons afterwards with: npx tauri icon src-tauri/app-icon.png"""
import pathlib
from PIL import Image, ImageDraw, ImageFilter

S = 1024
out = pathlib.Path(__file__).resolve().parent.parent / "src-tauri/app-icon.png"

# gradient background
grad = Image.new("RGBA", (S, S))
px = grad.load()
a, b = (91, 79, 240), (229, 84, 27)
for y in range(S):
    for x in range(S):
        t = (x * 0.55 + y * 0.45) / S
        px[x, y] = tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3)) + (255,)

# macOS-style rounded square with margin
mask = Image.new("L", (S, S), 0)
m = 100
ImageDraw.Draw(mask).rounded_rectangle([m, m, S - m, S - m], radius=185, fill=255)
icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle([m, m + 14, S - m, S - m + 14], radius=185, fill=(0, 0, 0, 90))
shadow = shadow.filter(ImageFilter.GaussianBlur(18))
icon.alpha_composite(shadow)
icon.paste(grad, (0, 0), mask)

# router glyph: three inputs -> hub -> output
d = ImageDraw.Draw(icon)
W = (255, 255, 255, 255)
hub = (590, 512)
for y in (330, 512, 694):
    d.line([(300, y), hub], fill=W, width=34)
    d.ellipse([300 - 48, y - 48, 300 + 48, y + 48], fill=W)
d.line([hub, (770, 512)], fill=W, width=34)
d.polygon([(770, 440), (860, 512), (770, 584)], fill=W)
r = 82
d.polygon([(hub[0], hub[1] - r), (hub[0] + r, hub[1]), (hub[0], hub[1] + r), (hub[0] - r, hub[1])], fill=W)
icon.save(out)
print("wrote", out)
