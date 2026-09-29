"""Guards for the doc guard: nothing else catches a wrong number in prose, so
its detection logic must not rot into something that passes everything."""
import ast
import contextlib
import inspect
import io
import json
import re
import sys
import textwrap
import unittest
from unittest.mock import patch
from subprocess import CompletedProcess
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_docs as guard  # noqa: E402


USAGE_MENTIONING_EVERYTHING = " ".join(
    p.name for p in (guard.REPO / "tools").glob("*.py")
)


class ToolCoverageTests(unittest.TestCase):
    def test_an_undocumented_tool_is_reported(self):
        problems = guard.check_tools("only_one.py is documented")
        self.assertTrue(problems)
        self.assertTrue(any("never mentions it" in p for p in problems))

    def test_documenting_a_tool_that_does_not_exist_is_reported(self):
        text = USAGE_MENTIONING_EVERYTHING + " imaginary_tool.py"
        problems = guard.check_tools(text)
        self.assertEqual(
            [p for p in problems if "imaginary_tool" in p and "does not exist" in p],
            [p for p in problems if "imaginary_tool" in p],
        )
        self.assertTrue(any("imaginary_tool.py" in p for p in problems))

    def test_documenting_every_tool_is_clean(self):
        self.assertEqual(guard.check_tools(USAGE_MENTIONING_EVERYTHING), [])

    def test_the_external_script_is_not_demanded_to_exist(self):
        """compute_metrics.py lives in valplay, which is never modified here."""
        text = USAGE_MENTIONING_EVERYTHING + " compute_metrics.py"
        self.assertEqual(
            [p for p in guard.check_tools(text) if "compute_metrics" in p], []
        )

    def test_test_files_are_not_demanded_in_the_reference(self):
        problems = guard.check_tools(USAGE_MENTIONING_EVERYTHING + " test_thing.py")
        self.assertFalse(any("test_thing" in p for p in problems))


class CrateCoverageTests(unittest.TestCase):
    def test_a_missing_crate_row_is_reported(self):
        problems = guard.check_crates("`vrf-bitio` only")
        self.assertTrue(problems)
        self.assertTrue(any("vrfkit" in p for p in problems))

    def test_the_real_usage_doc_covers_every_crate(self):
        usage = guard.read(guard.USAGE)
        self.assertEqual(guard.check_crates(usage), [])


class LinkTests(unittest.TestCase):
    def test_a_dead_relative_link_is_reported(self):
        problems = guard.check_links(guard.USAGE, "[x](no_such_file.md)")
        self.assertTrue(any("no_such_file.md" in p for p in problems))

    def test_external_and_anchor_links_are_skipped(self):
        text = "[a](https://example.com) [b](#section)"
        self.assertEqual(guard.check_links(guard.USAGE, text), [])

    def test_a_link_with_an_anchor_checks_only_the_file(self):
        text = "[a](../README.md#어쩌고)"
        self.assertEqual(guard.check_links(guard.USAGE, text), [])

    def test_every_top_level_doc_is_link_checked(self):
        names = {p.name for p in guard.link_checked_docs()}
        on_disk = {p.name for p in (guard.REPO / "docs").glob("*.md")}
        self.assertEqual(names & on_disk, on_disk)
        for path in guard.link_checked_docs():
            self.assertEqual(guard.check_links(path, guard.read(path)), [], path.name)


