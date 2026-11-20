# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Tests for trajectory.py: `python3 -m unittest discover reports/trajectory`."""

import argparse
import datetime as dt
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import trajectory as t


def commit(sha, at, author="Me", subject="s"):
    return {"sha": sha, "author": author, "at": dt.datetime.fromisoformat(at), "subject": subject}


class Cocomo(unittest.TestCase):
    def test_matches_the_cocomo_tool(self):
        # `cocomo` on the tree at v0.28.1 (179,646 lines): $6,291,874.52.
        est = t.cocomo(179646)
        self.assertAlmostEqual(est["cost"], 6291874.52, delta=1)
        self.assertEqual(t.cocomo(0)["cost"], 0)


class TestLines(unittest.TestCase):
    def test_the_test_module_is_counted_from_its_attribute(self):
        text = "\n".join([
            "fn main() {}",
            "",
            "#[cfg(test)]",
            "mod tests {",
            "    // a comment",
            "",
            "    #[test]",
            "    fn works() {}",
            "}",
        ])
        # The attribute, `mod tests {`, `#[test]`, the fn and the brace.
        self.assertEqual(t.test_code_lines(text), 5)
        # A cfg(test) item that is not a module is not the test module.
        self.assertEqual(t.test_code_lines("#[cfg(test)]\nuse x;\n"), 0)
        # The last test module counts, on one line or needing test with
        # more; block comments do not.
        text = "\n".join([
            "#[cfg(test)]",
            "mod helpers {}",
            "fn production() {}",
            "#[cfg(all(test, unix))] mod tests {",
            "    /* a",
            "       block */",
            "    fn t() {}",
            "}",
        ])
        self.assertEqual(t.test_code_lines(text), 3)
        self.assertTrue(t.is_test_file("crates/x/tests/it.rs"))
        self.assertFalse(t.is_test_file("crates/x/src/tests.rs"))


class CommitTimes(unittest.TestCase):
    def test_a_stray_offset_is_read_in_the_days_majority(self):
        commits = [
            commit("a", "2026-03-06T10:00:00+00:00"),
            commit("b", "2026-03-06T12:00:00+00:00"),
            # A web commit carrying an old zone: 11:19 UTC.
            commit("c", "2026-03-06T05:19:00-06:00", subject="web"),
            # Another author's day is theirs.
            commit("d", "2026-03-06T05:00:00-06:00", author="Other"),
        ]
        self.assertEqual([(c["sha"], m) for c, m in t.strays(commits)], [("c", "+0000")])
        answers, decided = t.normalize(commits, {}, lambda *_: None)
        self.assertEqual(answers, {"c": "+0000"})
        self.assertIn("read in +0000", decided[0])
        times = t.local_times(commits, answers)
        self.assertEqual(times["c"].hour, 11)
        # A recorded answer is used, and not asked again.
        asked = []
        answers, decided = t.normalize(commits, {"c": "-0600"}, lambda *a: asked.append(a))
        self.assertEqual((answers["c"], asked, decided), ("-0600", [], []))
        # An answer given is kept.
        answers, _ = t.normalize(commits, {}, lambda *_: "+0100")
        self.assertEqual(answers, {"c": "+0100"})

    def test_a_tie_or_a_clock_change_is_not_a_stray_by_majority(self):
        # One commit each in two zones far apart: neither is the day's.
        tie = [
            commit("a", "2026-03-06T10:00:00+01:00"),
            commit("b", "2026-03-06T08:00:00-05:00"),
        ]
        self.assertEqual(sorted((c["sha"], m) for c, m in t.strays(tie)), [("a", None), ("b", None)])
        answers, decided = t.normalize(tie, {}, lambda *_: None)
        self.assertEqual(answers, {})
        self.assertIn("no offset has a majority", decided[0])
        # Ireland falls back on 2026-10-25: +0100 in the morning, then +0000.
        change = [
            commit("a", "2026-10-25T09:00:00+01:00"),
            commit("b", "2026-10-25T15:00:00+00:00"),
            commit("c", "2026-10-25T20:00:00+00:00"),
        ]
        self.assertEqual(t.strays(change), [])
        # A single switch two hours apart is no clock change.
        self.assertFalse(t.clock_change([
            commit("a", "2026-10-25T09:00:00+02:00"),
            commit("b", "2026-10-25T15:00:00+00:00"),
        ]))

    def test_shares_and_heatmap(self):
        times = [
            dt.datetime(2026, 11, 14, 22, 0),  # Saturday evening
            dt.datetime(2026, 11, 16, 3, 0),  # Monday night
            dt.datetime(2026, 11, 16, 10, 0),  # Monday daytime
        ]
        self.assertEqual(
            t.shares(times),
            {"commits": 3, "weekend": 1, "night": 1, "daytime": 1, "evening": 1},
        )
        grid = t.heatmap(times)["counts"]
        self.assertEqual((grid[5][22], grid[0][3], grid[0][10]), (1, 1, 1))

    def test_answers_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tz.toml"
            self.assertEqual(t.load_answers(path), {})
            t.save_answers(path, {"b": "+0000", "a": "-0500"})
            self.assertEqual(t.load_answers(path), {"a": "-0500", "b": "+0000"})


