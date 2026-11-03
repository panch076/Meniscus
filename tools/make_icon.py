"""Draws the app icon (a droplet in a glass ring) with no third-party packages."""
import math
import struct
import sys
import zlib
from pathlib import Path

SIZE = 256


def pixel(x, y):
    u, v = (x + 0.5) / SIZE * 2 - 1, (y + 0.5) / SIZE * 2 - 1
    r = math.hypot(u, v)
    # Rounded glass disc with a soft edge.
    disc = max(0.0, min(1.0, (0.94 - r) * SIZE / 2))
    if disc == 0:
        return 0, 0, 0, 0
    t = (u + v + 2) / 4
    cr, cg, cb = 24 + 70 * t, 90 + 60 * t, 200 + 40 * t
    # Rim highlight.
    rim = math.exp(-(((r - 0.88) / 0.035) ** 2))
    # Droplet: a circle that tapers to a point at the top.
    dy = v - 0.16
    width = 0.42 * math.sqrt(max(0.0, 1 - ((dy + 0.1) / 0.72) ** 2)) * (0.55 + 0.45 * min(1.0, (dy + 0.82) / 0.8))
    inside = abs(u) < width and -0.62 < dy < 0.62
    if inside:
        shade = 0.72 + 0.28 * (1 - abs(u) / max(width, 1e-3))
        cr, cg, cb = 225 * shade, 245 * shade, 255 * shade
    cr, cg, cb = cr + rim * 90, cg + rim * 90, cb + rim * 60
    return int(min(cr, 255)), int(min(cg, 255)), int(min(cb, 255)), int(disc * 255)


def png_bytes():
    rows = bytearray()
    for y in range(SIZE):
        rows.append(0)
        for x in range(SIZE):
            rows.extend(pixel(x, y))

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    png = png_bytes()
    (out / "icon.png").write_bytes(png)
    # An ICO may hold a PNG directly; size bytes of 0 mean 256.
    ico = struct.pack("<HHH", 0, 1, 1) + struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22) + png
    (out / "icon.ico").write_bytes(ico)


main()