class AnchorTests(unittest.TestCase):
    """`check_links` checks only that a linked file exists, so a heading
    renamed under a link, or a slug guessed wrong, went unreported:
    USAGE.md's table of contents linked `#downstream-conversion` for a heading
    whose anchor is `#downstream-conversion-tools`."""

    TARGET = guard.REPO / "docs" / "TARGET.md"
    SOURCE = guard.REPO / "docs" / "SOURCE.md"

    def lookup(self, text):
        anchors = frozenset(guard.heading_anchors(text))
        return lambda path: anchors if path == self.TARGET.resolve() else None

    def test_slugs_follow_githubs_rules(self):
        for heading, slug in (
                ("Regression guards -- after non-trivial changes",
                 "regression-guards----after-non-trivial-changes"),
                ("`fields.parquet`", "fieldsparquet"),
                ("Notes on F# and C++ readers",
                 "notes-on-f-and-c-readers"),
                ("`raw_bits`: SmallVec, and the rejected arena",
                 "raw_bits-smallvec-and-the-rejected-arena"),
                ("Generated files — never hand-edit", "generated-files--never-hand-edit"),
                ("5. `tools/` reference", "5-tools-reference"),
                ("See [the table](other.md#x) here", "see-the-table-here"),
                ("참조 저장소 및 PR 조사", "참조-저장소-및-pr-조사")):
            with self.subTest(heading=heading):
                self.assertEqual(guard.github_slug(heading), slug)

    def test_a_repeated_heading_gets_a_numbered_anchor(self):
        self.assertEqual(guard.heading_anchors("# A\n## A\n### A b\n#### A"),
                         {"a", "a-1", "a-b", "a-2"})

    def test_a_hash_line_in_fenced_code_is_not_a_heading(self):
        text = ("```bash\n# not a heading\n```python\n# still code\n```\n"
                "~~~\n## nor this\n~~~\n## Real ##\n#hashtag\n")
        self.assertEqual(guard.heading_anchors(text), {"real"})

    def test_a_broken_same_file_anchor_is_reported(self):
        text = "## Downstream conversion tools\n\n[x](#downstream-conversion)\n"
        problems = guard.broken_markdown_anchors(
            self.SOURCE, text, lambda path: frozenset(guard.heading_anchors(text)))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("SOURCE.md:3", problems[0])
        self.assertIn("#downstream-conversion", problems[0])
        fixed = text.replace("(#downstream-conversion)", "(#downstream-conversion-tools)")
        self.assertEqual(guard.broken_markdown_anchors(
            self.SOURCE, fixed, lambda path: frozenset(guard.heading_anchors(fixed))), [])

    def test_a_broken_cross_file_anchor_is_reported(self):
        lookup = self.lookup("# Title\n## Name interning\n")
        good = "[a](TARGET.md#name-interning)"
        bad = "[a](TARGET.md#name-intern)"
        self.assertEqual(guard.broken_markdown_anchors(self.SOURCE, good, lookup), [])
        problems = guard.broken_markdown_anchors(self.SOURCE, bad, lookup)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("#name-intern", problems[0])

    def test_a_percent_encoded_anchor_is_decoded(self):
        lookup = self.lookup("## 어쩌고\n")
        for link in ("[a](TARGET.md#%EC%96%B4%EC%A9%8C%EA%B3%A0)", "[a](TARGET.md#어쩌고)"):
            with self.subTest(link=link):
                self.assertEqual(guard.broken_markdown_anchors(self.SOURCE, link, lookup), [])

    def test_links_in_fenced_code_and_line_anchors_are_not_heading_links(self):
        lookup = self.lookup("## Only\n")
        text = ("```\n[a](TARGET.md#nothing)\n```\n"
                "[b](../tools/check_docs.py#L10)\n[c](https://example.com/x.md#y)\n")
        checked = []
        self.assertEqual(
            guard.broken_markdown_anchors(self.SOURCE, text, lookup, checked), [])
        self.assertEqual(checked, [])

    def test_a_code_reference_to_a_missing_heading_or_doc_is_reported(self):
        # Two literals, so the repository scan does not read this file as
        # citing the headings it tests.
        lookup = self.lookup("## Name interning\n")
        target = "docs/TARGET" ".md"
        text = (f"//! {target}#name-interning.\n"
                f"// ({target}#name-intern)\n"
                f"# see docs/GONE" ".md#anything\n")
        checked = []
        problems = guard.broken_code_anchors("x.rs", text, lookup, checked)
        self.assertEqual(len(checked), 3, checked)
        self.assertEqual(len(problems), 2, problems)
        self.assertIn("x.rs:2", problems[0])
        self.assertIn("#name-intern", problems[0])
        self.assertIn("does not exist", problems[1])

    def test_the_archive_is_a_target_but_not_a_source(self):
        sources = {p.relative_to(guard.REPO).as_posix() for p in guard.link_checked_docs()}
        self.assertFalse(any(name.startswith("docs/archive/") for name in sources))
        archived = guard.REPO / "docs" / "archive" / "PROJECT_STATUS.md"
        slug = sorted(guard.anchors_of(archived.resolve()))[0]
        text = f"[a](archive/PROJECT_STATUS.md#{slug}) [b](archive/PROJECT_STATUS.md#no-{slug})"
        problems = guard.broken_markdown_anchors(guard.REPO / "docs" / "X.md", text)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(f"#no-{slug}", problems[0])

    def test_the_shipped_docs_and_sources_have_no_broken_anchor(self):
        checked = {}
        self.assertEqual(guard.anchor_problems(checked), [])
        # A guard that read nothing would pass the line above.
        self.assertGreater(len(checked["docs"]), 0)
        self.assertGreater(len(checked["code"]), 0)

    def test_an_unlistable_source_tree_is_reported_not_passed(self):
        import subprocess as sp
        real = guard.subprocess.run

        def failing(cmd, *a, **kw):
            if cmd[:2] == ["git", "-C"]:
                return sp.CompletedProcess(cmd, 128, stdout="", stderr="fatal")
            return real(cmd, *a, **kw)

        with patch.object(guard.subprocess, "run", side_effect=failing):
            problems = guard.anchor_problems()
        self.assertTrue(any("went unchecked" in p for p in problems), problems)


