"""Shared export discovery for the tools that read a directory of exports.

The defect this closes: `vrfkit export` writes into a sibling staging
directory, `.{out}.vrfkit-staging-{pid}-{nonce}`, and renames it over
`--out` only once every table and `manifest.json` are complete. A process
killed before that rename never runs its cleanup, so the staging directory
stays. Measured at 259ed10: killed 1.5 s into an export,
`.pub2.vrfkit-staging-55396-0` held a 1,561,999-byte `fields.parquet` with
no footer and no manifest. `audit_match_observations.py` took every child
holding `fields.parquet` for an export and counted that one as a failed
export; `validate_type_evidence.py`, `summarize_value_coverage.py` and
`summarize_unresolved_fields.py` globbed the same way.

The quiet case is the other sibling. Publication moves a prior `--out` to
`.{out}.vrfkit-previous-{pid}-{nonce}` for the length of one rename, and it
stays there when it cannot be deleted or the process dies between the two
renames. That directory is a complete export, manifest included, so every
one of those four tools read the replay twice and exited 0. Measured at
259ed10 on a fixture holding one export and its `previous` copy side by
side: the audit reported 2 corroborated RPCs for 1, the coverage summary 10
rows for 5, the raw inventory 2 physical rows for 1, the type evidence 2
rows for 1.

`is_generated_sibling` is the one definition of those names, matched exactly
as `crates/vrfkit/src/driver/publish.rs` builds them, and
`tools/tests/test_export_scan.py` reads that file's constants back so the
two cannot drift apart without a failing test. Tools skip such a directory
when *discovering* exports and list what they skipped under
`skipped_generated_dirs` in their report -- always present, an empty list
included, so "nothing was skipped" reads differently from "this check never
ran". A path the caller names explicitly is never filtered: pointing a tool
at a leftover is deliberate. The export itself warns about every such
sibling the next time it writes to the same destination, and deletes
nothing; neither does anything here.
"""
from __future__ import annotations

import re
from collections.abc import Iterable
from pathlib import Path

#: The text `publish.rs` puts between a destination name and the kind, and
#: the kinds it creates, in the order of its `STAGING` and `PREVIOUS`
#: constants. `tools/tests/test_export_scan.py` requires both to equal them.
GENERATED_INFIX = ".vrfkit-"
GENERATED_KINDS = ("staging", "previous")

#: `.{destination}.vrfkit-{kind}-{pid}-{nonce}`. The destination may hold any
#: character, dots and newlines included; pid and nonce are ASCII digits only
#: (`\d` would also accept other scripts' digits, which Rust never writes).
GENERATED_SIBLING = re.compile(
    r"\A\..+" + re.escape(GENERATED_INFIX)
    + "(?:" + "|".join(map(re.escape, GENERATED_KINDS)) + ")"
    + r"-[0-9]+-[0-9]+\Z",
    re.DOTALL,
)


def is_generated_sibling(name: str) -> bool:
    """Whether a directory named `name` is `vrfkit export` staging or backup."""
    return GENERATED_SIBLING.match(name) is not None


def child_exports(parent: Path) -> tuple[list[Path], list[Path]]:
    """Split `parent`'s direct child directories into candidates and leftovers.

    Returns the children holding `fields.parquet` that are not generated
    siblings, and every generated sibling directory -- listed whether or not
    it holds a table, because a staging directory killed early may hold
    nothing yet and is still a leftover worth naming. Both are sorted and
    unresolved; callers that key exports by resolved path resolve them.
    """
    candidates: list[Path] = []
    skipped: list[Path] = []
    for child in sorted(parent.iterdir()):
        if not child.is_dir():
            continue
        if is_generated_sibling(child.name):
            skipped.append(child)
        elif (child / "fields.parquet").is_file():
            candidates.append(child)
    return candidates, skipped


def generated_ancestor(path: Path, root: Path) -> Path | None:
    """The generated sibling directory `path` lies in below `root`, if any.

    For a recursive search. `root` itself never counts, and neither does
    `path`'s own final component: only the directories between them do.
    """
    current = root
    for part in path.relative_to(root).parts[:-1]:
        current = current / part
        if is_generated_sibling(part):
            return current
    return None


def skipped_report(skipped: Iterable[Path]) -> list[str]:
    """The `skipped_generated_dirs` value: resolved, unique, sorted."""
    return sorted({str(path.resolve()) for path in skipped})


def leftover_note(skipped: Iterable[Path]) -> str:
    """Appended to a tool's "no exports found" error, so a directory holding
    only leftovers says so instead of reading as an empty one."""
    names = skipped_report(skipped)
    if not names:
        return ""
    noun = "directory" if len(names) == 1 else "directories"
    return (f" (skipped {len(names)} vrfkit export staging/backup {noun}, which are "
            f"never exports: {', '.join(names)})")
