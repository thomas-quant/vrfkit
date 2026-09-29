"""Assert that the prose docs still describe THIS repo.

A number measured once gets quoted forever, and a stale sentence compiles and
passes every test. So this reads the repo and the docs and compares:

  1. every tools/*.py script is mentioned in USAGE (an unmentioned tool is an
     undiscoverable one)
  2. every script USAGE names exists
  3. every crate has a row in the layer table
  4. every relative link in README and USAGE resolves
  5. README and USAGE quote the live overlay table sizes
  6. no quoted table size in any of `ALL_DOCS` is stale, even beside a live
     one (`stale_table_size_claims`)
  7. no Rust doc comment or Cargo.toml quotes a stale one
  8. README and USAGE quote the live test counts, and no stale count sits
     beside a live one (`stale_test_counts`); `--fast` keeps only
     `contradicting_test_counts`
  9. no `MEASURED_RE` count in any of `ALL_DOCS` is stale, and a count that
     could not be measured is reported rather than skipped
 10. every relative link in the other `ALL_DOCS` and in docs/*.md resolves
 11. generated-file inventories include every live target and generator
 12. README and USAGE export rows/bytes match the committed baseline JSON
 13. the five overlay buckets in that baseline still partition
     `overlay_rows_offered` exactly, and every counter the docs quote is
     present (`overlay_partition_problems`)
 14. no quoted overlay counter or `Typed` ratio in any of `ALL_DOCS` is stale
     against that baseline (`stale_overlay_counters`)
 15. README still carries the overlay summary block, so (14) cannot be
     satisfied by deleting it
 16. both build tables use the same verification method and measured clean/
     checked counts, covering exactly the registered payload transforms
 17. CONTRIBUTING's `cargo check` lines and ci.yml's `$matrix` are the same
     cases in the same order
 18. every `#anchor` a link in a link-checked doc names, and every
     `docs/<name>.md#<anchor>` a Rust or Python source names, is a heading
     of its target by GitHub's slug rules (`anchor_problems`). docs/archive/
     is a target, never a source. Setext headings are not read, so a link to
     one is reported rather than passed.

A number is guarded when something in the repo can be *run* to produce it.
docs/DATA.md's measurements ("377,487 elements", "1,021 windows") come from
analysis runs, so a check would only be a second copy of the number. (12)-(15)
read committed JSON, so none of them needs a `.vrf`.

(8) runs both suites, roughly `cargo test` plus the tools suite: run the full
guard when touching docs or before finishing a session. CI runs it in the
Windows MSRV job, where Rust and Python are both installed; the Python
platform/version matrix runs `--fast` for source/document consistency.

Usage:
    python tools/check_docs.py
    python tools/check_docs.py --fast     # skip (8), no test runs
"""
from __future__ import annotations

import argparse
import functools
import json
import re
import importlib.util
import subprocess
import sys
import unicodedata
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parent.parent
README = REPO / "README.md"
USAGE = REPO / "docs" / "USAGE.md"

GENERATED_INVENTORY = {
    "crates/vrf-decode/src/checksum_table.rs": "tools/extract_checksum_types.py",
    "crates/vrf-decode/src/scoped_types.rs": "tools/generate_scoped_types.py",
    "crates/vrf-transform/tests/data/native_vectors.rs": "tools/capture_native_transforms.py",
}
GENERATED_INVENTORY_DOCS = (
    "README.md",
    "CONTRIBUTING.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
)

#: Named in the docs but not shipped here.
EXTERNAL_SCRIPTS = {"compute_metrics.py", "python_interop.py"}

BUILD_AUDIT = REPO / "tools/fixtures/build_verification.json"
BUILD_METHOD = "Validation + checkpoints + typed/raw"

