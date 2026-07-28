"""Generate the deterministic Cyberpunk Curl Downloader icon set.

The renderer intentionally uses only Python's standard library so release builds do not
need Pillow or another native image package.  The SVG in assets/ is the vector source
for the same visual language; this small rasterizer keeps the packaged PNG/ICO output
reproducible on a clean portable-build machine.
"""
from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "firefox-extension" / "icons"
ICO_PATH = ROOT / "assets" / "curl-downloader.ico"

BG = (3, 7, 18, 255)
PANEL = (17, 24, 39, 255)
EDGE = (38, 50, 71, 255)
TRACK = (24, 36, 58, 255)
CYAN = (0, 217, 255, 255)
CYAN_LIGHT = (103, 232, 249, 255)
MAGENTA = (217, 70, 239, 255)


def clamp(value: float, low: float, high: float) -> float:
    return max(low, min(high, value))


def blend(dst: list[int], src: tuple[int, int, int, int], amount: float) -> None:
    amount = clamp(amount, 0.0, 1.0)
    for index in range(3):
        dst[index] = round(dst[index] * (1.0 - amount) + src[index] * amount)
    dst[3] = 255


def pixel(canvas: list[list[list[int]]], x: float, y: float, color: tuple[int, int, int, int], alpha: float = 1.0) -> None:
    ix, iy = round(x), round(y)
    if 0 <= iy < len(canvas) and 0 <= ix < len(canvas[0]):
        blend(canvas[iy][ix], color, alpha)


def line(canvas: list[list[list[int]]], a: tuple[float, float], b: tuple[float, float], width: float, color: tuple[int, int, int, int], glow: float = 0.0) -> None:
    min_x = max(0, math.floor(min(a[0], b[0]) - width - glow))
    max_x = min(len(canvas[0]) - 1, math.ceil(max(a[0], b[0]) + width + glow))
    min_y = max(0, math.floor(min(a[1], b[1]) - width - glow))
    max_y = min(len(canvas) - 1, math.ceil(max(a[1], b[1]) + width + glow))
    dx, dy = b[0] - a[0], b[1] - a[1]
    length_sq = dx * dx + dy * dy or 1.0
    for y in range(min_y, max_y + 1):
        for x in range(min_x, max_x + 1):
            t = clamp(((x - a[0]) * dx + (y - a[1]) * dy) / length_sq, 0.0, 1.0)
            px, py = a[0] + t * dx, a[1] + t * dy
            distance = math.hypot(x - px, y - py)
            if distance <= width / 2.0 + glow:
                alpha = 1.0 if distance <= width / 2.0 else (1.0 - (distance - width / 2.0) / max(glow, 1.0)) * 0.3
                blend(canvas[y][x], color, alpha)


def circle(canvas: list[list[list[int]]], center: tuple[float, float], radius: float, color: tuple[int, int, int, int], width: float = 0.0, glow: float = 0.0) -> None:
    min_x = max(0, math.floor(center[0] - radius - width - glow))
    max_x = min(len(canvas[0]) - 1, math.ceil(center[0] + radius + width + glow))
    min_y = max(0, math.floor(center[1] - radius - width - glow))
    max_y = min(len(canvas) - 1, math.ceil(center[1] + radius + width + glow))
    for y in range(min_y, max_y + 1):
        for x in range(min_x, max_x + 1):
            distance = abs(math.hypot(x - center[0], y - center[1]) - radius)
            if distance <= width / 2.0 + glow:
                alpha = 1.0 if distance <= width / 2.0 else (1.0 - (distance - width / 2.0) / max(glow, 1.0)) * 0.3
                blend(canvas[y][x], color, alpha)


def arc(canvas: list[list[list[int]]], center: tuple[float, float], radius: float, start: float, sweep: float, width: float, color: tuple[int, int, int, int], glow: float = 0.0) -> None:
    steps = max(12, round(abs(sweep) * radius / 12.0))
    points = []
    for step in range(steps + 1):
        angle = start + sweep * step / steps
        points.append((center[0] + math.cos(angle) * radius, center[1] + math.sin(angle) * radius))
    for first, second in zip(points, points[1:]):
        line(canvas, first, second, width, color, glow)


