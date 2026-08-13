#!/usr/bin/env python3
import re
import sys
from pathlib import Path


TOPBAR = """
<div class="topbar">
  <div class="wrap">
    <a class="brand repo-link" href="/"><span class="mark" aria-label="iuna"><svg viewBox="0 0 32 32" aria-hidden="true" focusable="false"><circle class="mark-dot" cx="9.4" cy="7.6" r="2.8"></circle><path class="mark-loop" d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13"></path></svg></span><span>iuna</span></a>
    <nav aria-label="Page sections">
      <a href="/#whatisiuna">What is iuna</a>
      <a href="/#howtojoin">Join</a>
      <a href="/git/iuna/file/docs/protocol.md.html">Protocol</a>
      <a href="/git/iuna/file/README.md.html">Source</a>
      <a href="/downloads/">Downloads</a>
    </nav>
  </div>
</div>"""


def postprocess_html(html: str) -> str:
    if '<div class="topbar">' not in html:
        html = re.sub(r"<body([^>]*)>", r"<body\1>\n" + TOPBAR, html, count=1)

    html = re.sub(
        r"<td id=\"logo\"[^>]*>.*?</td>",
        "",
        html,
        count=1,
        flags=re.S,
    )
    html = re.sub(
        r"<td><a href=\"[^\"]*\"><img src=\"[^\"]*logo\.png\"[^>]*></a></td>",
        "",
        html,
        count=1,
        flags=re.S,
    )
    return re.sub(r"<tr([^>]*)><td></td><td>", r"<tr\1><td>", html)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <generated-git-root>", file=sys.stderr)
        return 2

    git_root = Path(sys.argv[1])
    if not git_root.is_dir():
        print(f"error: generated git root does not exist: {git_root}", file=sys.stderr)
        return 1

    for path in git_root.rglob("*.html"):
        html = path.read_text(encoding="utf-8")
        path.write_text(postprocess_html(html), encoding="utf-8")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
