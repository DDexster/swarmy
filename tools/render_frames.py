#!/usr/bin/env python3
"""Render frames recorded by `swarmy demo --record` to PNG files and an animated GIF.

Usage: tools/render_frames.py FRAMES.jsonl OUT_DIR [--gif NAME.gif] [--png INDEX:NAME ...]
       [--every N] [--scale S] [--start N]
       tools/render_frames.py --ansi TEXT OUT_DIR --png 0:NAME   (text with ANSI colours, one frame)
"""
import argparse
import json
import os
import re

from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

BG = (10, 13, 18)
FG = (205, 214, 224)
FONTS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeMono.ttf",
]
# up, down, left, right
BOX = {"│": (1, 1, 0, 0), "─": (0, 0, 1, 1), "╭": (0, 1, 0, 1), "╮": (0, 1, 1, 0), "╰": (1, 0, 0, 1), "╯": (1, 0, 1, 0),
       "├": (1, 1, 0, 1), "└": (1, 0, 0, 1), "┤": (1, 1, 1, 0), "┬": (0, 1, 1, 1), "┴": (1, 0, 1, 1), "┼": (1, 1, 1, 1)}
BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"


def hex2rgb(h, default):
    if not h:
        return default
    return tuple(int(h[i : i + 2], 16) for i in (1, 3, 5))