class Repository(unittest.TestCase):
    """A small repository with two releases and a stray tag."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        def run(*args, at="2026-01-01T10:00:00+00:00"):
            env = {"GIT_AUTHOR_DATE": at, "GIT_COMMITTER_DATE": at, "HOME": self.tmp.name,
                   "PATH": "/usr/bin:/bin"}
            subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True, env=env)
        run("init", "-q", "-b", "main")
        run("config", "user.name", "Me")
        run("config", "user.email", "me@example.org")
        (self.repo / "lib.rs").write_text("fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n")
        run("add", ".")
        run("commit", "-q", "-m", "one")
        run("tag", "v0.1.0")
        (self.repo / "more.rs").write_text("fn b() {}\nfn c() {}\n")
        run("add", ".")
        run("commit", "-q", "-m", "two", at="2026-01-03T22:00:00+00:00")
        run("tag", "v0.2.0")
        run("tag", "v1.0")  # not vX.Y.Z

    def tearDown(self):
        self.tmp.cleanup()

    def args(self, **over):
        base = dict(repo=str(self.repo), until=None, head=False, author=None, no_ask=True,
                    timezones=self.repo / "tz.toml", private=False)
        base.update(over)
        return argparse.Namespace(**base)

    def test_the_history_needs_no_line_counter_or_author(self):
        # As the release job runs it: no --author, no user.name either.
        counted = {"languages": {}, "code": 10, "rust_code": 6, "rust_tests": 4}
        with mock.patch.object(t, "count_tree", return_value=counted):
            with mock.patch.dict("os.environ", {"GIT_CONFIG_GLOBAL": "/dev/null"}):
                out = t.build(self.args(until="v0.1.0", head=True))
        self.assertEqual([r["tag"] for r in out["releases"]], ["v0.1.0", "HEAD"])
        self.assertEqual(out["authors"], [])
        self.assertEqual(out["releases"][1]["commit_times"]["commits"], 1)
        self.assertNotIn("heatmap", out)

    @unittest.skipUnless(shutil.which("tokei"), "needs tokei")
    def test_releases_are_measured_and_compared(self):
        self.assertEqual(t.tags(self.repo), ["v0.1.0", "v0.2.0"])
        out = t.build(self.args(author=["Me"], private=True))
        first, second = out["releases"]
        self.assertEqual((first["rust_code"], first["rust_tests"]), (1, 4))
        self.assertEqual((second["commits"], second["lines_added"]), (1, 2))
        self.assertEqual(second["days_since_previous"], 2.5)
        self.assertEqual(second["commit_times"]["evening"], 1)
        self.assertEqual(sum(map(sum, out["heatmap"]["counts"])), 2)


if __name__ == "__main__":
    unittest.main()