LINK_RE = re.compile(r"\[`?([^\]]+?)`?\]\(([^)]+)\)")
SCRIPT_RE = re.compile(r"`?([a-z_][a-z0-9_]*\.py)`?")


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def check_build_verification(readme: str, usage: str, registry: str, report: dict) -> list[str]:
    """Both public tables must use the same measured scope and acceptance rule."""
    problems = []
    # lib.rs's `transforms!` block; the `ALL_VERSIONS` literal is the test fixtures' form.
    match = (re.search(r"^transforms! \{$(.*?)^\}$", registry, re.S | re.M)
             or re.search(r"ALL_VERSIONS:.*?=\s*&\[(.*?)\];", registry, re.S))
    if not match:
        return ["cannot read supported transform registry"]
    versions = {f"{a}.{b}" for a, b in re.findall(r"\bV(\d\d)(\d\d)\b", match[1])}
    if not versions:
        return ["supported transform registry is empty"]
    measured = {branch.removeprefix("++Ares-Core+release-"): row
                for branch, row in report.get("builds", {}).items()}
    if set(measured) != versions:
        problems.append("build audit does not cover exactly the supported registry")
    if report.get("executable_changed") is not False:
        problems.append("build audit executable changed or its integrity result is absent")
    if report.get("build_errors"):
        problems.append("build audit has unresolved build-level errors")
    for version, row in measured.items():
        counts = [row.get(key) for key in ("replays", "passed", "failed")]
        if (any(type(value) is not int or value < 0 for value in counts)
                or counts[0] == 0 or counts[1] + counts[2] != counts[0]):
            problems.append(f"build audit {version}: invalid replay accounting")
            continue
        hashes = row.get("input_sha256", [])
        if len(hashes) != counts[0] or len(set(hashes)) != counts[0]:
            problems.append(f"build audit {version}: input hashes do not match replay count")
        work = row.get("counts", {})
        checkpoint_counts = [work.get(key) for key in
                             ("checkpoint_content_blocks", "checkpoint_overlay_decoded_ok")]
        if (row.get("checkpoint_evidence") != "observed"
                or any(type(value) is not int or value <= 0 for value in checkpoint_counts)):
            problems.append(f"build audit {version}: no positive checkpoint decoding evidence")
    for name, doc, readme_table in (("README", readme, True), ("USAGE", usage, False)):
        for quoted in re.findall(r"Payload transform \((\d+) builds\)", doc):
            if int(quoted) != len(versions):
                problems.append(f"{name}: transform layer lists {quoted} builds, registry has {len(versions)}")
        rows = {}
        for line in doc.splitlines():
            cells = [cell.strip().replace("**", "") for cell in line.strip().strip("|").split("|")]
            if not cells or not re.fullmatch(r"\d{2}\.\d{2}", cells[0]):
                continue
            version = cells[0]
            if version in rows:
                problems.append(f"{name}: duplicate build row {version}")
            rows[version] = cells
        if set(rows) != versions:
            problems.append(f"{name}: support table differs from supported registry")
        for version in sorted(versions & set(rows) & set(measured)):
            cells, actual = rows[version], measured[version]
            expected = f"{actual['passed']}/{actual['replays']}"
            count_index = 2 if readme_table else 1
            if len(cells) != count_index + 2 or cells[count_index] != expected:
                problems.append(f"{name}: {version} clean/checked must be {expected}")
            if cells[-1] != BUILD_METHOD:
                problems.append(f"{name}: {version} uses a different verification method")
            if readme_table and cells[1] != f"`release-{version}`":
                problems.append(f"{name}: {version} branch label differs")
    return problems


def check_tools(usage: str) -> list[str]:
    """Every shipped tool is documented, and every documented tool ships."""
    problems = []
    shipped = {p.name for p in (REPO / "tools").glob("*.py")}
    named = {n for n in SCRIPT_RE.findall(usage) if not n.startswith("test_")}

    for name in sorted(shipped - named):
        problems.append(f"tools/{name} exists but docs/USAGE.md never mentions it")
    for name in sorted(named - shipped - EXTERNAL_SCRIPTS):
        problems.append(f"docs/USAGE.md names tools/{name}, which does not exist")
    return problems


def check_crates(usage: str) -> list[str]:
    crates = {p.parent.name for p in (REPO / "crates").glob("*/Cargo.toml")}
    return [f"crate {c} has no row in the docs/USAGE.md layer table"
            for c in sorted(crates) if f"`{c}`" not in usage]


def check_links(path: Path, text: str) -> list[str]:
    problems = []
    for _label, target in LINK_RE.findall(text):
        if target.startswith(("http://", "https://", "#", "mailto:")):
            continue
        target = target.split("#", 1)[0]
        if target and not (path.parent / target).exists():
            problems.append(f"{path.name}: link -> {target} does not resolve")
    return problems


def table_lengths() -> tuple[str, str] | None:
    table = read(REPO / "crates" / "vrf-decode" / "src" / "table.rs")
    entries = re.search(r"OVERLAY_TABLE: \[OverlayEntry; (\d+)\]", table)
    handles = re.search(r"OVERLAY_HANDLE_TABLE: \[OverlayHandleEntry; (\d+)\]", table)
    return (entries.group(1), handles.group(1)) if entries and handles else None


def check_table_sizes(docs: dict[str, str]) -> list[str]:
    """The overlay table's declared lengths, as quoted in prose."""
    lengths = table_lengths()
    if lengths is None:
        return ["table.rs: could not read the declared slice lengths"]

    problems = []
    for n, what, where in ((lengths[0], "overlay table", ("README.md", "USAGE.md")),
                           (lengths[1], "handle table", ("USAGE.md",))):
        pretty = f"{int(n):,}"
        for name in where:
            if n not in docs[name] and pretty not in docs[name]:
                problems.append(f"{name}: {what} is {pretty}, not quoted")
    return problems


#: How prose states the two table sizes, narrow enough that a match
#: is always that claim. Every such number must be the live one, so a stale
#: figure cannot hide beside a correct one (`check_table_sizes` only asks
#: whether the live number appears; see `stale_test_counts`).
TABLE_CLAIM_RE = (
    ("overlay table", 0, re.compile(r"(\d[\d,]*)\s+entries\b")),
    ("handle table", 1, re.compile(r"(\d[\d,]*)\s+handles\b")),
)


def stale_table_size_claims(docs: dict[str, str], lengths) -> list[str]:
    """Every quoted table size, in any doc, that is not the live one."""
    if lengths is None:
        return ["table.rs: could not read the declared slice lengths"]
    return [
        f"{name}:{i}: says {quoted} {what.split()[0]} entries/handles, but the "
        f"{what} holds {int(lengths[index]):,}"
        for name, text in docs.items()
        for i, line in enumerate(text.splitlines(), 1)
        for what, index, pattern in TABLE_CLAIM_RE
        for quoted in pattern.findall(line)
        if quoted.replace(",", "") != lengths[index]
    ]


