#!/usr/bin/env python3
import re
import sys
from pathlib import Path


TOPBAR = """
<div class="topbar">
  <div class="wrap">
    <a class="brand repo-link" href="/"><span class="mark" aria-hidden="true"><svg viewBox="0 0 32 32" focusable="false"><circle cx="9.4" cy="7.6" r="2.8" fill="currentColor"></circle><path d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13" fill="none" stroke="currentColor" stroke-width="4.2" stroke-linecap="round" stroke-linejoin="round"></path></svg></span><span>iuna</span></a>
    <nav aria-label="Page sections">
      <a href="/#whatisiuna">What is iuna</a>
      <a href="/#howtojoin">Join</a>
      <a href="/git/iuna/file/docs/protocol.md.html">Protocol</a>
      <a href="/git/iuna/file/README.md.html">Source</a>
      <a href="/downloads/">Downloads</a>
    </nav>
  </div>
</div>"""


LINE_RE = re.compile(r'(?P<line><a\s+[^>]*class="line"[^>]*>.*?</a>)(?P<body>.*)', re.S)
BLOB_RE = re.compile(r'(<pre id="blob"[^>]*>)(.*?)(</pre>)', re.S)


def postprocess_html(html: str, path: Path | None = None) -> str:
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
    html = re.sub(r"<tr([^>]*)><td></td><td>", r"<tr\1><td>", html)

    if path is not None and is_markdown_file_page(path):
        html = style_markdown_blob(html)
    return html


def is_markdown_file_page(path: Path) -> bool:
    return path.name.endswith(".md.html") or path.name.endswith(".markdown.html")


def style_markdown_blob(html: str) -> str:
    def replace_blob(match: re.Match[str]) -> str:
        opening, blob, closing = match.groups()
        if re.search(r'class="[^"]*\bmd-source\b', opening):
            return match.group(0)
        if 'class="' in opening:
            opening = re.sub(r'class="([^"]*)"', r'class="\1 md-source"', opening, count=1)
        else:
            opening = opening[:-1] + ' class="md-source">'
        return opening + style_markdown_lines(blob) + closing

    return BLOB_RE.sub(replace_blob, html, count=1)


def style_markdown_lines(blob: str) -> str:
    lines = blob.splitlines(keepends=True)
    in_fence = False
    styled = []
    for line in lines:
        newline = ""
        if line.endswith("\r\n"):
            line, newline = line[:-2], "\r\n"
        elif line.endswith("\n"):
            line, newline = line[:-1], "\n"

        match = LINE_RE.match(line)
        if not match:
            styled.append(line + newline)
            continue

        anchor = match.group("line")
        body = match.group("body")
        stripped = body.lstrip(" ")
        leading = body[: len(body) - len(stripped)]
        if stripped.startswith("```") or stripped.startswith("~~~"):
            in_fence = not in_fence
            body = leading + '<span class="md-fence">' + stripped + "</span>"
        elif not in_fence:
            body = style_markdown_line_body(body)
        styled.append(anchor + body + newline)
    return "".join(styled)


def style_markdown_line_body(body: str) -> str:
    if not body.strip():
        return body

    leading = body[: len(body) - len(body.lstrip(" "))]
    text = body[len(leading) :]
    heading = re.match(r"(#{1,6})(\s+)(.*)", text)
    if heading:
        level = len(heading.group(1))
        return (
            leading
            + f'<span class="md-heading md-heading-{level}">'
            + '<span class="md-marker">'
            + heading.group(1)
            + heading.group(2)
            + "</span>"
            + style_markdown_inline(heading.group(3))
            + "</span>"
        )

    blockquote = re.match(r"(&gt;)(\s?)(.*)", text)
    if blockquote:
        return (
            leading
            + '<span class="md-blockquote"><span class="md-marker">'
            + blockquote.group(1)
            + blockquote.group(2)
            + "</span>"
            + style_markdown_inline(blockquote.group(3))
            + "</span>"
        )

    unordered = re.match(r"([-*+])(\s+)(.*)", text)
    if unordered:
        return leading + style_markdown_marker_line("md-list", unordered)

    ordered = re.match(r"(\d+\.)(\s+)(.*)", text)
    if ordered:
        return leading + style_markdown_marker_line("md-list", ordered)

    thematic_break = re.match(r"((?:[-*_]\s*){3,})$", text)
    if thematic_break:
        return leading + '<span class="md-rule">' + thematic_break.group(1) + "</span>"

    return leading + style_markdown_inline(text)


def style_markdown_marker_line(class_name: str, match: re.Match[str]) -> str:
    return (
        f'<span class="{class_name}"><span class="md-marker">'
        + match.group(1)
        + match.group(2)
        + "</span>"
        + style_markdown_inline(match.group(3))
        + "</span>"
    )


def style_markdown_inline(text: str) -> str:
    spans: list[str] = []

    def stash(class_name: str, value: str) -> str:
        spans.append(f'<span class="{class_name}">{value}</span>')
        return f"\0{len(spans) - 1}\0"

    text = re.sub(r"`([^`\n]+)`", lambda m: stash("md-code", m.group(1)), text)
    text = re.sub(r"\*\*([^*\n]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"__([^_\n]+)__", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<!\*)\*([^*\n]+)\*(?!\*)", r"<em>\1</em>", text)

    def restore(match: re.Match[str]) -> str:
        return spans[int(match.group(1))]

    return re.sub(r"\0(\d+)\0", restore, text)


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
        path.write_text(postprocess_html(html, path), encoding="utf-8")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
