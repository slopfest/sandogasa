# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Tests for check_guidelines.py: `python3 -m unittest discover scripts/guidelines`."""

import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path

import check_guidelines as g


def doc(*lines):
    return "\n".join(lines) + "\n"


class Check(unittest.TestCase):
    def test_a_rule_with_a_bold_lead_and_background_passes(self):
        text = doc(
            "# Guidelines",
            "",
            "## Testing",
            "",
            "- **Tests must not reach the external network.** Use a",
            "  wiremock server on loopback.",
            "  - Background: dbranch 0.15.0 failed %check on 2026-11-02.",
        )
        self.assertEqual(g.check(text), [])

    def test_a_rule_without_a_bold_lead_is_reported(self):
        text = doc("## Testing", "- Always write tests.")
        self.assertEqual(g.check(text), [(2, "a rule that does not open with its bold lead")])

    def test_sub_bullets_and_text_before_the_first_section_need_no_lead(self):
        text = doc(
            "Intro list:",
            "- plain item",
            "## Testing",
            "- **Rule.**",
            "  - plain step",
        )
        self.assertEqual(g.check(text), [])

    def test_a_long_lead_and_a_long_rule_are_reported(self):
        lead = " ".join(["word"] * (g.MAX_LEAD_WORDS + 1))
        rest = " ".join(["more"] * g.MAX_BULLET_WORDS)
        text = doc("## S", f"- **{lead}** {rest}")
        found = [m for _, m in g.check(text)]
        self.assertEqual(found, [
            f"a bold lead of {g.MAX_LEAD_WORDS + 1} words (at most {g.MAX_LEAD_WORDS})",
            f"a rule of {g.MAX_LEAD_WORDS + 1 + g.MAX_BULLET_WORDS} words (at most {g.MAX_BULLET_WORDS})",
        ])

    def test_the_word_count_leaves_out_sub_bullets(self):
        steps = [f"  - step {' '.join(['x'] * 50)}" for _ in range(3)]
        text = doc("## S", "- **Rule.** Short.", *steps)
        self.assertEqual(g.check(text), [])

    def test_continuation_lines_count_toward_the_rule(self):
        line = " ".join(["x"] * 40)
        text = doc("## S", "- **Rule.**", f"  {line}", f"  {line}")
        self.assertEqual(g.check(text), [(2, "a rule of 81 words (at most 80)")])

    def test_a_date_belongs_in_a_background_bullet(self):
        text = doc(
            "## S",
            "- **Rule.** Since 2026-11-24.",
            "  - **Background:** first seen 2026-11-20.",
            "  - Background, 2026-11-10: slow.",
            "  - a step dated 2026-11-01",
        )
        self.assertEqual(g.check(text), [
            (2, "a date outside a Background: bullet"),
            (5, "a date outside a Background: bullet"),
        ])

    def test_fenced_code_is_skipped(self):
        text = doc("## S", "```", "- not a rule 2026-11-01", "```", "- **Rule.**")
        self.assertEqual(g.check(text), [])

    def test_a_heading_or_blank_line_ends_a_bullet(self):
        text = doc("## S", "- **Rule.**", "", "  indented paragraph 2026-11-01", "### Sub", "- **Next.**")
        self.assertEqual(g.check(text), [])


class Main(unittest.TestCase):
    def run_main(self, text):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "CLAUDE.md"
            path.write_text(text)
            out = StringIO()
            with redirect_stdout(out):
                status = g.main([str(path)])
            return status, out.getvalue().replace(str(path), "F")

    def test_findings_are_printed_with_their_line_and_fail(self):
        status, out = self.run_main(doc("## S", "- plain"))
        self.assertEqual(status, 1)
        self.assertEqual(out, "F:2: a rule that does not open with its bold lead\n")

    def test_a_clean_file_passes_quietly(self):
        self.assertEqual(self.run_main(doc("## S", "- **Rule.**")), (0, ""))

    def test_no_file_is_a_usage_error(self):
        self.assertEqual(g.main([]), 2)


if __name__ == "__main__":
    unittest.main()