#: The phrase Rust doc comments and Cargo.toml use for the table's size, kept
#: this narrow (not any "N entries") so a match is always a size claim.
ENTRY_PHRASE_RE = re.compile(r"([\d,]+)-entry (?:generated )?table")


def stale_entry_phrases(text: str, live: set[str]) -> list[tuple[int, str]]:
    """`(line number, quoted size)` for every table-size claim not in `live`,
    which holds both spellings (1188 and 1,188 are the same claim)."""
    return [(i, quoted)
            for i, line in enumerate(text.splitlines(), 1)
            for quoted in ENTRY_PHRASE_RE.findall(line)
            if quoted not in live]


def check_source_table_size() -> list[str]:
    """The table size as quoted in Rust prose and Cargo.toml (`vrf-decode`'s
    crate docs, its feature table and its manifest)."""
    lengths = table_lengths()
    if lengths is None:
        return []
    n = int(lengths[0])
    live = {lengths[0], f"{n:,}"}

    sources = sorted((REPO / "crates").rglob("*.rs"))
    sources += sorted((REPO / "crates").rglob("Cargo.toml"))
    return [f"{path.relative_to(REPO).as_posix()}:{i}: says {quoted}-entry "
            f"table; it is {n:,}"
            for path in sources
            for i, quoted in stale_entry_phrases(read(path), live)]


#: The phrase the docs use to state a suite size, narrow enough that a match
#: is always a claim about one of the two suites. The suite may be named
#: between the number and the noun, as README's highlight does ("793 Rust
#: tests"); that line went stale twice while only the plain form was read.
TEST_COUNT_RE = re.compile(r"(\d[\d,]*)\s+(?:(Rust|Python)\s+)?(?:tests|passing)\b")


def stale_test_counts(text: str, live: set[str],
                      by_suite: dict[str, set[str]] | None = None) -> list[tuple[int, str]]:
    """`(line number, quoted count)` for every suite-size claim not in `live`.

    Presence is not agreement: README carried `387 tests` and `355 passing` at
    once for twelve commits, and a check asking whether the live number
    appears somewhere passed on the first. `live` holds both suite counts in
    both spellings, and every claim must be one of them; a claim that names
    its suite must be that suite's, when `by_suite` gives it.
    """
    return [(i, quoted)
            for i, line in enumerate(text.splitlines(), 1)
            for quoted, suite in TEST_COUNT_RE.findall(line)
            if quoted not in ((by_suite or {}).get(suite) or live)]


def contradicting_test_counts(docs: dict[str, str]) -> list[str]:
    """Suite-size claims that cannot all be true at once.

    `stale_test_counts` needs the real numbers, so it runs only in full mode;
    this survives `--fast`. It cannot know which number is right and need not:
    there are exactly two suites, so a third distinct value is a
    contradiction. Blind to a count wrong the same way everywhere.
    """
    seen: list[tuple[str, int, str]] = [
        (name, i, quoted)
        for name, text in docs.items()
        for i, line in enumerate(text.splitlines(), 1)
        for quoted, _suite in TEST_COUNT_RE.findall(line)
    ]
    distinct = {quoted.replace(",", "") for _, _, quoted in seen}
    if len(distinct) <= 2:
        return []
    sites = ", ".join(f"{name}:{i} says {quoted}" for name, i, quoted in seen)
    return [
        f"{len(distinct)} different test counts claimed but there are two "
        f"suites, so at least one is stale -- {sites}"
    ]


#: Every doc this guard reads. README and USAGE must *quote* the live numbers;
#: the rest only must not contradict them (CLAUDE.md quotes no counts by
#: design, so this tier adds no obligation there).
ALL_DOCS = ("README.md", "docs/USAGE.md", "docs/DATA.md", "CONTRIBUTING.md",
            "CLAUDE.md")

#: Numbers quoted in prose *and* produced by something runnable, as `(count
#: pattern, context, context window)`, phrased narrowly enough that a match is
#: always that claim. Where there is a context, a line must name its check to
#: be read as claiming the count: README's "all 215 files" is about replays,
#: not the ASCII sweep. The window is how many lines ABOVE the count the
#: context may sit, because prose wraps: USAGE.md writes "The `ADDITIONS`
#: pass ... There are" and "currently 73 of them" on the next line. Widening
#: the pattern instead would read the generic "currently N of them" wherever
#: a future paragraph uses it.
MEASURED_RE = {
    "ascii": (re.compile(r"(\d+) files?\b"), re.compile(r"ascii", re.I), 0),
    "corrections": (re.compile(r"(\d+) corrections"), None, 0),
    # `ADDITIONS`, the descriptor-silent subset of the corrections list.
    "additions": (re.compile(r"currently (\d+) of them"),
                  re.compile(r"ADDITIONS"), 2),
    # Each phrase below is specific enough to be that claim with no context.
    "matrix": (re.compile(r"(\d+) `cargo check`"), None, 0),
    "matrix_cases": (re.compile(r"the same (\d+) cases"), None, 0),
    "golden": (re.compile(r"(\d+) mechanically extracted golden vectors"), None, 0),
    # "N builds" is generic -- the transform table talks about builds too -- so
    # it counts only on a line that names the semantic guard.
    "metrics_builds": (re.compile(r"(\d+) builds\b"),
                       re.compile(r"check_metrics_baseline"), 0),
}