class FeatureMatrixTests(unittest.TestCase):
    CONTRIBUTING = (
        "cargo +1.86.0 check -p vrf-a --no-default-features --locked\n"
        "cargo +1.86.0 check -p vrf-a --no-default-features --features x --locked\n"
    )
    CI = '          $matrix = @(\n            @("vrf-a", ""), @("vrf-a", "x")\n          )\n'

    def test_the_shipped_matrices_agree(self):
        contributing = guard.read(guard.REPO / "CONTRIBUTING.md")
        ci = guard.read(guard.REPO / ".github" / "workflows" / "ci.yml")
        self.assertEqual(guard.check_feature_matrix(contributing, ci), [])

    def test_identical_lists_pass(self):
        self.assertEqual(guard.check_feature_matrix(self.CONTRIBUTING, self.CI), [])

    def test_a_reordered_matrix_is_reported(self):
        ci = self.CI.replace('@("vrf-a", ""), @("vrf-a", "x")', '@("vrf-a", "x"), @("vrf-a", "")')
        problems = guard.check_feature_matrix(self.CONTRIBUTING, ci)
        self.assertTrue(any("order" in p for p in problems), problems)

    def test_a_case_missing_from_ci_is_reported(self):
        ci = self.CI.replace(', @("vrf-a", "x")', "")
        problems = guard.check_feature_matrix(self.CONTRIBUTING, ci)
        self.assertTrue(any("vrf-a" in p and "x" in p for p in problems), problems)

    def test_an_unparseable_ci_matrix_is_reported_not_passed(self):
        problems = guard.check_feature_matrix(self.CONTRIBUTING, "no matrix here")
        self.assertTrue(problems)

    def test_an_empty_contributing_matrix_is_reported_not_passed(self):
        problems = guard.check_feature_matrix("no cargo lines", self.CI)
        self.assertTrue(problems)


class TableSizeTests(unittest.TestCase):
    def test_a_stale_table_size_is_reported(self):
        docs = {"README.md": "the table has 999 entries",
                "USAGE.md": "the table has 999 entries"}
        problems = guard.check_table_sizes(docs)
        self.assertTrue(problems)

    def test_the_shipped_docs_quote_the_live_sizes(self):
        docs = {"README.md": guard.read(guard.README),
                "USAGE.md": guard.read(guard.USAGE)}
        self.assertEqual(guard.check_table_sizes(docs), [])

    def test_both_comma_and_plain_forms_are_accepted(self):
        table = guard.read(guard.REPO / "crates" / "vrf-decode" / "src" / "table.rs")
        import re
        n = re.search(r"OVERLAY_TABLE: \[OverlayEntry; (\d+)\]", table).group(1)
        h = re.search(r"OVERLAY_HANDLE_TABLE: \[OverlayHandleEntry; (\d+)\]",
                      table).group(1)
        plain = {"README.md": n, "USAGE.md": f"{n} {h}"}
        comma = {"README.md": f"{int(n):,}", "USAGE.md": f"{int(n):,} {h}"}
        self.assertEqual(guard.check_table_sizes(plain), [])
        self.assertEqual(guard.check_table_sizes(comma), [])


