"""Render Aeri's promo video frame by frame with Pillow and encode it with ffmpeg.

Usage:  python scripts/promo/make_promo.py [--out promo.mp4] [--fps 30] [--preview SECONDS]
--preview writes single PNG frames at the given times instead of a video (for quick checks).
Requires Pillow and ffmpeg on PATH. Text and timing live in SCENES below.
"""
import argparse
import math
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parents[2]
W, H = 1920, 1080
FONT_DIR = "C:/Windows/Fonts/"
F_BOLD = FONT_DIR + "segoeuib.ttf"
F_SEMI = FONT_DIR + "seguisb.ttf"
F_REG = FONT_DIR + "segoeui.ttf"
F_CJK = FONT_DIR + "msyh.ttc"  # for the Japanese reply

BG = (14, 16, 24)
INK = (243, 244, 246)
SUB = (156, 163, 175)
ACCENT = (129, 140, 248)
TEAL = (94, 234, 212)
CARD = (24, 27, 39)
LINE = (40, 45, 62)
USER_BG = (49, 46, 129)
USER_INK = (224, 231, 255)
CHIP = (33, 37, 52)

_fonts = {}


def font(path, size):
    key = (path, size)
    if key not in _fonts:
        try:
            _fonts[key] = ImageFont.truetype(path, size)
        except OSError:
            _fonts[key] = ImageFont.truetype(F_REG, size)
    return _fonts[key]


def ease(t):
    t = max(0.0, min(1.0, t))
    return 1 - (1 - t) ** 3


def clamp01(t):
    return max(0.0, min(1.0, t))


def fade(t, start, dur=0.5):
    return ease((t - start) / dur)


def lerp(a, b, k):
    return a + (b - a) * k


def mix(c1, c2, k):
    return tuple(int(lerp(a, b, k)) for a, b in zip(c1, c2))


# ---------- background ----------

