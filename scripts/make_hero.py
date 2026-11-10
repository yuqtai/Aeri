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


TEXT = {
    "zh": {"tagline": "喊一声，剩下的交给它。", "sub": "动嘴就行 · 插件扩展 · 开源免费",
           "chips": ["待机 60 MB", "整机 CPU < 0.2%", "下载 27 MB"], "suffix": "", "shots": ["screen-read.png", "shutdown-confirm.png"], "bold": FONT_BOLD, "regular": FONT},
    "en": {"tagline": "Just say \"Hey Aeri.\" It does the rest.", "sub": "Hands-free · plugins for any app · open source",
           "chips": ["60 MB idle", "< 0.2% CPU", "27 MB download"], "suffix": "-en", "shots": ["en-chat.png", "en-settings.png"],
           "bold": "C:/Windows/Fonts/segoeuib.ttf", "regular": "C:/Windows/Fonts/segoeui.ttf"},
}


def make(name, t, lang="zh"):
    tx = TEXT[lang]
    c = Image.new("RGBA", (W * S, H * S), t["bg"] + (255,))
    glow = Image.new("RGBA", c.size, (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse((980 * S, -120 * S, 1700 * S, 600 * S), fill=t["glow"])
    ImageDraw.Draw(glow).ellipse((-200 * S, 420 * S, 500 * S, 1000 * S), fill=t["glow"][:3] + (t["glow"][3] // 2,))
    c.alpha_composite(glow.filter(ImageFilter.GaussianBlur(140 * S)))

    d = ImageDraw.Draw(c)
    logo = Image.open(ROOT / "assets" / "app.png").convert("RGBA").resize((120 * S, 120 * S), Image.LANCZOS)
    c.alpha_composite(logo, (110 * S, 170 * S))
    d.text((110 * S, 315 * S), "Aeri", font=ImageFont.truetype(FONT_BOLD, 96 * S), fill=t["title"])
    d.text((114 * S, 450 * S), tx["tagline"], font=ImageFont.truetype(tx["bold"], 38 * S), fill=t["text"])
    d.text((114 * S, 510 * S), tx["sub"], font=ImageFont.truetype(tx["regular"], 26 * S), fill=t["sub"])

    chip_font = ImageFont.truetype(tx["regular"], 22 * S)
    x = 114 * S
    for label in tx["chips"]:
        w = d.textlength(label, font=chip_font) + 40 * S
        d.rounded_rectangle((x, 580 * S, x + w, 628 * S), 24 * S, fill=t["chip"])
        d.text((x + 20 * S, 590 * S), label, font=chip_font, fill=t["chip_text"])
        x += w + 14 * S

    # Two app screenshots, overlapping; sized so both fit whatever the screenshot proportions
    width = 360
    for shot, xy in zip(tx["shots"], [(850, 70), (1190, 140)]):
        img = Image.open(OUT / shot).convert("RGBA")
        k = min(width / img.width, 592 / img.height)
        img = img.resize((int(img.width * k * S), int(img.height * k * S)), Image.LANCZOS)
        paste_card(c, rounded(img, 24 * S), (xy[0] * S, xy[1] * S), t["shadow"])

    c.resize((W, H), Image.LANCZOS).convert("RGB").save(OUT / f"hero{tx['suffix']}-{name}.png", optimize=True)


for n, t in THEMES.items():
    for lang in TEXT:
        make(n, t, lang)
print("done")