class SourceTableSizeTests(unittest.TestCase):
    """Rust prose and Cargo.toml quote the table size too."""

    LIVE = {"1188", "1,188"}

    def test_a_stale_size_is_reported_with_its_line(self):
        text = "one\n// the 1,185-entry generated table\nthree"
        self.assertEqual(guard.stale_entry_phrases(text, self.LIVE), [(2, "1,185")])

    def test_both_comma_and_plain_forms_are_accepted(self):
        for spelling in ("1,188-entry table", "1188-entry table"):
            self.assertEqual(guard.stale_entry_phrases(spelling, self.LIVE), [])

    def test_the_optional_generated_word_is_matched_either_way(self):
        """`N-entry table` and `N-entry generated table` are both in the tree."""
        for phrase in ("999-entry table", "999-entry generated table"):
            self.assertEqual(guard.stale_entry_phrases(phrase, self.LIVE),
                             [(1, "999")], phrase)

    def test_a_bare_entry_count_is_not_a_size_claim(self):
        """Dated measurements legitimately say "1,054 entries" about a table
        that no longer exists; only "N-entry table" claims the live size."""
        self.assertEqual(
            guard.stale_entry_phrases("measured over 1,054 entries", self.LIVE), [])

    def test_the_shipped_crates_quote_the_live_size(self):
        self.assertEqual(guard.check_source_table_size(), [])


class SuiteMeasurementTests(unittest.TestCase):
    RUST = "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
    PYTHON = "Ran 4 tests in 0.01s\n\nOK\n"

    def measure(self, rust=None, python=None, rust_exit=0, python_exit=0):
        with patch.object(guard.subprocess, "run", side_effect=[
            CompletedProcess([], rust_exit, stdout=self.RUST if rust is None else rust, stderr=""),
            CompletedProcess([], python_exit, stdout="", stderr=self.PYTHON if python is None else python),
        ]) as run:
            result = guard.measure_tests()
        return result, run

    def test_measures_both_suites_and_promotes_python_warnings(self):
        result, run = self.measure(rust=self.RUST * 2)
        self.assertEqual(result, (6, 4, []))
        self.assertEqual(run.call_args_list[1].args[0][1:3], ["-W", "error"])

    def test_empty_output_and_zero_tests_are_not_successful_measurements(self):
        for label, output in (("rust", ""), ("python", ""),
                              ("rust", self.RUST.replace("3 passed", "0 passed")),
                              ("python", self.PYTHON.replace("4 tests", "0 tests"))):
            with self.subTest(label=label, output=output):
                self.assertTrue(self.measure(**{label: output})[0][2])

    def test_passing_text_cannot_hide_a_failed_process(self):
        for kwargs in ({"rust_exit": 1}, {"python_exit": 1}):
            with self.subTest(kwargs=kwargs):
                self.assertTrue(self.measure(**kwargs)[0][2])

    def test_skipped_python_tests_are_not_reported_as_all_passing(self):
        self.assertTrue(self.measure(python=self.PYTHON.replace("OK", "OK (skipped=1)"))[0][2])

    def test_unrelated_passing_text_is_not_a_rust_suite_summary(self):
        self.assertTrue(self.measure(rust="an example says 999 passed\n")[0][2])


class TestCountTests(unittest.TestCase):
    """A doc may not carry a stale suite size beside a live one."""

    LIVE = {"387", "120"}

    def test_a_stale_count_is_reported_with_its_line(self):
        text = "one\ncargo test --workspace **355 passing**\nthree"
        self.assertEqual(guard.stale_test_counts(text, self.LIVE), [(2, "355")])

    def test_a_live_count_elsewhere_does_not_excuse_a_stale_one(self):
        text = "- **387 tests** plus a validation suite\nverified: **355 passing**"
        self.assertEqual(guard.stale_test_counts(text, self.LIVE), [(2, "355")])

    def test_either_suite_count_is_accepted(self):
        self.assertEqual(guard.stale_test_counts("387 passing\n120 tests", self.LIVE), [])

    def test_a_number_that_is_not_a_count_claim_is_ignored(self):
        """Only "N tests" / "N passing" is a claim about the suites; README's
        "2,387 intermediate moves" is not."""
        self.assertEqual(
            guard.stale_test_counts("we recover 2,387 intermediate moves", self.LIVE), [])

    def test_a_count_that_names_its_suite_is_read(self):
        """README's highlight puts the suite between the number and the noun;
        it went stale twice while only "N tests" was read."""
        text = "- **355 Rust tests** plus a layered validation suite"
        self.assertEqual(guard.stale_test_counts(text, self.LIVE), [(1, "355")])
        self.assertEqual(guard.stale_test_counts("**387 Rust tests**", self.LIVE), [])

    def test_a_count_that_names_its_suite_must_be_that_suites(self):
        by_suite = {"Rust": {"387"}, "Python": {"120"}}
        self.assertEqual(guard.stale_test_counts(
            "**120 Rust tests**\n**387 Python tests**\n387 passing\n120 tests",
            self.LIVE, by_suite), [(1, "120"), (2, "387")])

    def test_the_shipped_readme_highlight_is_read(self):
        claims = [suite for line in guard.read(guard.README).splitlines()
                  for _count, suite in guard.TEST_COUNT_RE.findall(line)]
        self.assertIn("Rust", claims)


