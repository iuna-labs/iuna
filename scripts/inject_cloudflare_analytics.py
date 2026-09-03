#!/usr/bin/env python3
import re
import sys
from pathlib import Path


ANALYTICS = """<!-- Cloudflare Web Analytics --><script type='module' src='https://static.cloudflareinsights.com/beacon.min.js' data-cf-beacon='{"token": "a0d6dbbe9b7846f59f24fd2052322cda"}'></script><!-- End Cloudflare Web Analytics -->"""
BODY_CLOSE_RE = re.compile(r"</body\s*>", re.IGNORECASE)


def inject_analytics(html: str) -> str:
    if ANALYTICS in html:
        return html

    matches = list(BODY_CLOSE_RE.finditer(html))
    if not matches:
        raise ValueError("HTML document has no closing body tag")

    closing_body = matches[-1]
    return html[: closing_body.start()] + ANALYTICS + "\n" + html[closing_body.start() :]


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <site-root>", file=sys.stderr)
        return 2

    site_root = Path(sys.argv[1])
    if not site_root.is_dir():
        print(f"error: site root does not exist: {site_root}", file=sys.stderr)
        return 1

    html_paths = sorted(site_root.rglob("*.html"))
    if not html_paths:
        print(f"error: no HTML pages found below {site_root}", file=sys.stderr)
        return 1

    injected = 0
    for path in html_paths:
        html = path.read_text(encoding="utf-8")
        updated_html = inject_analytics(html)
        if updated_html == html:
            continue
        path.write_text(updated_html, encoding="utf-8")
        injected += 1

    print(
        f"Cloudflare Web Analytics present on {len(html_paths)} HTML pages "
        f"({injected} injected)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