def background(t):
    img = Image.new("RGB", (W, H), BG)
    glow = Image.new("RGB", (W // 4, H // 4), BG)
    g = ImageDraw.Draw(glow)
    # two slow-moving soft glows
    x1 = 330 + 40 * math.sin(t * 0.35)
    y1 = 60 + 30 * math.cos(t * 0.3)
    g.ellipse((x1 - 170, y1 - 170, x1 + 170, y1 + 170), fill=(40, 42, 92))
    x2 = 80 + 30 * math.cos(t * 0.25)
    y2 = 230 + 20 * math.sin(t * 0.4)
    g.ellipse((x2 - 130, y2 - 130, x2 + 130, y2 + 130), fill=(18, 52, 58))
    glow = glow.filter(ImageFilter.GaussianBlur(60)).resize((W, H), Image.BILINEAR)
    return glow


def text_center(d, y, s, f, fill):
    w = d.textlength(s, font=f)
    d.text(((W - w) / 2, y), s, font=f, fill=fill)


def compose(img, layer):
    """Blend an RGBA layer onto img (RGB or RGBA)."""
    if img.mode == "RGBA":
        img.alpha_composite(layer)
    else:
        img.paste(layer, (0, 0), layer)


def alpha_text(img, xy, s, f, fill, a, anchor=None):
    """Draw text with opacity a (0..1)."""
    if a <= 0:
        return
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).text(xy, s, font=f, fill=fill + (int(255 * a),), anchor=anchor)
    compose(img, layer)


def bolt(img, x, y, color, a):
    """Small lightning icon (Segoe UI has no ⚡ glyph)."""
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    pts = [(x + 9, y), (x + 1, y + 13), (x + 7, y + 13), (x + 4, y + 24), (x + 13, y + 9), (x + 7, y + 9), (x + 10, y)]
    ImageDraw.Draw(layer).polygon(pts, fill=color + (int(255 * a),))
    compose(img, layer)


def logo(size):
    return Image.open(ROOT / "assets" / "app.png").convert("RGBA").resize((size, size), Image.LANCZOS)


# ---------- scenes ----------

def scene_intro(img, t):
    # 0-4 s: logo, name, tagline
    a = fade(t, 0.2, 0.8)
    out = clamp01((t - 3.4) / 0.6)
    a *= 1 - out
    size = int(lerp(150, 170, ease(t / 4)))
    lg = logo(size)
    lg.putalpha(lg.getchannel("A").point(lambda v: int(v * a)))
    img.paste(lg, ((W - size) // 2, int(300 - 20 * ease(t / 1.2))), lg)
    alpha_text(img, (W / 2, 520), "Aeri", font(F_BOLD, 120), INK, a, anchor="mm")
    alpha_text(img, (W / 2, 640), "Your PC, by voice.", font(F_SEMI, 52), SUB, fade(t, 1.0, 0.8) * (1 - out), anchor="mm")


def scene_hook(img, t):
    # 4-8 s
    t0 = t - 4
    out = 1 - clamp01((t0 - 3.4) / 0.6)
    alpha_text(img, (W / 2, 450), "Most assistants talk.", font(F_SEMI, 78), SUB, fade(t0, 0.1) * out, anchor="mm")
    alpha_text(img, (W / 2, 580), "Aeri gets things done.", font(F_BOLD, 96), INK, fade(t0, 1.0) * out, anchor="mm")
    # underline sweep
    k = ease((t0 - 1.6) / 0.8) * out
    if k > 0:
        d = ImageDraw.Draw(img)
        half = 400 * k
        d.rounded_rectangle((W / 2 - half, 650, W / 2 + half, 658), 4, fill=mix(BG, ACCENT, out))


CHAT = [
    ("Remind me to stretch in 20 minutes", "Set reminder: Time to stretch", "Done. I'll remind you to stretch at 4:34."),
    ("What's 18 percent of 2450?", "Calculate: 0.18 × 2450", "18 percent of 2450 is 441."),
    ("How much disk space do I have left?", "Check computer status", "About 49 GB free on C and 118 GB on D."),
    ("Translate good morning into Japanese and copy it", "Copy to clipboard", "\u201cGood morning\u201d is おはよう (ohayou). It's on your clipboard."),
]
CHAT_START = 8.0
CHAT_STEP = 3.1


def draw_bubble_right(img, d, x_right, y, s, a):
    f = font(F_REG, 30)
    w = d.textlength(s, font=f) + 48
    box = (x_right - w, y, x_right, y + 62)
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    ld.rounded_rectangle(box, 22, fill=USER_BG + (int(255 * a),))
    ld.text((box[0] + 24, y + 12), s, font=f, fill=USER_INK + (int(255 * a),))
    compose(img, layer)


def has_cjk(s):
    return any("\u3040" <= ch <= "\u9fff" for ch in s)


def scene_chat(img, t):
    t0 = t - CHAT_START
    out = 1 - clamp01((t - 20.6) / 0.5)
    if t0 < 0 or out <= 0:
        return
    d = ImageDraw.Draw(img)
    # caption on the left
    alpha_text(img, (150, 360), "Just say it.", font(F_BOLD, 76), INK, fade(t0, 0.0) * out)
    alpha_text(img, (150, 460), "Reminders, math, files, settings,", font(F_REG, 36), SUB, fade(t0, 0.3) * out)
    alpha_text(img, (150, 510), "apps, translation \u2014 done on your PC.", font(F_REG, 36), SUB, fade(t0, 0.3) * out)
    alpha_text(img, (150, 620), "\u201cHey Aeri\u201d", font(F_SEMI, 44), TEAL, fade(t0, 0.6) * out)

    # app window
    x0, y0, x1, y1 = 900, 110, 1780, 970
    win_a = fade(t0, 0.0, 0.6) * out
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    ld.rounded_rectangle((x0, y0, x1, y1), 28, fill=CARD + (int(250 * win_a),), outline=LINE + (int(255 * win_a),), width=2)
    img.paste(layer, (0, 0), layer)
    alpha_text(img, (x0 + 36, y0 + 26), "Aeri", font(F_BOLD, 34), INK, win_a)
    alpha_text(img, (x0 + 36, y0 + 70), "Listening for \u201cHey Aeri\u201d", font(F_REG, 22), SUB, win_a)
    d.line((x0 + 2, y0 + 112, x1 - 2, y0 + 112), fill=mix(BG, LINE, win_a), width=2)

    # messages scroll up as new ones arrive
    rows = []
    for i, (q, tool, ans) in enumerate(CHAT):
        ts = 0.6 + i * CHAT_STEP
        rows.append((ts, q, tool, ans))
    block = 196
    visible = [r for r in rows if t0 >= r[0]]
    shift = max(0, len(visible) - 3) * block
    shift_anim = 0
    if len(visible) > 3:
        last = visible[-1][0]
        shift_anim = block * (1 - ease((t0 - last) / 0.5))
    y = y0 + 140 - shift + shift_anim
    main_img = img
    img = Image.new("RGBA", main_img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    for ts, q, tool, ans in rows:
        k = t0 - ts
        if k < 0:
            break
        top = y
        if top + block > y0 + 120 and top < y1 - 40:
            fa = fade(k, 0.0, 0.3) * out
            # clip messages that scrolled above the header
            draw_bubble_right(img, d, x1 - 36, top, q, fa)
            if k > 0.7:
                ca = fade(k, 0.7, 0.3) * out * (fa / max(out, 1e-6) if out else 0)
                f = font(F_REG, 22)
                w = d.textlength(tool, font=f) + 54
                lyr = Image.new("RGBA", img.size, (0, 0, 0, 0))
                ImageDraw.Draw(lyr).rounded_rectangle((x0 + 70, top + 80, x0 + 70 + w, top + 116), 10, fill=CHIP + (int(255 * ca),))
                compose(img, lyr)
                bolt(img, x0 + 84, top + 86, SUB, ca)
                alpha_text(img, (x0 + 108, top + 84), tool, f, SUB, ca)
            if k > 1.2:
                n = int(len(ans) * clamp01((k - 1.2) / 0.9))
                aa = out * (fa / max(out, 1e-6) if out else 0)
                lyr = Image.new("RGBA", img.size, (0, 0, 0, 0))
                ImageDraw.Draw(lyr).ellipse((x0 + 40, top + 140, x0 + 54, top + 154), fill=ACCENT + (int(255 * aa),))
                compose(img, lyr)
                f = font(F_CJK if has_cjk(ans) else F_REG, 28)
                alpha_text(img, (x0 + 70, top + 130), ans[:n], f, INK, aa)
        y += block
    # only the area between the header and the mic button shows messages
    clip = Image.new("L", main_img.size, 0)
    ImageDraw.Draw(clip).rectangle((x0, y0 + 116, x1, y1 - 140), fill=255)
    msgs = img
    msgs.putalpha(Image.composite(msgs.getchannel("A"), Image.new("L", msgs.size, 0), clip))
    img = main_img
    compose(img, msgs)
    d = ImageDraw.Draw(img)

    # mic button
    pulse = 0.5 + 0.5 * math.sin(t * 4)
    cx, cy = (x0 + x1) // 2, y1 - 70
    lyr = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ld = ImageDraw.Draw(lyr)
    r = 44 + 8 * pulse
    ld.ellipse((cx - r, cy - r, cx + r, cy + r), fill=ACCENT + (int(60 * win_a),))
    ld.ellipse((cx - 38, cy - 38, cx + 38, cy + 38), fill=(240, 242, 255, int(255 * win_a)))
    img.paste(lyr, (0, 0), lyr)
    dd = ImageDraw.Draw(img)
    c = mix(CARD, ACCENT, win_a)
    dd.rounded_rectangle((cx - 8, cy - 20, cx + 8, cy + 6), 8, fill=c)
    dd.arc((cx - 16, cy - 10, cx + 16, cy + 16), 0, 180, fill=c, width=3)
    dd.line((cx, cy + 16, cx, cy + 22), fill=c, width=3)


def scene_light(img, t):
    # 21-27 s
    t0 = t - 21
    out = 1 - clamp01((t0 - 5.4) / 0.6)
    if t0 < 0 or out <= 0:
        return
    alpha_text(img, (W / 2, 250), "Light enough to leave on all day.", font(F_BOLD, 68), INK, fade(t0, 0) * out, anchor="mm")
    stats = [(60, "MB", "memory when idle"), (0.2, "%", "CPU when idle", "<"), (27, "MB", "download")]
    for i, s in enumerate(stats):
        val, unit, label = s[0], s[1], s[2]
        prefix = s[3] if len(s) > 3 else ""
        cx = W / 2 + (i - 1) * 520
        k = ease((t0 - 0.5 - i * 0.25) / 1.2)
        a = fade(t0, 0.5 + i * 0.25) * out
        num = val * k
        txt = prefix + (f"{num:.1f}" if isinstance(val, float) else f"{int(round(num))}")
        alpha_text(img, (cx, 520), txt, font(F_BOLD, 150), TEAL if i == 0 else INK, a, anchor="mm")
        alpha_text(img, (cx, 630), unit, font(F_SEMI, 46), ACCENT, a, anchor="mm")
        alpha_text(img, (cx, 700), label, font(F_REG, 34), SUB, a, anchor="mm")
    alpha_text(img, (W / 2, 850), "One Rust process. No browser engine. No GPU.", font(F_REG, 36), SUB, fade(t0, 1.8) * out, anchor="mm")


def chips(img, t0, items, y, out, start=0.6, step=0.12, size=40):
    d = ImageDraw.Draw(img)
    f = font(F_SEMI, size)
    widths = [d.textlength(s, font=f) + 70 for s in items]
    gap = 22
    # wrap into rows of max ~1500px
    rows, row, rw = [], [], 0
    for s, w in zip(items, widths):
        if row and rw + w > 1500:
            rows.append((row, rw - gap))
            row, rw = [], 0
        row.append((s, w))
        rw += w + gap
    rows.append((row, rw - gap))
    idx = 0
    for r, (row, rw) in enumerate(rows):
        x = (W - rw) / 2
        for s, w in row:
            a = fade(t0, start + idx * step, 0.4) * out
            dy = 20 * (1 - a)
            lyr = Image.new("RGBA", img.size, (0, 0, 0, 0))
            ImageDraw.Draw(lyr).rounded_rectangle((x, y + r * 110 + dy, x + w, y + r * 110 + 80 + dy), 40, fill=CHIP + (int(255 * a),), outline=LINE + (int(255 * a),), width=2)
            img.paste(lyr, (0, 0), lyr)
            alpha_text(img, (x + w / 2, y + r * 110 + 40 + dy), s, f, INK, a, anchor="mm")
            x += w + gap
            idx += 1


def scene_models(img, t):
    # 27-32 s
    t0 = t - 27
    out = 1 - clamp01((t0 - 4.4) / 0.6)
    if t0 < 0 or out <= 0:
        return
    alpha_text(img, (W / 2, 260), "Your models. Your keys.", font(F_BOLD, 76), INK, fade(t0, 0) * out, anchor="mm")
    chips(img, t0, ["OpenAI", "Anthropic", "Gemini", "Ollama", "Groq", "OpenRouter", "DeepSeek", "Any OpenAI-compatible API"], 420, out)
    alpha_text(img, (W / 2, 800), "Speech recognition runs offline on your PC \u2014 or pick a cloud voice.", font(F_REG, 38), TEAL, fade(t0, 1.8) * out, anchor="mm")


def scene_plugins(img, t):
    # 32-36 s
    t0 = t - 32
    out = 1 - clamp01((t0 - 3.4) / 0.6)
    if t0 < 0 or out <= 0:
        return
    alpha_text(img, (W / 2, 260), "Plugins teach it your apps.", font(F_BOLD, 76), INK, fade(t0, 0) * out, anchor="mm")
    chips(img, t0, ["Spotify", "Outlook", "VS Code", "Slack", "Notion", "Obsidian", "Teams", "Discord", "Telegram", "Zoom"], 420, out, start=0.4, step=0.09)
    alpha_text(img, (W / 2, 800), "Messages, emails and calls always wait for your OK.", font(F_REG, 38), SUB, fade(t0, 1.4) * out, anchor="mm")


def scene_cta(img, t):
    # 36-40 s
    t0 = t - 36
    if t0 < 0:
        return
    a = fade(t0, 0.0, 0.8)
    lg = logo(130)
    lg.putalpha(lg.getchannel("A").point(lambda v: int(v * a)))
    img.paste(lg, ((W - 130) // 2, 250), lg)
    alpha_text(img, (W / 2, 470), "Aeri", font(F_BOLD, 110), INK, a, anchor="mm")
    alpha_text(img, (W / 2, 580), "Free and open source for Windows", font(F_SEMI, 46), SUB, fade(t0, 0.5), anchor="mm")
    k = fade(t0, 1.0)
    d = ImageDraw.Draw(img)
    f = font(F_SEMI, 44)
    s = "github.com/yuqtai/Aeri"
    w = d.textlength(s, font=f) + 90
    lyr = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(lyr).rounded_rectangle(((W - w) / 2, 680, (W + w) / 2, 770), 45, fill=ACCENT + (int(255 * k),))
    img.paste(lyr, (0, 0), lyr)
    alpha_text(img, (W / 2, 725), s, f, (255, 255, 255), k, anchor="mm")


SCENES = [scene_intro, scene_hook, scene_chat, scene_light, scene_models, scene_plugins, scene_cta]
DURATION = 40.0


def frame(t):
    img = background(t)
    for s in SCENES:
        s(img, t)
    # fade in from black at the start, out at the end
    k = min(clamp01(t / 0.5), clamp01((DURATION - t) / 0.6))
    if k < 1:
        img = Image.blend(Image.new("RGB", (W, H), (0, 0, 0)), img, k)
    return img


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "docs" / "promo" / "aeri-promo.mp4"))
    ap.add_argument("--fps", type=int, default=30)
    ap.add_argument("--preview", type=float, nargs="*")
    args = ap.parse_args()
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    if args.preview is not None:
        for t in args.preview:
            p = out.parent / f"preview-{t:05.1f}.png"
            frame(t).save(p)
            print(p)
        return
    n = int(DURATION * args.fps)
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(args.fps), "-i", "-",
           "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(out)]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    for i in range(n):
        proc.stdin.write(frame(i / args.fps).tobytes())
        if i % args.fps == 0:
            print(f"\r{i / args.fps:4.0f}s / {DURATION:.0f}s", end="", file=sys.stderr)
    proc.stdin.close()
    proc.wait()
    print(f"\n{out}")


if __name__ == "__main__":
    main()