class TableSizeClaimTests(unittest.TestCase):
    """Every number that claims to BE a table size must be the live one, even
    one line from the correct one."""

    LENGTHS = ("1255", "84")

    def test_a_stale_entry_count_beside_the_live_one_is_caught(self):
        docs = {"README.md": "the overlay table (1,255 entries, 84 handles)\n"
                             "elsewhere: 1,185 entries"}
        problems = guard.stale_table_size_claims(docs, self.LENGTHS)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("1,185", problems[0])
        self.assertIn("README.md:2", problems[0])

    def test_a_stale_handle_count_is_caught(self):
        problems = guard.stale_table_size_claims(
            {"USAGE.md": "overlay table 1,255 + 63 handles"}, self.LENGTHS)
        self.assertTrue(any("63" in p for p in problems), problems)

    def test_both_spellings_of_the_live_numbers_pass(self):
        docs = {"a.md": "1,255 entries and 84 handles",
                "b.md": "1255 entries and 84 handles"}
        self.assertEqual(guard.stale_table_size_claims(docs, self.LENGTHS), [])

    def test_punctuation_before_word_handles_is_not_a_size_claim(self):
        docs = {"USAGE.md": "identifiers, handles, checksums, and payloads"}
        self.assertEqual(guard.stale_table_size_claims(docs, self.LENGTHS), [])

    def test_the_shipped_docs_make_no_stale_size_claim(self):
        lengths = guard.table_lengths()
        docs = {name: guard.read(guard.REPO / name) for name in guard.ALL_DOCS}
        self.assertEqual(guard.stale_table_size_claims(docs, lengths), [])


class MeasurementFailureTests(unittest.TestCase):
    """A measurement that could not be taken (git failing) is reported, not
    left to skip every claim that depends on it."""

    def test_a_working_measurement_reports_no_problem(self):
        problems = []
        counts = guard.measured_counts(problems)
        self.assertIn("ascii", counts)
        self.assertEqual(problems, [])

    def test_a_failed_enumeration_is_reported_rather_than_skipped(self):
        import subprocess as sp
        real = guard.subprocess.run

        def failing(cmd, *a, **kw):
            if cmd[:2] == ["git", "-C"]:
                return sp.CompletedProcess(cmd, 128, stdout="", stderr="fatal")
            return real(cmd, *a, **kw)

        problems = []
        guard.subprocess.run = failing
        try:
            counts = guard.measured_counts(problems)
        finally:
            guard.subprocess.run = real
        self.assertNotIn("ascii", counts)
        self.assertTrue(problems)
        self.assertIn("ascii", " ".join(problems))


