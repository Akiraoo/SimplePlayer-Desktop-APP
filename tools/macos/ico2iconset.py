"""Writes the PNG layers of a .ico into a macOS .iconset folder (for iconutil).

    python3 ico2iconset.py app.ico AppIcon.iconset

Only the sizes the .ico has are used; iconutil is fine with a partial set.
"""
import os
import struct
import sys

src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
count = struct.unpack("<HHH", data[:6])[2]
layers = {}
for i in range(count):
    w, h, _, _, _, _, size, off = struct.unpack("<BBBBHHII", data[6 + 16 * i: 22 + 16 * i])
    png = data[off: off + size]
    if png[:4] == b"\x89PNG":
        layers[w or 256] = png

os.makedirs(dst, exist_ok=True)
# iconset names: icon_<n>x<n>.png and icon_<n>x<n>@2x.png (= 2n pixels)
for n in (16, 32, 128, 256, 512):
    if n in layers:
        open(os.path.join(dst, f"icon_{n}x{n}.png"), "wb").write(layers[n])
    if 2 * n in layers:
        open(os.path.join(dst, f"icon_{n}x{n}@2x.png"), "wb").write(layers[2 * n])
print("iconset:", sorted(os.listdir(dst)))