#: The CONTRIBUTING and ci.yml spellings of the feature-check matrix.
MATRIX_LINE_RE = re.compile(r"cargo \+1\.86\.0 check -p (\S+) --no-default-features"
                            r"(?: --features (\S+))? --locked")
MATRIX_BLOCK_RE = re.compile(r"\$matrix = @\((.*?)\n\s*\)", re.S)
MATRIX_CASE_RE = re.compile(r'@\("([^"]+)",\s*"([^"]*)"\)')
GOLDEN_LEN_RE = re.compile(r"pub const VECTORS: \[\(&str, usize, &str\); (\d+)\]")


def feature_matrices(contributing: str, ci: str) -> tuple[list[tuple[str, str]], list[tuple[str, str]] | None]:
    """The (crate, feature) cases each file runs, in order. `None` for ci.yml
    means its matrix block could not be found at all."""
    documented = [(crate, feature or "") for crate, feature in MATRIX_LINE_RE.findall(contributing)]
    block = MATRIX_BLOCK_RE.search(ci)
    return documented, (MATRIX_CASE_RE.findall(block.group(1)) if block else None)


def check_feature_matrix(contributing: str, ci: str) -> list[str]:
    """CONTRIBUTING's `cargo check` lines and ci.yml's `$matrix` must be the
    same cases in the same order. An empty or unparseable side is a problem,
    never a vacuous match."""
    documented, ci_cases = feature_matrices(contributing, ci)
    if not documented:
        return ["CONTRIBUTING.md: no `cargo +1.86.0 check -p ... --no-default-features` lines found"]
    if ci_cases is None:
        return [".github/workflows/ci.yml: no `$matrix = @(...)` block found"]
    problems = [f"feature matrix: {case} is in CONTRIBUTING.md but not ci.yml"
                for case in documented if case not in ci_cases]
    problems += [f"feature matrix: {case} is in ci.yml but not CONTRIBUTING.md"
                 for case in ci_cases if case not in documented]
    if not problems and documented != ci_cases:
        problems.append("feature matrix: CONTRIBUTING.md and ci.yml list the same "
                        "cases in a different order")
    return problems


def link_checked_docs() -> list[Path]:
    """Every doc whose relative links are checked: the five read above plus
    every top-level file under docs/. docs/archive/ is dated history and keeps
    whatever it linked to at the time."""
    paths = {REPO / name for name in ALL_DOCS}
    paths.update((REPO / "docs").glob("*.md"))
    return sorted(paths)


FENCE_OPEN_RE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
FENCE_CLOSE_RE = re.compile(r"^ {0,3}(`{3,}|~{3,})[ \t]*$")
ATX_HEADING_RE = re.compile(r"^ {0,3}#{1,6}(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$")
CODE_SPAN_RE = re.compile(r"(`+)(.+?)\1")
INLINE_LINK_RE = re.compile(r"!?\[([^\]]*)\]\([^)]*\)")
#: A Rust or Python source naming a doc heading. A slug never holds a dot, so
#: a sentence-ending `.` after the anchor is not read as part of it.
CODE_ANCHOR_RE = re.compile(r"\b(docs/(?:[\w.-]+/)*[\w.-]+\.md)#([\w-]+)")


def unfenced_lines(text: str):
    """`(line number, line)` for every line outside a fenced code block, where
    a `#` line is a comment and a `[x](y)` is sample text, not a link."""
    fence = None
    for i, line in enumerate(text.splitlines(), 1):
        if fence:
            close = FENCE_CLOSE_RE.match(line)
            if close and close.group(1)[0] == fence[0] and len(close.group(1)) >= len(fence):
                fence = None
            continue
        opened = FENCE_OPEN_RE.match(line)
        if opened:
            fence = opened.group(1)
            continue
        yield i, line


def github_slug(heading: str) -> str:
    """The anchor GitHub renders for a heading: the rendered text (code spans
    keep their content, links their text, images drop out) lowercased, every
    character that is not a letter, mark, digit, `_`, `-` or space removed,
    and each space turned into `-`. Underscore emphasis is not rendered."""
    def outside_code(segment: str) -> str:
        return INLINE_LINK_RE.sub(
            lambda m: "" if m.group(0).startswith("!") else m.group(1), segment)

    parts, pos = [], 0
    for span in CODE_SPAN_RE.finditer(heading):
        parts.append(outside_code(heading[pos:span.start()]))
        code = span.group(2)
        if code.startswith(" ") and code.endswith(" ") and code.strip():
            code = code[1:-1]
        parts.append(code)
        pos = span.end()
    parts.append(outside_code(heading[pos:]))
    return "".join(
        ch for ch in "".join(parts).lower()
        if ch in " -" or unicodedata.category(ch) in ("Nd", "Pc")
        or unicodedata.category(ch)[0] in "LM").replace(" ", "-")


