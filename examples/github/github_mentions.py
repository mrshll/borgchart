"""Conservative explicit-address policy for GitHub comment bodies.

This is an application addressing rule, not GitHub mention or CommonMark
emulation. A body addresses ``login`` only when a raw plain line starts with
up to three ASCII spaces and ``@login`` (case-insensitive), followed by end of
line, whitespace or ordinary punctuation (``:,.!?;)]``) that is itself
followed by end of line or whitespace.

The line scanner models only what it can classify with certainty and fails
toward no match otherwise:

* Top-level fenced code (backticks or tildes, 0-3 spaces) is skipped exactly.
  A fence-like line in any other context (after list markers, indented four
  or more columns, backtick info containing a backtick), an interior line
  indented less than its opener, or an ambiguously indented closer disables
  every later line, because a list container could change fence parity.
* Blockquotes, including lazy continuation lines, are skipped until a blank
  line.
* HTML blocks are skipped until their end marker or blank line. Blocks that
  start at column 0 with a known HTML type 1-6 opener are trusted by this
  policy (our reading of CommonMark, not a verified GitHub guarantee); all others are a superset region in which fence-like lines disable
  the rest of the body and after which the paragraph stays ineligible.
* Within a blank-line-delimited paragraph, a line that may leave an inline
  construct open (unbalanced backtick runs, escaped backticks, ``<`` mixed with
  backticks, an unclosed tag, ``[`` or ``(``, an odd ``$``, a leading ``[``)
  makes the rest of that paragraph ineligible. An odd ``$$`` disables the rest
  of the body.
* HTML comments and raw ``code``/``pre``/``a``-style elements are a mask from
  the opener until the closer is seen in a trusted context; unclosed means
  the rest of the body.
* Setext heading text and paragraphs containing a table delimiter row are not
  addresses.

Every step is linear in the body length; the caller bounds body size.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

_LOGIN = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}")
_NEWLINE = re.compile(r"\r\n?|\n")
_ADDRESS = re.compile(r" {0,3}@([A-Za-z0-9-]+)")
_ADDRESS_END = re.compile(r"[:,.!?;)\]]*(?:[ \t]|$)")
_LIST_MARKER = re.compile(r"(?:[-+*]|[0-9]{1,9}[.)])(?=[ \t])")
_BACKTICKS = re.compile(r"`+")
_BRACKET = re.compile(r"\\.|[\[\]()]")
_TAG_OPEN = re.compile(r"<[A-Za-z/!?]")
_DOLLAR = re.compile(r"\\.|\$")
_TAG = re.compile(r"<!--|-->|<(/?)([A-Za-z][A-Za-z0-9]*)(?=[\s/>]|$)")
# A line that cannot open, close or address anything: ordinary paragraph text.
_PLAIN = re.compile(r"[^\s`<>\[\]()$\\|~=*+#@0-9-][^`<>\[\]()$\\|]*")
_HTML_NAME = re.compile(r"</?([a-z][a-z0-9-]*)")
_SETEXT = re.compile(r" {0,3}(?:=+|-+)[ \t]*")
_TABLE_DELIMITER = re.compile(
    r" {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*"
)

# Elements excluded by this application addressing policy.
_RAW_TAGS = frozenset(
    [
        "a",
        "code",
        "pre",
        "script",
        "style",
        "textarea",
        "kbd",
        "samp",
        "tt",
        "xmp",
        "plaintext",
        "listing",
        "noscript",
        "template",
        "iframe",
        "svg",
        "math",
        "title",
        "noembed",
        "noframes",
    ]
)
_TYPE1_TAGS = frozenset(("pre", "script", "style", "textarea"))
_TYPE6_TAGS = frozenset(
    [
        "address",
        "article",
        "aside",
        "base",
        "basefont",
        "blockquote",
        "body",
        "caption",
        "center",
        "col",
        "colgroup",
        "dd",
        "details",
        "dialog",
        "dir",
        "div",
        "dl",
        "dt",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "frame",
        "frameset",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hr",
        "html",
        "iframe",
        "legend",
        "li",
        "link",
        "main",
        "menu",
        "menuitem",
        "nav",
        "noframes",
        "ol",
        "optgroup",
        "option",
        "p",
        "param",
        "search",
        "section",
        "summary",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "title",
        "tr",
        "track",
        "ul",
    ]
)


class _Poisoned(Exception):
    """Structure became ambiguous; no later line may qualify."""


def _indent(line: str, pos: int = 0) -> tuple[int, int]:
    column = 0
    while pos < len(line) and line[pos] in " \t":
        column = column + 1 if line[pos] == " " else column + 4 - column % 4
        pos += 1
    return column, pos


def _strip_containers(line: str) -> tuple[int, int, bool]:
    """Return first indent, content position and whether markers were seen."""
    first, pos = _indent(line)
    markers = False
    while True:
        if pos < len(line) and line[pos] == ">":
            pos += 1
        elif match := _LIST_MARKER.match(line, pos):
            pos = match.end()
        else:
            return first, pos, markers
        markers = True
        _, pos = _indent(line, pos)


def _fence_like(line: str, pos: int) -> bool:
    return line.startswith("```", pos) or line.startswith("~~~", pos)


def _blank(line: str) -> bool:
    return not line.strip(" \t")


def _code_spans(line: str) -> list[tuple[int, int]] | None:
    """Pair backtick runs like CommonMark; None when any run is unmatched."""
    runs = [match.span() for match in _BACKTICKS.finditer(line)]
    spans = []
    index = 0
    while index < len(runs):
        length = runs[index][1] - runs[index][0]
        close = index + 1
        while close < len(runs) and runs[close][1] - runs[close][0] != length:
            close += 1
        if close == len(runs):
            return None
        spans.append((runs[index][0], runs[close][1]))
        index = close + 1
    return spans


def _inline(line: str) -> tuple[bool, str]:
    """Return whether the line may leave an inline construct open, and its
    text with code spans blanked. Raises _Poisoned for an odd ``$$``."""
    tainted = False
    masked = line
    if "`" in line:
        # A tag before a backtick takes precedence over the span; a span
        # opened first still hides tags, so masking stays valid for openers.
        tainted = "<" in line
        spans = None if "\\`" in line else _code_spans(line)
        if spans is None:
            tainted = True
        else:
            pieces = []
            last = 0
            for start, end in spans:
                pieces.append(line[last:start])
                pieces.append(" " * (end - start))
                last = end
            pieces.append(line[last:])
            masked = "".join(pieces)
    if masked.count("$$") % 2:
        raise _Poisoned
    if tainted:
        return True, masked
    if masked.lstrip(" \t").startswith("["):
        return True, masked
    depth = {"[": 0, "(": 0}
    for match in _BRACKET.finditer(masked):
        char = match.group()
        if char in depth:
            depth[char] += 1
        elif char == "]" and depth["["]:
            depth["["] -= 1
        elif char == ")" and depth["("]:
            depth["("] -= 1
    if depth["["] or depth["("]:
        return True, masked
    opener = None
    for match in _TAG_OPEN.finditer(masked):
        opener = match.start()
    if opener is not None and ">" not in masked[opener:]:
        return True, masked
    dollars = sum(1 for match in _DOLLAR.finditer(masked) if match.group() == "$")
    return dollars % 2 == 1, masked


def _html_start(line: str, pos: int) -> tuple[str | None, bool, int]:
    """Return end marker (None: blank line), trusted type and search start."""
    lower = line.lower()
    if line.startswith("<!--", pos):
        return "-->", True, pos + 4
    if line.startswith("<?", pos):
        return "?>", True, pos + 2
    if line.startswith("<![CDATA[", pos):
        return "]]>", True, pos + 9
    if line.startswith("<!", pos):
        return (
            ">",
            pos + 2 < len(line) and line[pos + 2].isascii() and line[pos + 2].isalpha(),
            pos + 2,
        )
    match = _HTML_NAME.match(lower, pos)
    if match is None:
        return None, False, pos
    rest = lower[match.end() :]
    name = match.group(1)
    closing = lower.startswith("</", pos)
    if not closing and name in _TYPE1_TAGS and (not rest or rest[0] in " \t>"):
        return f"</{name}>", True, match.end()
    bounded = not rest or rest[0] in " \t>" or rest.startswith("/>")
    return None, name in _TYPE6_TAGS and bounded, match.end()


@dataclass
class _Scanner:
    login: str
    chunk_tainted: bool = False
    chunk_table: bool = False
    candidates: list[int] = field(default_factory=list)
    in_quote: bool = False
    fence: tuple[str, int, int] | None = None
    region_ends: set[str] = field(default_factory=set)
    region_blank: bool = False
    region_trusted: bool = False
    in_comment: bool = False
    raw_depth: dict[str, int] = field(default_factory=dict)

    def run(self, body: str) -> bool:
        try:
            for index, line in enumerate(_NEWLINE.split(body)):
                if self.line(index, line):
                    return True
            return self.finish_chunk()
        except _Poisoned:
            return False

    @property
    def in_region(self) -> bool:
        return bool(self.region_ends) or self.region_blank

    @property
    def masked(self) -> bool:
        return self.in_comment or any(self.raw_depth.values())

    def finish_chunk(self) -> bool:
        found = bool(self.candidates) and not self.chunk_table
        self.candidates.clear()
        self.chunk_table = False
        return found

    def line(self, index: int, line: str) -> bool:
        if self.fence is not None:
            self.fence_line(line)
            return False
        if not (self.in_region or self.in_quote) and _PLAIN.fullmatch(line):
            return False
        if _blank(line):
            self.in_quote = False
            self.region_blank = False
            self.chunk_tainted = False
            return self.finish_chunk()
        if self.in_region:
            self.region_line(line)
            return False
        indent, pos = _indent(line)
        if indent <= 3 and line.startswith(">", pos):
            self.in_quote = True
            self.scan_tags(line, closers=False)
            return self.finish_chunk()
        first, content, markers = _strip_containers(line)
        fence_like = _fence_like(line, content)
        html = _TAG_OPEN.match(line, content) is not None
        if self.in_quote:
            if not (fence_like or html):
                self.scan_tags(line, closers=False)
                return False
            self.in_quote = False
        if fence_like:
            found = self.finish_chunk()
            if found:
                return True
            self.open_fence(line, first, content, markers)
            return False
        if html:
            found = self.finish_chunk()
            self.open_region(line, content, trusted=first == 0 and not markers)
            return found
        self.paragraph_line(index, line)
        return False

    def open_fence(self, line: str, indent: int, pos: int, markers: bool) -> None:
        char = line[pos]
        end = pos
        while end < len(line) and line[end] == char:
            end += 1
        if markers or indent > 3 or (char == "`" and "`" in line[end:]):
            raise _Poisoned
        self.fence = (char, end - pos, indent)

    def fence_line(self, line: str) -> None:
        if _blank(line):
            return
        char, length, opener = self.fence
        indent, pos = _indent(line)
        if indent < opener:
            raise _Poisoned
        end = pos
        while end < len(line) and line[end] == char:
            end += 1
        if end - pos < length or not _blank(line[end:]):
            return
        if indent <= 3:
            self.fence = None
            self.chunk_tainted = False
        elif indent <= 3 + opener:
            raise _Poisoned

    def open_region(self, line: str, pos: int, trusted: bool) -> None:
        end, known, search = _html_start(line, pos)
        self.region_trusted = trusted and known
        self.require(line, end, search)
        self.scan_tags(line, closers=self.region_trusted)
        self.after_region_line()

    def require(self, line: str, end: str | None, search: int) -> None:
        if end is None:
            self.region_blank = True
        elif end not in line.lower()[search:]:
            self.region_ends.add(end)

    def region_line(self, line: str) -> None:
        lower = line.lower()
        self.region_ends = {end for end in self.region_ends if end not in lower}
        if not self.region_trusted:
            _, content, _ = _strip_containers(line)
            if _fence_like(line, content):
                raise _Poisoned
            if _TAG_OPEN.match(line, content):
                end, _, search = _html_start(line, content)
                self.require(line, end, search)
        self.scan_tags(line, closers=self.region_trusted)
        self.after_region_line()

    def after_region_line(self) -> None:
        if not self.in_region:
            # Ended on a non-blank line: an untrusted region may have been
            # paragraph text in GitHub, so the paragraph stays ineligible.
            self.chunk_tainted = not self.region_trusted

    def paragraph_line(self, index: int, line: str) -> None:
        clean = not self.chunk_tainted
        if clean and not self.masked and self.addresses(line):
            self.candidates.append(index)
        if _SETEXT.fullmatch(line) and self.candidates[-1:] == [index - 1]:
            self.candidates.pop()
        if "|" in line and _TABLE_DELIMITER.fullmatch(line):
            self.chunk_table = True
        try:
            tainted, masked = _inline(line)
        except _Poisoned:
            self.candidates.clear()
            raise
        self.chunk_tainted |= tainted
        self.scan_tags(masked, closers=clean and not tainted)

    def addresses(self, line: str) -> bool:
        match = _ADDRESS.match(line)
        return (
            match is not None
            and match.group(1).lower() == self.login
            and _ADDRESS_END.match(line, match.end()) is not None
        )

    def scan_tags(self, text: str, closers: bool) -> None:
        if "<" not in text and not (self.in_comment and "-->" in text):
            return
        for match in _TAG.finditer(text):
            token = match.group()
            if self.in_comment:
                self.in_comment = token != "-->"
            elif token == "<!--":
                self.in_comment = True
            elif token != "-->":
                name = match.group(2).lower()
                if name not in _RAW_TAGS:
                    continue
                if not match.group(1):
                    self.raw_depth[name] = self.raw_depth.get(name, 0) + 1
                elif closers and self.raw_depth.get(name):
                    self.raw_depth[name] -= 1


def explicit_mention(body: str, login: str) -> bool:
    """True when some plain line of body starts with an explicit @login."""
    if not isinstance(body, str) or not isinstance(login, str):
        raise TypeError("body and login must be strings")
    if not _LOGIN.fullmatch(login):
        raise ValueError("login must be a GitHub login")
    login = login.lower()
    # Cheap prefilter: ASCII lowercasing is exact, so no match is possible.
    if f"@{login}" not in body.lower():
        return False
    return _Scanner(login).run(body)
