#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""sandogasa's development trajectory, release by release (#20).

Walks every `v*` tag (oldest first), extracts its tree with `git archive`
into a temporary directory, counts its lines with tokei, and writes one
JSON row per tag: lines by language, Rust code and test lines apart, the
number of crates and tools, the commits and lines changed since the
previous tag, the days between them, a basic COCOMO estimate, and when the
release's commits were made.

COCOMO's estimate assumes a human team working at a human pace; next to the
actual days between tags it measures this project's AI-assisted pace, not
its value.

Commit times are commit times, not work times. Each author's commits are
normalized by day: the offset most of an author's commits carry on a (UTC)
day is taken as theirs, and a commit with another offset (one made in a web
interface set to an old zone, say) is converted to it. Interactively, each
such commit is asked about, defaulting to the majority, and the answer is
kept in timezones.toml beside this script; without a terminal the recorded
answers and the majority are used. The public output carries only coarse
shares per release; `--private` adds the weekday x hour heatmap.

Needs git, tokei (15) and Python 3.11 or later; nothing else.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
TIMEZONES = HERE / "timezones.toml"

# Basic COCOMO, organic mode, with the constants the `cocomo` crate uses,
# so the figures match that tool's.
COCOMO_A, COCOMO_B = 2.4, 1.05
COCOMO_C, COCOMO_D = 2.5, 0.38
AVERAGE_WAGE = 56286.0
OVERHEAD = 2.4

# Bands of the day, in local hours: [start, end).
BANDS = (("night", 0, 6), ("daytime", 6, 18), ("evening", 18, 24))
WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")

# `#[cfg(test)]`, or one that needs test among other things, alone on its
# line or before the module on the same line.
TEST_ATTR = re.compile(r"^\s*#\[cfg\((test|(all|any)\(.*\btest\b.*\))\)\]\s*(?P<rest>.*)$")
MOD_LINE = re.compile(r"^\s*(pub(\(crate\))?\s+)?mod\s+\w+\s*\{")


def cocomo(sloc: int) -> dict:
    """Basic organic COCOMO for `sloc` lines of code."""
    effort = COCOMO_A * (sloc / 1000) ** COCOMO_B if sloc else 0.0
    schedule = COCOMO_C * effort**COCOMO_D if effort else 0.0
    return {
        "sloc": sloc,
        "effort_person_months": round(effort, 2),
        "schedule_months": round(schedule, 2),
        "people": round(effort / schedule, 2) if schedule else 0.0,
        "cost": round(effort * (AVERAGE_WAGE / 12) * OVERHEAD, 2),
    }


def test_code_lines(text: str) -> int:
    """Code lines in a Rust file's test module: from its last
    `#[cfg(test)] mod ... {` (the project's ordering rule puts the tests
    last) to the end of the file, leaving out blank lines, `//` comments
    and block comments."""
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        m = TEST_ATTR.match(line)
        if not m:
            continue
        rest = m.group("rest")
        if (rest and MOD_LINE.match(rest)) or (
            not rest and i + 1 < len(lines) and MOD_LINE.match(lines[i + 1])
        ):
            start = i
    if start is None:
        return 0
    count = 0
    in_block = False
    for line in lines[start:]:
        stripped = line.strip()
        if in_block:
            if "*/" in stripped:
                in_block = False
            continue
        if stripped.startswith("/*"):
            in_block = "*/" not in stripped
            continue
        if stripped and not stripped.startswith("//"):
            count += 1
    return count


def is_test_file(path: str) -> bool:
    """A Rust file that is a test as a whole: under a `tests/` directory."""
    return "/tests/" in "/" + path