def heading_anchors(text: str) -> set[str]:
    """Every anchor a document's ATX headings give, with GitHub's `-1`, `-2`
    suffixes on repeats."""
    seen: dict[str, int] = {}
    anchors = set()
    for _, line in unfenced_lines(text):
        heading = ATX_HEADING_RE.match(line)
        if not heading:
            continue
        slug = github_slug((heading.group(1) or "").strip())
        count = seen.get(slug, 0)
        anchors.add(f"{slug}-{count}" if count else slug)
        seen[slug] = count + 1
    return anchors


@functools.lru_cache(maxsize=None)
def anchors_of(path: Path) -> frozenset[str] | None:
    """The anchors of a markdown file on disk; `None` when it does not exist."""
    return frozenset(heading_anchors(read(path))) if path.is_file() else None


def broken_markdown_anchors(path: Path, text: str, lookup=anchors_of,
                            checked: list | None = None) -> list[str]:
    """Links in one doc whose `#anchor` names no heading of their target;
    each link checked is appended to `checked`. A missing target file is
    `check_links`'s report, and an anchor on a non-markdown target (a `#L10`
    line link) is GitHub's, not a heading."""
    problems = []
    for i, line in unfenced_lines(text):
        for _label, target in LINK_RE.findall(line):
            target = target.strip().split()[0] if target.strip() else ""
            if target.startswith(("http://", "https://", "mailto:")) or "#" not in target:
                continue
            file_part, fragment = target.split("#", 1)
            dest = (path.parent / file_part).resolve() if file_part else path
            if not fragment or dest.suffix.lower() != ".md":
                continue
            anchors = lookup(dest)
            if anchors is None:
                continue
            if checked is not None:
                checked.append(target)
            if unquote(fragment) not in anchors:
                problems.append(f"{path.name}:{i}: link -> {target}: "
                                f"{dest.name} has no heading #{unquote(fragment)}")
    return problems


def broken_code_anchors(name: str, text: str, lookup=anchors_of,
                        checked: list | None = None) -> list[str]:
    """`docs/<name>.md#<anchor>` references in one source file that name a
    doc or a heading that does not exist; each one is appended to
    `checked`. Paths are repository-relative."""
    problems = []
    for i, line in enumerate(text.splitlines(), 1):
        for doc, fragment in CODE_ANCHOR_RE.findall(line):
            if checked is not None:
                checked.append(f"{doc}#{fragment}")
            anchors = lookup((REPO / doc).resolve())
            if anchors is None:
                problems.append(f"{name}:{i}: cites {doc}#{fragment}, but {doc} does not exist")
            elif fragment not in anchors:
                problems.append(f"{name}:{i}: cites {doc}#{fragment}, but {doc} has no such heading")
    return problems


def anchor_problems(checked: dict[str, list] | None = None) -> list[str]:
    """Every broken anchor in the link-checked docs and the tracked Rust and
    Python sources. `checked["docs"]` and `checked["code"]` receive every
    reference read, so a run that read none can say so. A source list that
    cannot be read is reported, not treated as an empty one."""
    checked = {} if checked is None else checked
    docs, code = checked.setdefault("docs", []), checked.setdefault("code", [])
    problems = [p for path in link_checked_docs()
                for p in broken_markdown_anchors(path, read(path), checked=docs)]
    r = subprocess.run(["git", "-C", str(REPO), "ls-files", "--", "*.rs", "*.py"],
                       capture_output=True, text=True, encoding="utf-8",
                       errors="replace", timeout=120)
    if r.returncode != 0:
        return problems + [
            f"could not list the Rust and Python sources: git ls-files exited "
            f"{r.returncode} ({(r.stderr or '').strip()[:120]}); every code "
            f"anchor went unchecked"]
    for name in r.stdout.splitlines():
        if name.strip():
            text = (REPO / name).read_text(encoding="utf-8", errors="replace")
            problems += broken_code_anchors(name, text, checked=code)
    return problems


def check_generated_inventory(docs: dict[str, str]) -> list[str]:
    """Every live generated target is named wherever contributors check it."""
    problems = []
    for name, text in docs.items():
        for target, generator in GENERATED_INVENTORY.items():
            target_claim = Path(target).name if name.endswith("PULL_REQUEST_TEMPLATE.md") else target
            if f"`{target_claim}`" not in text:
                problems.append(f"{name}: generated inventory is missing `{target_claim}`")
            if (
                not name.endswith("PULL_REQUEST_TEMPLATE.md")
                and f"`{generator}`" not in text
            ):
                problems.append(f"{name}: generated inventory is missing `{generator}`")
    return problems


def baseline_table_figures() -> dict[str, tuple[int, int]]:
    """Rows and bytes promised by the committed reference export baselines:
    the main tables from the export baseline (the docs quote the default
    main-only run), every `checkpoint_*` table from the checkpoint baseline."""
    export = json.loads(read(REPO / "tools" / "baselines" / "export_02d4d478.json"))
    checkpoint = json.loads(
        read(REPO / "tools" / "baselines" / "checkpoint_02d4d478.json")
    )
    figures = {
        f"{name}.parquet": (int(values["rows"]), int(values["bytes"]))
        for name, values in export["parquet"].items()
    }
    figures.update(
        (f"{name}.parquet", (int(values["rows"]), int(values["bytes"])))
        for name, values in checkpoint["parquet"].items()
        if name.startswith("checkpoint_")
    )
    return figures


