#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Check that an agent guidelines file (CLAUDE.md) stays easy to vet.

Each rule is a top-level bullet that opens with the rule in bold, one
sentence a reader can take on its own; the rest of the bullet says how
to apply it, briefly, with steps in sub-bullets; and history (what went
wrong, when) sits in a `Background:` sub-bullet, the only place a date
appears. So reading the bold text gives every rule, and a reviewer can
skip the history.

Usage: check_guidelines.py FILE...   (exit 1 on any finding)
"""

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

# A bold lead longer than this is no longer one sentence to read at a
# glance; a bullet's own text longer than this is a procedure, which
# belongs in DEVELOPMENT.md or RELEASING.md with the rule pointing there.
MAX_LEAD_WORDS = 30
MAX_BULLET_WORDS = 80

DATE = re.compile(r"\b\d{4}-\d{2}-\d{2}\b")
BULLET = re.compile(r"^( *)[-*] ")
BACKGROUND = re.compile(r"^(\*\*)?Background\b")


@dataclass
class Item:
    """One bullet: its indent, first line number and text lines."""

    indent: int
    line: int
    lines: list = field(default_factory=list)

    @property
    def text(self):
        return " ".join(s.strip() for s in self.lines)


def items(text):
    """The bullets of a Markdown file, each with its continuation lines.

    Yields `(section, item)`, where `section` is the `##` heading the
    bullet sits under (None before the first). Fenced code is skipped.
    """
    section = None
    current = None
    fenced = False
    for n, raw in enumerate(text.splitlines(), start=1):
        if raw.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        if raw.startswith("#"):
            if current:
                yield section, current
                current = None
            if raw.startswith("## "):
                section = raw[3:].strip()
            continue
        m = BULLET.match(raw)
        if m:
            if current:
                yield section, current
            indent = len(m.group(1))
            current = Item(indent, n, [raw[m.end():]])
        elif raw.strip() and current and raw.startswith(" "):
            current.lines.append(raw)
        else:
            if current:
                yield section, current
                current = None
    if current:
        yield section, current


def bold_lead(text):
    """The bold text a bullet opens with, or None."""
    if not text.startswith("**"):
        return None
    end = text.find("**", 2)
    return text[2:end] if end > 2 else None


def check(text):
    """Findings for one file's text, as `(line, message)` pairs."""
    found = []
    for section, item in items(text):
        body = item.text
        if DATE.search(body) and not BACKGROUND.match(body):
            found.append((item.line, "a date outside a Background: bullet"))
        if section is None or item.indent:
            continue
        lead = bold_lead(body)
        if lead is None:
            found.append((item.line, "a rule that does not open with its bold lead"))
            continue
        words = len(lead.split())
        if words > MAX_LEAD_WORDS:
            found.append((item.line, f"a bold lead of {words} words (at most {MAX_LEAD_WORDS})"))
        words = len(body.split())
        if words > MAX_BULLET_WORDS:
            found.append((item.line, f"a rule of {words} words (at most {MAX_BULLET_WORDS})"))
    return found


def main(argv):
    if not argv:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    status = 0
    for name in argv:
        for line, message in check(Path(name).read_text()):
            print(f"{name}:{line}: {message}")
            status = 1
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