class ContradictingCountTests(unittest.TestCase):
    """The half of the count check that survives `--fast`: with exactly two
    suites, a third distinct value is a contradiction."""

    def test_a_third_distinct_count_is_reported(self):
        docs = {"README.md": "- **394 tests**\nverified: **355 passing**",
                "USAGE.md": "# 133 passing"}
        problems = guard.contradicting_test_counts(docs)
        self.assertTrue(problems)
        self.assertIn("355", " ".join(problems))

    def test_the_two_live_suites_are_not_a_contradiction(self):
        docs = {"README.md": "394 tests and the Python suite has 133 tests",
                "USAGE.md": "394 passing\n133 passing"}
        self.assertEqual(guard.contradicting_test_counts(docs), [])

    def test_one_count_everywhere_is_not_a_contradiction(self):
        docs = {"README.md": "394 tests", "USAGE.md": "394 passing"}
        self.assertEqual(guard.contradicting_test_counts(docs), [])

    def test_a_stale_highlight_is_a_third_count(self):
        docs = {"README.md": "- **390 Rust tests** plus\n394 passing",
                "USAGE.md": "# 133 passing"}
        self.assertIn("390", " ".join(guard.contradicting_test_counts(docs)))

    def test_the_report_names_every_site_so_the_stale_one_can_be_found(self):
        docs = {"README.md": "394 tests\n355 passing", "USAGE.md": "133 passing"}
        joined = " ".join(guard.contradicting_test_counts(docs))
        for expected in ("README.md:1", "README.md:2", "USAGE.md:1"):
            self.assertIn(expected, joined)

    def test_the_shipped_docs_do_not_contradict_themselves(self):
        docs = {"README.md": guard.read(guard.README),
                "USAGE.md": guard.read(guard.USAGE)}
        self.assertEqual(guard.contradicting_test_counts(docs), [])


class MeasuredCountTests(unittest.TestCase):
    """Counts produced by something runnable, quoted in prose that rots."""

    def test_a_stale_ascii_count_is_caught(self):
        problems = guard.stale_measured_counts(
            {"x.md": "`check_ascii` on 999 files."}, {"ascii": 115})
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("999", problems[0])

    def test_the_live_ascii_count_passes(self):
        text = "`check_ascii` on 115 files, and 115 files, ASCII only"
        self.assertEqual(
            guard.stale_measured_counts({"x.md": text}, {"ascii": 115}), [])

    def test_three_different_correction_counts_are_all_caught(self):
        text = "(85 corrections)\nsays 86 corrections\n# 49 corrections present"
        problems = guard.stale_measured_counts({"x.md": text}, {"corrections": 85})
        self.assertEqual(len(problems), 2, problems)

    def test_a_stale_golden_vector_count_is_caught(self):
        text = "66 mechanically extracted golden vectors (11 staging"
        problems = guard.stale_measured_counts({"x.md": text}, {"golden": 77})
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("66", problems[0])

    def test_both_stale_matrix_counts_are_caught(self):
        text = "as 25 `cargo check`\nlines; ci.yml expresses **the same 25 cases** as"
        problems = guard.stale_measured_counts(
            {"x.md": text}, {"matrix": 27, "matrix_cases": 27})
        self.assertEqual(len(problems), 2, problems)

    def test_a_metrics_build_count_counts_only_beside_its_guard(self):
        live = {"metrics_builds": 7}
        unrelated = "The transform is shared across 7 builds (5 builds before)."
        self.assertEqual(guard.stale_measured_counts({"x.md": unrelated}, live), [])
        stale = "| `check_metrics_baseline.py` | rounds, score, K/D/A (5 builds) |"
        self.assertEqual(len(guard.stale_measured_counts({"x.md": stale}, live)), 1)

    def test_the_live_golden_and_matrix_counts_are_measured(self):
        live = guard.measured_counts()
        self.assertEqual(live["golden"], 88)
        self.assertEqual(live["matrix"], live["matrix_cases"])
        self.assertGreater(live["matrix"], 0)
        self.assertGreater(live["metrics_builds"], 0)

    def test_a_corpus_file_count_is_not_an_ascii_claim(self):
        """README says "all 215 files" about replays, not about the sweep."""
        text = "- **Framing** (`validate_corpus.py`, all 215 files) -- framing."
        self.assertEqual(
            guard.stale_measured_counts({"x.md": text}, {"ascii": 115}), [])

    def test_the_repository_is_clean(self):
        live = guard.measured_counts()
        docs = {name: guard.read(guard.REPO / name) for name in guard.ALL_DOCS}
        self.assertEqual(guard.stale_measured_counts(docs, live), [])

    def test_live_correction_count_includes_dynamic_weapon_entries(self):
        # expectation_count() includes the generated table's dynamic weapon
        # entries. Pinned on purpose: a verified typing change must change this
        # number visibly.
        self.assertEqual(guard.measured_counts()["corrections"], 219)


