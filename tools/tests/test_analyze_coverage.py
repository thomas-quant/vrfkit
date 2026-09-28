"""Guards for the coverage analysis: never fatal on a measured miss, never an
unmeasured zero (see `missed_report`), and an argv parsed in full.
"""
import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import analyze_coverage as guard  # noqa: E402


class MissedReportTests(unittest.TestCase):
    def test_a_measured_zero_is_reported_as_a_zero(self):
        line = guard.missed_report(0, measured=True)
        self.assertIn("0", line)
        self.assertNotIn("NOT MEASURED", line)

    def test_a_measured_count_is_reported(self):
        self.assertIn("7", guard.missed_report(7, measured=True))

    def test_an_unmeasured_run_does_not_report_a_zero(self):
        line = guard.missed_report(0, measured=False)
        self.assertIn("NOT MEASURED", line)
        self.assertNotIn(": 0", line)


class ClassifyTests(unittest.TestCase):
    def test_a_group_in_the_overlay_is_covered(self):
        counts, _, _ = guard.classify(
            [{"path": "/Script/A", "fields": [1, 2]}], {"/Script/A"}, set())
        self.assertEqual(counts["covered"], 1)

    def test_a_group_with_a_descriptor_and_no_overlay_entry_was_missed(self):
        counts, missed, _ = guard.classify(
            [{"path": "/Script/A", "fields": [1, 2]}], set(), {"/Script/A"})
        self.assertEqual(counts["extractor_missed"], 1)
        self.assertEqual(missed, [("/Script/A", 2)])

    def test_a_group_nobody_describes_is_raw_only(self):
        counts, _, no_desc = guard.classify(
            [{"path": "/Script/A", "fields": [1]}], set(), set())
        self.assertEqual(counts["no_descriptor"], 1)
        self.assertEqual(no_desc, [("/Script/A", 1)])


class CommandLineTests(unittest.TestCase):
    """`--csharp-dir=PATH`, a valueless flag, `--help` and typos are parsed or
    rejected, never dropped in favour of the vendored descriptors."""

    def run_main(self, *args: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = guard.main(["analyze_coverage.py", *args])
            except SystemExit as exit_:
                code = exit_.code
        return code, out.getvalue(), err.getvalue()

    def test_an_unknown_or_valueless_flag_is_rejected(self):
        for args in (["--csharp-dri", "x"], ["--csharp-dir"]):
            with self.subTest(args=args):
                code, _, err = self.run_main(*args)
                self.assertEqual(code, 2)
                self.assertIn("usage:", err)

    def test_help_prints_usage(self):
        code, out, _ = self.run_main("--help")
        self.assertEqual(code, 0)
        self.assertIn("--csharp-dir", out)

    def test_the_equals_form_selects_the_descriptor_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"net_field_export_groups": [
                {"path": "/Script/A", "fields": [1]}]}), encoding="utf-8")
            descriptors = root / "descriptors"
            descriptors.mkdir()
            (descriptors / "A.cs").write_text(
                'public override string Path => "/Script/A";', encoding="utf-8")
            with mock.patch.object(guard, "MANIFEST_PATH", manifest):
                code, out, _ = self.run_main(f"--csharp-dir={descriptors}")
        self.assertEqual(code, 0)
        self.assertIn("C# descriptor paths: 1\n", out)


if __name__ == "__main__":
    unittest.main()