def render(size: int, progress: int | None = None) -> bytes:
    scale = 4
    high = size * scale
    canvas = [[[BG[0], BG[1], BG[2], BG[3]] for _ in range(high)] for _ in range(high)]
    def s(value: float) -> float:
        return value * scale

    # Rounded-square approximation with a dark inset and graphite edge.
    for y in range(high):
        for x in range(high):
            nx, ny = x / scale, y / scale
            edge_distance = min(nx - 8, 248 - nx, ny - 8, 248 - ny)
            if edge_distance >= 0:
                blend(canvas[y][x], PANEL, 0.95)
                if edge_distance < 6:
                    blend(canvas[y][x], EDGE, 0.75)

    center = (s(128), s(128))
    radius = s(88)
    circle(canvas, center, radius, TRACK, s(14))
    if progress is None:
        arc(canvas, center, radius, -math.pi / 2, math.pi * 0.5, s(8), MAGENTA, s(4))
        arc(canvas, center, radius, 0, math.pi, s(8), CYAN, s(4))
    else:
        fraction = clamp(progress / 100.0, 0.0, 1.0)
        if fraction > 0:
            arc(canvas, center, radius, -math.pi / 2, math.tau * fraction, s(8), CYAN, s(4))
        if fraction < 1:
            arc(canvas, center, radius, -math.pi / 2 + math.tau * fraction, math.tau * (1.0 - fraction), s(5), MAGENTA, s(2))

    line(canvas, (s(36), s(68)), (s(58), s(68)), s(5), MAGENTA, s(3))
    line(canvas, (s(58), s(68)), (s(70), s(56)), s(5), MAGENTA, s(3))
    line(canvas, (s(70), s(56)), (s(100), s(56)), s(5), MAGENTA, s(3))
    circle(canvas, (s(36), s(68)), s(5), MAGENTA, s(4))
    line(canvas, (s(220), s(188)), (s(198), s(188)), s(5), MAGENTA, s(3))
    line(canvas, (s(198), s(188)), (s(186), s(200)), s(5), MAGENTA, s(3))
    line(canvas, (s(186), s(200)), (s(156), s(200)), s(5), MAGENTA, s(3))
    circle(canvas, (s(220), s(188)), s(5), MAGENTA, s(4))

    line(canvas, (s(128), s(68)), (s(128), s(154)), s(22), CYAN, s(6))
    line(canvas, (s(84), s(132)), (s(128), s(180)), s(22), CYAN, s(6))
    line(canvas, (s(128), s(180)), (s(172), s(132)), s(22), CYAN, s(6))
    line(canvas, (s(84), s(190)), (s(172), s(190)), s(9), CYAN_LIGHT, s(2))

    pixels = []
    for y in range(size):
        for x in range(size):
            total = [0, 0, 0, 0]
            for yy in range(y * scale, (y + 1) * scale):
                for xx in range(x * scale, (x + 1) * scale):
                    for channel in range(4):
                        total[channel] += canvas[yy][xx][channel]
            pixels.append(tuple(round(channel / (scale * scale)) for channel in total))
    return png(size, size, pixels)


def png(width: int, height: int, pixels: list[tuple[int, int, int, int]]) -> bytes:
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixels[y * width + x])
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")


def ico(images: list[tuple[int, bytes]]) -> bytes:
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = bytearray()
    offset = 6 + 16 * len(images)
    body = bytearray()
    for size, data in images:
        entries.extend(struct.pack("<BBBBHHII", 0 if size >= 256 else size, 0 if size >= 256 else size, 0, 0, 1, 32, len(data), offset))
        body.extend(data)
        offset += len(data)
    return header + bytes(entries) + bytes(body)


def main() -> None:
    ICON_DIR.mkdir(parents=True, exist_ok=True)
    for size in (16, 32, 48):
        (ICON_DIR / f"curl-downloader-{size}.png").write_bytes(render(size))
    for percent in range(0, 101, 10):
        (ICON_DIR / f"progress-{percent:03d}.png").write_bytes(render(32, percent))
    ICO_PATH.write_bytes(ico([(size, render(size)) for size in (16, 32, 48, 64, 128)]))
    print(f"generated {ICON_DIR} and {ICO_PATH}")


if __name__ == "__main__":
    main()