class GeneratedInventoryTests(unittest.TestCase):
    def test_a_missing_checksum_table_is_reported(self):
        docs = {
            name: " ".join(
                value
                for target, generator in guard.GENERATED_INVENTORY.items()
                if not target.endswith("checksum_table.rs")
                for value in (target, generator)
            )
            for name in guard.GENERATED_INVENTORY_DOCS
        }
        problems = guard.check_generated_inventory(docs)
        self.assertTrue(any("checksum_table.rs" in p for p in problems), problems)

    def test_the_shipped_generated_file_inventories_are_complete(self):
        docs = {
            name: guard.read(guard.REPO / name)
            for name in guard.GENERATED_INVENTORY_DOCS
        }
        self.assertEqual(guard.check_generated_inventory(docs), [])


class BaselineFigureTests(unittest.TestCase):
    def test_a_stale_export_measurement_is_reported(self):
        tables = guard.baseline_table_figures()
        live = f"{tables['fields.parquet'][0]:,}"
        stale = f"{tables['fields.parquet'][0] + 1:,}"
        rows = "\n".join(f"| `{name}` | {n:,} | {size:,} |"
                         for name, (n, size) in tables.items())
        docs = {"README.md": rows.replace(live, stale, 1)}
        problems = guard.check_baseline_figures(docs, tables)
        self.assertTrue(any("fields.parquet" in p for p in problems), problems)

    def test_a_missing_baseline_table_is_reported(self):
        tables = guard.baseline_table_figures()
        problems = guard.check_baseline_figures(
            {"README.md": "No measured export table here."}, tables
        )
        self.assertEqual(len(problems), len(tables), problems)

    def test_every_checkpoint_table_is_checked_against_its_own_baseline(self):
        checkpoint = json.loads(guard.read(
            guard.REPO / "tools" / "baselines" / "checkpoint_02d4d478.json"))
        expected = {
            f"{name}.parquet": (int(values["rows"]), int(values["bytes"]))
            for name, values in checkpoint["parquet"].items()
            if name.startswith("checkpoint_")
        }
        self.assertGreater(len(expected), 1, "the baseline lost its checkpoint tables")
        tables = guard.baseline_table_figures()
        self.assertEqual({name: tables.get(name) for name in expected}, expected)

    def test_the_shipped_docs_quote_every_live_baseline_figure(self):
        docs = {
            "README.md": guard.read(guard.README),
            "docs/USAGE.md": guard.read(guard.USAGE),
        }
        self.assertEqual(
            guard.check_baseline_figures(docs, guard.baseline_table_figures()), []
        )


class DocCoverageTests(unittest.TestCase):
    def test_data_md_contributing_and_claude_md_are_covered(self):
        self.assertLessEqual({"docs/DATA.md", "CONTRIBUTING.md", "CLAUDE.md"},
                             set(guard.ALL_DOCS))


class CheckCountTests(unittest.TestCase):
    """The printed `N checks` is the number of results main() combines, read
    from its source: the elements of the `checks` list plus each
    `checks.append` (the suite measurement)."""

    def checks_in_main(self) -> tuple[int, int]:
        tree = ast.parse(textwrap.dedent(inspect.getsource(guard.main)))
        listed = [node.value for node in ast.walk(tree)
                  if isinstance(node, ast.Assign)
                  and [getattr(t, "id", None) for t in node.targets] == ["checks"]
                  and isinstance(node.value, ast.List)]
        self.assertEqual(len(listed), 1, "main() must combine its checks in one "
                         "`checks` list, or its printed count is not derived from them")
        appended = sum(1 for node in ast.walk(tree)
                       if isinstance(node, ast.Call)
                       and isinstance(node.func, ast.Attribute)
                       and node.func.attr == "append"
                       and getattr(node.func.value, "id", None) == "checks")
        return len(listed[0].elts), appended

    def printed_count(self, *argv: str) -> int:
        output = io.StringIO()
        with patch.object(sys, "argv", ["check_docs.py", *argv]), \
                patch.object(guard, "measure_tests", return_value=(1, 1, [])), \
                contextlib.redirect_stdout(output), \
                contextlib.redirect_stderr(io.StringIO()):
            guard.main()
        found = re.search(r"(\d+) checks$", output.getvalue(), re.M)
        self.assertIsNotNone(found, output.getvalue())
        return int(found.group(1))

    def test_the_printed_count_is_the_number_of_checks_combined(self):
        listed, appended = self.checks_in_main()
        self.assertEqual(self.printed_count("--fast"), listed)
        self.assertEqual(self.printed_count(), listed + appended)


if __name__ == "__main__":
    unittest.main()
