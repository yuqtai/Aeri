"""Check local Markdown/HTML documentation links using only the standard library."""

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
FILES = [ROOT / "README.md", ROOT / "README.zh-CN.md", *sorted((ROOT / "docs").rglob("*.md"))]


def prose(text):
    return re.sub(r"^(`{3,}|~{3,}).*?^\1[^\n]*$", "", text, flags=re.M | re.S)


def anchors(path):
    text = prose(path.read_text(encoding="utf-8"))
    result = set(re.findall(r'(?:id|name)=["\']([^"\']+)["\']', text))
    counts = {}
    for title in re.findall(r"^#{1,6} +(.+?) *#* *$", text, flags=re.M):
        title = re.sub(r"!?\[([^]]+)\]\([^)]*\)", r"\1", title)
        slug = re.sub(r"[^\w -]", "", title.lower()).replace(" ", "-")
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        result.add(slug if count == 0 else f"{slug}-{count}")
    return result


def links(text):
    text = prose(text)
    text = re.sub(r"`[^`\n]*`", "", text)
    for match in re.finditer(r"!?\[[^]\n]*\]\(\s*(<[^>]+>|[^\s)]+)(?:\s+[^)]*)?\)", text):
        yield match.group(1).strip("<>")
    for match in re.finditer(r'^ {0,3}\[[^]]+\]:\s*(<[^>]+>|\S+)', text, flags=re.M):
        yield match.group(1).strip("<>")
    for match in re.finditer(r'(?:href|src)=["\']([^"\']+)["\']', text):
        yield match.group(1)
    for match in re.finditer(r'srcset=["\']([^"\']+)["\']', text):
        for item in match.group(1).split(","):
            yield item.strip().split()[0]


def main():
    checked = 0
    failures = []
    for source in FILES:
        for link in links(source.read_text(encoding="utf-8")):
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc:
                continue
            checked += 1
            target = (source.parent / unquote(parsed.path)).resolve() if parsed.path else source
            if not target.exists():
                failures.append(f"{source.relative_to(ROOT)}: {link} -> missing file")
            elif parsed.fragment and target.suffix.lower() == ".md" and unquote(parsed.fragment) not in anchors(target):
                failures.append(f"{source.relative_to(ROOT)}: {link} -> missing heading")
    print(f"Checked {len(FILES)} Markdown files and {checked} relative links (including images and heading fragments).")
    print(f"Broken links: {len(failures)}")
    for failure in failures:
        print(failure)
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