def check_baseline_figures(
    docs: dict[str, str], figures: dict[str, tuple[int, int]]
) -> list[str]:
    """Every measured export row in active docs must match the live baseline."""
    problems = []
    for doc_name, text in docs.items():
        for table_name, expected in figures.items():
            pattern = re.compile(
                rf"^\|\s*`?{re.escape(table_name)}`?\s*\|"
                rf"\s*([\d,]+)\s*\|\s*([\d,]+)\s*\|",
                re.MULTILINE,
            )
            matches = pattern.findall(text)
            if not matches:
                problems.append(
                    f"{doc_name}: measured export table is missing {table_name}"
                )
                continue
            for quoted_rows, quoted_bytes in matches:
                actual = (
                    int(quoted_rows.replace(",", "")),
                    int(quoted_bytes.replace(",", "")),
                )
                if actual != expected:
                    problems.append(
                        f"{doc_name}: {table_name} says {actual[0]:,} rows / "
                        f"{actual[1]:,} bytes, baseline says {expected[0]:,} / "
                        f"{expected[1]:,}"
                    )
    return problems


#: The overlay summary block README reprints, keyed by the label as printed and
#: mapped to the baseline counter it quotes. `export_02d4d478.json` records
#: them for the reference replay, so no `.vrf` is needed (CI has none).
#:
#: `Decode errors` is deliberately absent: "Decode errors: 0" names a failure
#: mode in CLAUDE.md (twice), docs/DATA.md and docs/USAGE.md rather than
#: measuring this replay, so guarding it would fire on all four the first time
#: the baseline recorded a nonzero value.
OVERLAY_COUNTER_KEYS = {
    "Decoded OK": "overlay_decoded_ok",
    "Raw/Skip": "overlay_raw_skip",
    "Not in table": "overlay_not_in_table",
    "No field name": "overlay_no_field_name",
    "Effect blobs": "effect_blobs_decoded",
}

#: The five buckets `print_overlay` (crates/vrfkit/src/driver/summary.rs) sums
#: to print `Rows offered`. Their sum is `overlay_rows_offered` exactly, so a
#: break means a bucket was added or one stopped counting.
#: `overlay_decode_errors` belongs here although `OVERLAY_COUNTER_KEYS` excludes
#: it: that exclusion is about doc prose, not arithmetic, and without it the
#: check would pass only while the pinned decode-error count is 0.
OVERLAY_PARTITION = ("overlay_decoded_ok", "overlay_decode_errors",
                     "overlay_raw_skip", "overlay_not_in_table",
                     "overlay_no_field_name")

#: `Typed` is not stored: it is `Decoded OK / Rows offered` as a percentage,
#: derived rather than pinned so it cannot drift from the two counters.
TYPED_RE = re.compile(r"Typed:\s*([\d.]+)%")


def baseline_overlay_counters() -> dict[str, int]:
    """The overlay counters the committed reference export recorded."""
    export = json.loads(read(REPO / "tools" / "baselines" / "export_02d4d478.json"))
    return {k: int(v) for k, v in export["counters"].items()}


def overlay_partition_problems(counters: dict[str, int]) -> list[str]:
    """The buckets must still add up, and the keys must still be there: a
    missing key would leave every line quoting it unchecked while
    `stale_overlay_counters` skipped it and this printed OK."""
    problems = []
    needed = set(OVERLAY_PARTITION) | {"overlay_rows_offered"} | set(
        OVERLAY_COUNTER_KEYS.values())
    for key in sorted(needed - counters.keys()):
        problems.append(
            f"export_02d4d478.json: counters.{key} is missing, so every doc "
            f"line quoting it went unchecked")
    if needed - counters.keys():
        return problems

    total = sum(counters[k] for k in OVERLAY_PARTITION)
    offered = counters["overlay_rows_offered"]
    if total != offered:
        problems.append(
            f"export_02d4d478.json: the five overlay buckets sum to {total:,} "
            f"but counters.overlay_rows_offered is {offered:,}; they are "
            f"documented as a partition of every row offered "
            f"({' + '.join(k.removeprefix('overlay_') for k in OVERLAY_PARTITION)})")
    return problems


def stale_overlay_counters(docs: dict[str, str],
                           counters: dict[str, int]) -> list[str]:
    """Every quoted overlay counter, in any doc, that is not the live one.

    Scoped to the printed `Label: N` form, so prose that merely names a bucket
    (README's "most of `Not in table` is RPC parameters") is not a quote.
    """
    problems = []
    for name, text in docs.items():
        for i, line in enumerate(text.splitlines(), 1):
            for label, key in OVERLAY_COUNTER_KEYS.items():
                if key not in counters:
                    continue
                for quoted in re.findall(
                        rf"{re.escape(label)}:\s*([\d,]+)", line):
                    if int(quoted.replace(",", "")) != counters[key]:
                        problems.append(
                            f"{name}:{i}: says {label} {quoted}, but "
                            f"counters.{key} is {counters[key]:,}")
            if "overlay_decoded_ok" not in counters:
                continue
            for quoted in TYPED_RE.findall(line):
                # Compared at the precision the doc chose, so 75.1 and 75.10
                # are the same claim and 75.2 is not.
                places = len(quoted.partition(".")[2])
                live = round(
                    100 * counters["overlay_decoded_ok"]
                    / counters["overlay_rows_offered"], places)
                if float(quoted) != live:
                    problems.append(
                        f"{name}:{i}: says Typed {quoted}%, but "
                        f"overlay_decoded_ok / overlay_rows_offered is "
                        f"{live}% ({counters['overlay_decoded_ok']:,} / "
                        f"{counters['overlay_rows_offered']:,})")
    return problems