class Renderer:
    def __init__(self, size):
        paths = [p for p in FONTS if os.path.exists(p)]
        self.fonts = [ImageFont.truetype(p, size) for p in paths]
        self.cmaps = {id(f): set(TTFont(p, lazy=True).getBestCmap().keys()) for f, p in zip(self.fonts, paths)}
        self.bold = ImageFont.truetype(BOLD, size) if os.path.exists(BOLD) else self.fonts[0]
        if os.path.exists(BOLD):
            self.cmaps[id(self.bold)] = set(TTFont(BOLD, lazy=True).getBestCmap().keys())
        self.cw = round(self.fonts[0].getlength("M"))
        asc, desc = self.fonts[0].getmetrics()
        self.ch = asc + desc + 2
        self.cache = {}

    def font_for(self, c, bold):
        key = (c, bold)
        if key not in self.cache:
            chosen = self.bold if bold else self.fonts[0]
            # Fall back when the primary font has no glyph (braille, some symbols).
            if c.strip() and not self.has_glyph(chosen, c):
                chosen = next((f for f in self.fonts if self.has_glyph(f, c)), self.fonts[0])
            self.cache[key] = chosen
        return self.cache[key]

    @staticmethod
    def has_glyph(font, c):
        try:
            return font.getmask(c).getbbox() is not None and font.getmask(c).size != font.getmask("￿").size or c in " "
        except Exception:
            return False

    def render(self, rows):
        w = max(sum(len(r[0]) for r in row) for row in rows)
        img = Image.new("RGB", (w * self.cw + 24, len(rows) * self.ch + 24), BG)
        d = ImageDraw.Draw(img)
        for y, row in enumerate(rows):
            x = 0
            for text, fg, bg, bold in row:
                fgc, bgc = hex2rgb(fg, FG), hex2rgb(bg, BG)
                px, py = 12 + x * self.cw, 12 + y * self.ch
                if bgc != BG:
                    d.rectangle([px, py, px + len(text) * self.cw - 1, py + self.ch - 1], fill=bgc)
                for i, c in enumerate(text):
                    if c == " ":
                        continue
                    cx = px + i * self.cw
                    if 0x2800 <= ord(c) <= 0x28FF:
                        self.braille(d, ord(c) - 0x2800, cx, py, fgc)
                        continue
                    if c in BOX:
                        self.box(d, c, cx, py, fgc)
                        continue
                    if c in "█▀▄▌▐▏▎▍▋▊▉▁▂▃▄▅▆▇":
                        self.block(d, c, cx, py, fgc)
                        continue
                    f = self.font_for(c, bold)
                    d.text((cx, py + 1), c, font=f, fill=fgc)
                x += len(text)
        return img

    def braille(self, d, bits, x, y, col):
        """Braille dots as filled squares on a 2×4 grid, like a terminal with a good font."""
        w, h = self.cw, self.ch
        dots = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)]
        cw, chh = w / 2, h / 4
        for i, (cx, cy) in enumerate(dots):
            if bits & (1 << i):
                x0, y0 = x + cx * cw + 1, y + cy * chh + 1
                d.rectangle([x0, y0, x0 + cw - 2, y0 + chh - 2], fill=col)

    def box(self, d, c, x, y, col):
        """Box-drawing glyphs as lines spanning the whole cell, so borders do not break."""
        w, h = self.cw, self.ch
        mx, my = x + w // 2, y + h // 2
        up, down, left, right = BOX[c]
        if c in "╭╮╰╯":
            r = min(w, h) // 2
            box = {"╭": (mx, my, mx + 2 * r, my + 2 * r, 180, 270), "╮": (mx - 2 * r, my, mx, my + 2 * r, 270, 360),
                   "╰": (mx, my - 2 * r, mx + 2 * r, my, 90, 180), "╯": (mx - 2 * r, my - 2 * r, mx, my, 0, 90)}[c]
            d.arc(box[:4], box[4], box[5], fill=col)
            if right:
                d.line([mx + r, my, x + w, my], fill=col)
            if left:
                d.line([x, my, mx - r, my], fill=col)
            if down:
                d.line([mx, my + r, mx, y + h], fill=col)
            if up:
                d.line([mx, y, mx, my - r], fill=col)
            return
        if up:
            d.line([mx, y, mx, my], fill=col)
        if down:
            d.line([mx, my, mx, y + h], fill=col)
        if left:
            d.line([x, my, mx, my], fill=col)
        if right:
            d.line([mx, my, x + w, my], fill=col)

    def block(self, d, c, x, y, col):
        """Block elements drawn as rectangles so bars join seamlessly."""
        w, h = self.cw, self.ch
        if c == "█":
            d.rectangle([x, y, x + w - 1, y + h - 1], fill=col)
        elif c == "▀":
            d.rectangle([x, y, x + w - 1, y + h // 2 - 1], fill=col)
        elif c == "▄":
            d.rectangle([x, y + h // 2, x + w - 1, y + h - 1], fill=col)
        elif c in "▏▎▍▌▋▊▉":
            k = "▏▎▍▌▋▊▉".index(c) + 1
            d.rectangle([x, y, x + max(1, w * k // 8) - 1, y + h - 1], fill=col)
        elif c == "▐":
            d.rectangle([x + w // 2, y, x + w - 1, y + h - 1], fill=col)
        else:
            k = "▁▂▃▄▅▆▇".index(c) + 1
            top = y + h - max(1, h * k // 8)
            d.rectangle([x, top, x + w - 1, y + h - 1], fill=col)


# The basic SGR colours as the dashboard's default theme draws them.
SGR = {"31": (255, 95, 95), "32": (95, 255, 135), "33": (255, 191, 0), "34": (95, 135, 255),
       "35": (215, 135, 255), "36": (0, 215, 255), "37": FG}
DIM = (110, 120, 135)


def ansi_frame(text):
    """One frame (rows of runs, as `swarmy demo --record` writes them) from text with SGR codes."""
    rows = []
    for line in text.rstrip("\n").split("\n"):
        runs, fg, bold, dim = [], None, False, False
        for part in re.split(r"(\x1b\[[0-9;]*m)", line):
            m = re.fullmatch(r"\x1b\[([0-9;]*)m", part)
            if m:
                for code in (m.group(1) or "0").split(";"):
                    if code in ("", "0"):
                        fg, bold, dim = None, False, False
                    elif code == "1":
                        bold = True
                    elif code == "2":
                        dim = True
                    elif code in SGR:
                        fg = SGR[code]
                continue
            if part:
                col = DIM if dim and fg is None else fg
                runs.append([part, "#%02x%02x%02x" % col if col else None, None, bold])
        rows.append(runs)
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("frames")
    ap.add_argument("out")
    ap.add_argument("--gif")
    ap.add_argument("--png", nargs="*", default=[])
    ap.add_argument("--every", type=int, default=1)
    ap.add_argument("--size", type=int, default=15)
    ap.add_argument("--scale", type=float, default=1.0)
    ap.add_argument("--start", type=int, default=0, help="first frame of the GIF")
    ap.add_argument("--ansi", action="store_true", help="FRAMES is text with ANSI colours: one frame")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    if a.ansi:
        frames = [ansi_frame(open(a.frames, encoding="utf-8").read())]
    else:
        frames = [json.loads(l) for l in open(a.frames)]
    r = Renderer(a.size)
    for spec in a.png:
        idx, name = spec.split(":", 1)
        img = r.render(frames[int(idx)])
        img.save(os.path.join(a.out, name), optimize=True)
        print("wrote", name, img.size)
    if a.gif:
        rgb = []
        for i in range(a.start, len(frames), a.every):
            img = r.render(frames[i])
            if a.scale != 1.0:
                img = img.resize((int(img.width * a.scale), int(img.height * a.scale)), Image.LANCZOS)
            rgb.append(img)
        # One palette for the whole animation, sampled from every screen, so rare colours
        # (a model's hue that appears on one screen only) survive.
        sample = rgb[:: max(1, len(rgb) // 12)]
        sheet = Image.new("RGB", (sample[0].width, sample[0].height * len(sample)))
        for k, im in enumerate(sample):
            sheet.paste(im, (0, k * im.height))
        pal = sheet.quantize(colors=255, method=Image.Quantize.MAXCOVERAGE)
        imgs = [im.quantize(palette=pal, dither=Image.Dither.NONE) for im in rgb]
        imgs[0].save(os.path.join(a.out, a.gif), save_all=True, append_images=imgs[1:], duration=100 * a.every, loop=0, optimize=True, disposal=1)
        print("wrote", a.gif, len(imgs), "frames", os.path.getsize(os.path.join(a.out, a.gif)) // 1024, "KB")


if __name__ == "__main__":
    main()
