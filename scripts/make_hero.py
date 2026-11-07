"""生成 README 顶部头图：左边 Logo + 标语，右边两张对话截图（浅色 / 深色两版）。
运行：python scripts/make_hero.py（需要 Pillow）"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "screenshots"
FONT = "C:/Windows/Fonts/msyh.ttc"
FONT_BOLD = "C:/Windows/Fonts/msyhbd.ttc"
W, H = 1600, 760
S = 2  # 先按 2 倍画，最后缩小，边缘更平滑

THEMES = {
    "light": {"bg": (246, 247, 251), "glow": (99, 102, 241, 60), "title": (17, 24, 39), "text": (75, 85, 99), "sub": (107, 114, 128), "chip": (255, 255, 255), "chip_text": (55, 65, 81), "shadow": 60},
    "dark": {"bg": (15, 17, 26), "glow": (99, 102, 241, 90), "title": (243, 244, 246), "text": (209, 213, 219), "sub": (156, 163, 175), "chip": (31, 35, 50), "chip_text": (209, 213, 219), "shadow": 160},
}


def rounded(img, r):
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, *img.size), r, fill=255)
    out = Image.new("RGBA", img.size)
    out.paste(img, (0, 0), mask)
    return out


def paste_card(canvas, img, xy, shadow_alpha):
    x, y = xy
    sh = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(sh).rounded_rectangle((x + 8 * S, y + 20 * S, x + img.width + 8 * S, y + img.height + 20 * S), 28 * S, fill=(0, 0, 0, shadow_alpha))
    canvas.alpha_composite(sh.filter(ImageFilter.GaussianBlur(26 * S)))
    canvas.alpha_composite(img, (x, y))


def make(name, t):
    c = Image.new("RGBA", (W * S, H * S), t["bg"] + (255,))
    glow = Image.new("RGBA", c.size, (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse((980 * S, -120 * S, 1700 * S, 600 * S), fill=t["glow"])
    ImageDraw.Draw(glow).ellipse((-200 * S, 420 * S, 500 * S, 1000 * S), fill=t["glow"][:3] + (t["glow"][3] // 2,))
    c.alpha_composite(glow.filter(ImageFilter.GaussianBlur(140 * S)))

    d = ImageDraw.Draw(c)
    logo = Image.open(ROOT / "assets" / "app.png").convert("RGBA").resize((120 * S, 120 * S), Image.LANCZOS)
    c.alpha_composite(logo, (110 * S, 170 * S))
    d.text((110 * S, 315 * S), "Aeri", font=ImageFont.truetype(FONT_BOLD, 96 * S), fill=t["title"])
    d.text((114 * S, 450 * S), "常驻一整天，也感觉不到它在。", font=ImageFont.truetype(FONT_BOLD, 38 * S), fill=t["text"])
    d.text((114 * S, 510 * S), "Windows 上的中文语音助手 · 说一句，它帮你做完", font=ImageFont.truetype(FONT, 26 * S), fill=t["sub"])

    chip_font = ImageFont.truetype(FONT, 22 * S)
    x = 114 * S
    for label in ["待机 60 MB", "整机 CPU < 0.2%", "下载 22 MB"]:
        w = d.textlength(label, font=chip_font) + 40 * S
        d.rounded_rectangle((x, 580 * S, x + w, 628 * S), 24 * S, fill=t["chip"])
        d.text((x + 20 * S, 590 * S), label, font=chip_font, fill=t["chip_text"])
        x += w + 14 * S

    for i, (shot, xy) in enumerate([("screen-read.png", (870, 70)), ("shutdown-confirm.png", (1200, 130))]):
        img = Image.open(OUT / shot).convert("RGBA")
        img = img.resize((int(img.width * 0.78 * S), int(img.height * 0.78 * S)), Image.LANCZOS)
        paste_card(c, rounded(img, 24 * S), (xy[0] * S, xy[1] * S), t["shadow"])

    c.resize((W, H), Image.LANCZOS).convert("RGB").save(OUT / f"hero-{name}.png", optimize=True)


for n, t in THEMES.items():
    make(n, t)
print("done")