def check_overlay_counters_present(readme: str,
                                   counters: dict[str, int]) -> list[str]:
    """README must still carry the block, not merely not contradict it:
    deleting it would satisfy `stale_overlay_counters` with nothing quoted."""
    return [f"README.md: overlay summary block is missing `{label}:`"
            for label, key in OVERLAY_COUNTER_KEYS.items()
            if key in counters
            and not re.search(rf"{re.escape(label)}:\s*[\d,]+", readme)]


def measured_counts(problems: list[str] | None = None) -> dict[str, int]:
    """The live values, read from the things that produce them.

    A measurement that could not be taken is left out, and
    `stale_measured_counts` skips a missing key; pass `problems` so that is
    reported instead of silently checking nothing.
    """
    counts = {}
    r = subprocess.run(["git", "-C", str(REPO), "ls-files", "--", "*.rs"],
                       capture_output=True, text=True, encoding="utf-8",
                       errors="replace", timeout=120)
    if r.returncode == 0:
        counts["ascii"] = len([ln for ln in r.stdout.splitlines() if ln.strip()])
    elif problems is not None:
        problems.append(
            f"could not measure the ascii file count: git ls-files exited "
            f"{r.returncode} ({(r.stderr or '').strip()[:120]}); every quoted "
            f"count went unchecked")

    # `tools/` on the path first: apply_type_corrections.py imports `atomic_io`,
    # which under `spec_from_file_location` resolves against sys.path, not the
    # file's own directory (ModuleNotFoundError otherwise).
    if str(REPO / "tools") not in sys.path:
        sys.path.insert(0, str(REPO / "tools"))
    spec = importlib.util.spec_from_file_location(
        "_atc", REPO / "tools" / "apply_type_corrections.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    counts["corrections"] = module.expectation_count(read(module.TABLE_RS))
    # Imported, not line-counted: each entry spans a commented block.
    counts["additions"] = len(module.ADDITIONS)

    documented, _ = feature_matrices(read(REPO / "CONTRIBUTING.md"), "")
    if documented:
        counts["matrix"] = counts["matrix_cases"] = len(documented)
    elif problems is not None:
        problems.append("could not measure the feature matrix: CONTRIBUTING.md "
                        "has no `cargo +1.86.0 check -p` lines")

    golden = GOLDEN_LEN_RE.search(read(
        REPO / "crates" / "vrf-transform" / "tests" / "data" / "golden_vectors.rs"))
    if golden:
        counts["golden"] = int(golden.group(1))
    elif problems is not None:
        problems.append("could not measure the golden-vector count: no "
                        "`VECTORS: [...; N]` declaration in golden_vectors.rs")

    metrics = json.loads(read(REPO / "tools" / "baselines" / "metrics_builds.json"))
    if isinstance(metrics.get("replays"), dict) and metrics["replays"]:
        counts["metrics_builds"] = len(metrics["replays"])
    elif problems is not None:
        problems.append("could not measure the semantic guard's build count: "
                        "metrics_builds.json has no replays")
    return counts


def stale_measured_counts(docs: dict[str, str], live: dict[str, int]) -> list[str]:
    """Every quoted measured count that is not the live one.

    Every match must be right, so a file saying 85, 86 and 49 corrections
    reports two problems. A count is a claim only when its context is on the
    same line or within `window` lines above it (see `MEASURED_RE`).
    """
    problems = []
    for name, text in docs.items():
        lines = text.splitlines()
        for i, line in enumerate(lines, 1):
            for what, (pattern, context, window) in MEASURED_RE.items():
                if what not in live:
                    continue
                if context is not None:
                    scope = lines[max(0, i - 1 - window):i]
                    if not any(context.search(ln) for ln in scope):
                        continue
                for quoted in pattern.findall(line):
                    if int(quoted) != live[what]:
                        problems.append(
                            f"{name}:{i}: says {quoted} {what}, but it is "
                            f"{live[what]}")
    return problems


def measure_tests() -> tuple[int, int, list[str]]:
    problems = []
    # The sweep's and CI's toolchain and lockfile: a newer default toolchain
    # could measure code that 1.86 rejects.
    r = subprocess.run(["cargo", "+1.86.0", "test", "--workspace", "--locked", "--quiet"],
                       cwd=REPO, capture_output=True,
                       text=True, encoding="utf-8", errors="replace", timeout=3600)
    out = (r.stdout or "") + (r.stderr or "")
    passed_matches = re.findall(r"^test result: ok\. (\d+) passed;", out, re.M)
    rust = sum(int(m) for m in passed_matches)
    if r.returncode != 0:
        problems.append("cargo test did not pass; doc counts not checked against it\n" + out[-4000:])
    elif not passed_matches:
        # Exit 0 with no "N passed" line means the output format changed, not
        # that nothing ran; a defaulted 0 would fail every quoted count wrongly.
        problems.append(
            "cargo test exited 0 but printed no 'N passed' line; the rust "
            "test count (0) was not measured")
    elif rust == 0:
        problems.append("cargo test reported zero passing tests")

    r2 = subprocess.run([sys.executable, "-W", "error", "-m", "unittest", "discover",
                         "-s", "tools/tests", "-p", "test_*.py"],
                        cwd=REPO, capture_output=True, text=True,
                        encoding="utf-8", errors="replace", timeout=1800)
    out2 = (r2.stdout or "") + (r2.stderr or "")
    m = re.search(r"^Ran (\d+) tests? in ", out2, re.M)
    tools_n = int(m.group(1)) if m else 0
    if r2.returncode != 0:
        problems.append("tools test suite did not pass\n" + out2[-4000:])
    elif m is None:
        problems.append(
            "the tools test suite exited 0 but printed no 'Ran N tests' "
            "line; the tools test count (0) was not measured")
    elif tools_n == 0:
        problems.append("tools test suite reported zero tests")
    if re.search(r"^OK \(.*skipped=[1-9]", out2, re.M):
        problems.append("tools test suite skipped tests; not every reported test passed")
    return rust, tools_n, problems


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fast", action="store_true",
                    help="skip the test-count check (does not run the suites)")
    args = ap.parse_args()

    if not USAGE.is_file():
        print(f"missing: {USAGE}", file=sys.stderr)
        return 2

    readme, usage = read(README), read(USAGE)
    docs = {"README.md": readme, "USAGE.md": usage}
    #: Every doc, for the checks that only ask a number not to be wrong; `docs`
    #: (README+USAGE) for the ones that require a number to be *present*.
    every = {name: read(REPO / name) for name in ALL_DOCS}
    generated_docs = {
        name: read(REPO / name) for name in GENERATED_INVENTORY_DOCS
    }

    measurement_problems: list[str] = []
    live_counts = measured_counts(measurement_problems)
    overlay_counters = baseline_overlay_counters()
    anchors_checked: dict[str, list] = {}

    # One entry per check: the summary prints len(checks), never a literal.
    checks = [
        check_tools(usage),
        check_crates(usage),
        check_links(README, readme),
        check_links(USAGE, usage),
        check_table_sizes(docs),
        stale_table_size_claims(every, table_lengths()),
        check_source_table_size(),
        contradicting_test_counts(every),
        measurement_problems,
        stale_measured_counts(every, live_counts),
        check_generated_inventory(generated_docs),
        check_baseline_figures(docs, baseline_table_figures()),
        overlay_partition_problems(overlay_counters),
        stale_overlay_counters(every, overlay_counters),
        check_overlay_counters_present(readme, overlay_counters),
        [p for name in ALL_DOCS
         for p in check_links(REPO / name, every[name])
         if name not in ("README.md", "docs/USAGE.md")],
        [p for path in link_checked_docs()
         if path.relative_to(REPO).as_posix() not in ALL_DOCS
         for p in check_links(path, read(path))],
        check_feature_matrix(read(REPO / "CONTRIBUTING.md"),
                             read(REPO / ".github" / "workflows" / "ci.yml")),
        check_build_verification(
            readme, usage, read(REPO / "crates/vrf-transform/src/lib.rs"),
            json.loads(read(BUILD_AUDIT))),
        anchor_problems(anchors_checked),
    ]

    if not args.fast:
        rust, tools_n, run_problems = measure_tests()
        for count, label in ((rust, "rust"), (tools_n, "tools")):
            for name, text in docs.items():
                if str(count) not in text:
                    run_problems.append(
                        f"{name}: {label} test count is {count}, not quoted")
        by_suite = {suite: {str(c), f"{c:,}"}
                    for suite, c in (("Rust", rust), ("Python", tools_n))}
        live = by_suite["Rust"] | by_suite["Python"]
        for name, text in every.items():
            run_problems += [
                f"{name}:{i}: says {quoted}; the suites are {rust} (Rust) "
                f"and {tools_n} (Python)"
                for i, quoted in stale_test_counts(text, live, by_suite)]
        print(f"tests: rust {rust}, tools {tools_n}")
        checks.append(run_problems)
    problems = [p for found in checks for p in found]

    n_tools = len(list((REPO / "tools").glob("*.py")))
    n_crates = len({p.parent.name for p in (REPO / "crates").glob("*/Cargo.toml")})
    print(f"docs: {len(ALL_DOCS)} files ({len(link_checked_docs())} link-checked)   "
          f"{n_tools} tools, {n_crates} crates, "
          f"{len(anchors_checked.get('docs', []))} doc links and "
          f"{len(anchors_checked.get('code', []))} code references to anchors, "
          f"{len(checks)} checks")

    if problems:
        print(f"\nFAILED: {len(problems)} stale or missing doc claim(s)",
              file=sys.stderr)
        for p in problems:
            print(f"    {p}", file=sys.stderr)
        return 1

    print("\nOK: the docs still describe this repo")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