def count_tree(tree: Path) -> dict:
    """tokei's counts for `tree`, by language, and Rust code split into
    code and tests."""
    out = subprocess.run(
        ["tokei", "--output", "json", "."],
        cwd=tree,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    data = json.loads(out)
    languages = {}
    for name, stats in data.items():
        if name == "Total" or not stats.get("code") and not stats.get("comments"):
            continue
        languages[name] = {k: stats.get(k, 0) for k in ("code", "comments", "blanks")}
    rust_tests = 0
    for report in data.get("Rust", {}).get("reports", []):
        path = report["name"].removeprefix("./")
        if is_test_file(path):
            rust_tests += report["stats"]["code"]
            continue
        try:
            text = (tree / path).read_text(errors="replace")
        except OSError:
            continue
        rust_tests += test_code_lines(text)
    rust = languages.get("Rust", {}).get("code", 0)
    total = data.get("Total", {}).get("code", 0)
    return {
        "languages": languages,
        "code": total,
        "rust_code": rust - rust_tests,
        "rust_tests": rust_tests,
    }


def git(*args: str, repo: Path) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


RELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+$")


def tags(repo: Path, tip: str = "HEAD") -> list[str]:
    """The release tags (`vX.Y.Z`) in `tip`'s history, oldest first: a
    tag fetched from another repository is not one of them."""
    listed = git("tag", "--list", "v*", "--merged", tip, "--sort=v:refname", repo=repo)
    return [t for t in listed.split() if RELEASE_TAG.match(t)]


def members(tree: Path, kind: str) -> int:
    """How many crates under `kind` (`crates` or `tools`) the tree has."""
    base = tree / kind
    if not base.is_dir():
        return 0
    return sum(1 for d in base.iterdir() if (d / "Cargo.toml").is_file())


def history(repo: Path, prev: str | None, tag: str) -> dict:
    """Commits and lines changed between `prev` and `tag`."""
    span = f"{prev}..{tag}" if prev else tag
    commits = int(git("rev-list", "--count", span, repo=repo).strip())
    added = removed = 0
    if prev:
        for line in git("diff", "--numstat", prev, tag, repo=repo).splitlines():
            a, r, _ = line.split("\t", 2)
            if a != "-":
                added += int(a)
                removed += int(r)
    return {"commits": commits, "lines_added": added, "lines_removed": removed}


def tag_date(repo: Path, tag: str) -> dt.datetime:
    stamp = git("log", "-1", "--format=%cI", tag, repo=repo).strip()
    return dt.datetime.fromisoformat(stamp)


# --- Commit times ---------------------------------------------------------


def parse_offset(text: str) -> dt.timezone:
    """`+0100` as a time zone."""
    sign = -1 if text.startswith("-") else 1
    hours, minutes = int(text[1:3]), int(text[3:5])
    return dt.timezone(sign * dt.timedelta(hours=hours, minutes=minutes))


def format_offset(tz: dt.tzinfo, at: dt.datetime) -> str:
    delta = tz.utcoffset(at) or dt.timedelta()
    sign = "-" if delta < dt.timedelta() else "+"
    minutes = abs(int(delta.total_seconds())) // 60
    return f"{sign}{minutes // 60:02d}{minutes % 60:02d}"


def read_commits(repo: Path, authors: list[str], tip: str) -> list[dict]:
    """The commits reachable from `tip` by `authors` (all when empty):
    hash, author, subject, and author date with its own offset."""
    out = git(
        "log", "--format=%H%x00%an%x00%aI%x00%s", tip, repo=repo
    )
    commits = []
    for line in out.splitlines():
        sha, author, stamp, subject = line.split("\0", 3)
        if authors and author not in authors:
            continue
        commits.append(
            {
                "sha": sha,
                "author": author,
                "at": dt.datetime.fromisoformat(stamp),
                "subject": subject,
            }
        )
    return commits


def own_offset(c: dict) -> str:
    return format_offset(c["at"].tzinfo, c["at"])


def clock_change(group: list[dict]) -> bool:
    """Whether a day's offsets are a clock change: two offsets an hour
    apart, the day's commits on one side of a single switch."""
    ordered = sorted(group, key=lambda c: c["at"])
    offsets = [c["at"].utcoffset() for c in ordered]
    switches = sum(1 for a, b in zip(offsets, offsets[1:]) if a != b)
    return switches == 1 and abs(offsets[0] - offsets[-1]) == dt.timedelta(hours=1)


def strays(commits: list[dict]) -> list[tuple[dict, str | None]]:
    """The commits whose offset differs from the one most of their
    author's commits carry that (UTC) day, with that majority offset, or
    None when no offset has a majority (a tie). A day the clocks changed
    on has no strays."""
    by_day = collections.defaultdict(list)
    for c in commits:
        day = c["at"].astimezone(dt.timezone.utc).date()
        by_day[(c["author"], day)].append(c)
    found = []
    for group in by_day.values():
        offsets = collections.Counter(own_offset(c) for c in group)
        if len(offsets) < 2 or clock_change(group):
            continue
        (first, n), (_, m) = offsets.most_common(2)
        majority = first if n > m else None
        for c in group:
            if own_offset(c) != majority:
                found.append((c, majority))
    return found


def load_answers(path: Path) -> dict[str, str]:
    if not path.exists():
        return {}
    with path.open("rb") as f:
        return dict(tomllib.load(f).get("commits", {}))


def save_answers(path: Path, answers: dict[str, str]) -> None:
    lines = [
        "# Offsets to read commits in, for commits whose recorded offset",
        "# differs from the one most of their author's commits carry that day",
        "# (reports/trajectory/trajectory.py asks, and keeps the answer here).",
        "",
        "[commits]",
    ]
    lines += [f'"{sha}" = "{offset}"' for sha, offset in sorted(answers.items())]
    path.write_text("\n".join(lines) + "\n")


def normalize(
    commits: list[dict],
    answers: dict[str, str],
    ask,
) -> tuple[dict[str, str], list[str]]:
    """Each stray commit's offset: the recorded answer, else what `ask`
    says (None: not asked), else the day's majority, or its own offset
    on a day with none. Returns the answers, the new ones included, and
    a line per commit decided without asking."""
    answers = dict(answers)
    decided = []
    for c, majority in strays(commits):
        if c["sha"] in answers:
            continue
        own = own_offset(c)
        answer = ask(c, own, majority)
        if answer is None:
            stamp = f"{c['sha'][:10]} {c['at']:%Y-%m-%d %H:%M} {own}"
            if majority is None:
                decided.append(f"{stamp}: kept, no offset has a majority that day: {c['subject']}")
                continue
            decided.append(f"{stamp}: read in {majority} (most of that day's): {c['subject']}")
            answer = majority
        answers[c["sha"]] = answer
    return answers, decided


def local_times(commits: list[dict], answers: dict[str, str]) -> dict[str, dt.datetime]:
    """Each commit's author time in the offset it is read in."""
    out = {}
    for c in commits:
        if c["sha"] in answers:
            out[c["sha"]] = c["at"].astimezone(parse_offset(answers[c["sha"]]))
        else:
            out[c["sha"]] = c["at"]
    return out


def band(hour: int) -> str:
    return next(name for name, start, end in BANDS if start <= hour < end)


def shares(times: list[dt.datetime]) -> dict:
    """The coarse shares a public report shows."""
    n = len(times)
    weekend = sum(1 for t in times if t.weekday() >= 5)
    bands = collections.Counter(band(t.hour) for t in times)
    return {
        "commits": n,
        "weekend": weekend,
        **{name: bands.get(name, 0) for name, _, _ in BANDS},
    }


def heatmap(times: list[dt.datetime]) -> dict:
    grid = collections.Counter((t.weekday(), t.hour) for t in times)
    return {
        "weekdays": list(WEEKDAYS),
        "counts": [[grid.get((d, h), 0) for h in range(24)] for d in range(7)],
    }


# --- Putting it together --------------------------------------------------


def measure(repo: Path, tag: str) -> dict:
    """The tree at `tag`: counts, crates and tools, COCOMO."""
    with tempfile.TemporaryDirectory(prefix="trajectory-") as tmp:
        tree = Path(tmp)
        archive = subprocess.run(
            ["git", "archive", tag], cwd=repo, check=True, capture_output=True
        ).stdout
        subprocess.run(["tar", "-x", "-C", str(tree)], input=archive, check=True)
        counts = count_tree(tree)
        counts["crates"] = members(tree, "crates")
        counts["tools"] = members(tree, "tools")
    counts["cocomo"] = cocomo(counts["code"])
    return counts


def ask_at_terminal(c: dict, own: str, majority: str | None) -> str:
    default = majority or own
    why = (
        f"most of that day's commits carry {majority}"
        if majority
        else "that day's commits carry different offsets, none in the majority"
    )
    prompt = (
        f"{c['sha'][:10]} {c['at']:%Y-%m-%d %H:%M} {own} {c['author']}: {c['subject']}\n"
        f"  {why}; offset to read it in [{default}]: "
    )
    while True:
        answer = input(prompt).strip() or default
        if re.fullmatch(r"[+-]\d{4}", answer):
            return answer
        print("  an offset such as +0100 or -0500, please", file=sys.stderr)


def build(args: argparse.Namespace) -> dict:
    repo = Path(args.repo).resolve()
    all_tags = tags(repo)
    if args.until:
        if args.until not in all_tags:
            raise SystemExit(f"error: no tag {args.until}")
        all_tags = all_tags[: all_tags.index(args.until) + 1]
    points = list(all_tags) + (["HEAD"] if args.head else [])
    if not points:
        raise SystemExit("error: no v* tags")
    tip = points[-1]
    authors = args.author or []

    commits = read_commits(repo, authors, tip)
    interactive = sys.stdin.isatty() and not args.no_ask
    recorded = load_answers(args.timezones)
    kept = dict(recorded)

    def ask_and_keep(c: dict, own: str, majority: str | None) -> str:
        # Saved one by one, so an interrupted run keeps what was answered.
        answer = ask_at_terminal(c, own, majority)
        kept[c["sha"]] = answer
        save_answers(args.timezones, kept)
        return answer

    answers, decided = normalize(
        commits, recorded, ask_and_keep if interactive else lambda *_: None
    )
    if kept != recorded:
        print(f"Recorded offsets in {args.timezones}", file=sys.stderr)
    for line in decided:
        print(f"note: {line}", file=sys.stderr)
    times = local_times(commits, answers)

    rows = []
    prev = None
    prev_date = None
    for point in points:
        print(f"Measuring {point}...", file=sys.stderr)
        date = tag_date(repo, point)
        span = git(
            "rev-list", *([f"{prev}..{point}"] if prev else [point]), repo=repo
        ).split()
        in_span = [times[s] for s in span if s in times]
        row = {
            "tag": point,
            "date": date.date().isoformat(),
            "days_since_previous": (
                round((date - prev_date).total_seconds() / 86400, 2) if prev_date else None
            ),
            **history(repo, prev, point),
            **measure(repo, point),
            "commit_times": shares(in_span),
        }
        rows.append(row)
        prev, prev_date = point, date
    out = {
        "generated_from": tip,
        "authors": authors,
        "bands": {name: [start, end] for name, start, end in BANDS},
        "releases": rows,
    }
    if args.private:
        out["heatmap"] = heatmap(list(times.values()))
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--author", action="append", help="author name to count "
                        "commit times for (repeat; default: every author)")
    parser.add_argument("--head", action="store_true",
                        help="add a row for HEAD after the last tag")
    parser.add_argument("--no-ask", action="store_true",
                        help="never ask about stray offsets, even at a terminal")
    parser.add_argument("-o", "--output", default="-",
                        help="where to write the JSON (default: stdout)")
    parser.add_argument("--private", action="store_true",
                        help="add the weekday x hour heatmap of commit times "
                        "(for your own use; not for the public report)")
    parser.add_argument("--repo", default=".", help="the repository (default: .)")
    parser.add_argument("--timezones", type=Path, default=TIMEZONES,
                        help=f"recorded offsets (default: {TIMEZONES})")
    parser.add_argument("--until", help="stop at this tag (default: the newest)")
    args = parser.parse_args(argv)
    data = json.dumps(build(args), indent=2) + "\n"
    if args.output == "-":
        sys.stdout.write(data)
    else:
        Path(args.output).write_text(data)
        print(f"Wrote {args.output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
