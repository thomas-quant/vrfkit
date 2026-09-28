"""Shared export discovery for the tools that read a directory of exports.

`vrfkit export` builds in `.{out}.vrfkit-staging-{pid}-{nonce}` and renames
it over `--out` once complete, moving a prior `--out` to
`.{out}.vrfkit-previous-{pid}-{nonce}` for one rename; a killed run or an
undeletable backup leaves them behind. Measured at 259ed10: `Stop-Process
-Force` 1.5 s into an export left a staging directory holding a
1,561,999-byte `fields.parquet` with no footer and no manifest, which the
audit counted as a failed export (candidate_exports 2, exit 1). A
`previous` sibling is a complete export, so tools read the replay twice and
exited 0: on one export beside its `previous` copy, the audit reported 2
corroborated RPCs for 1, the coverage summary 10 rows for 5, the raw
inventory 2 physical rows for 1, the type evidence 2 rows for 1.

`is_generated_sibling` is the one definition of those names, matched exactly
as crates/vrfkit/src/driver/publish.rs builds them (test_export_scan.py reads
its constants back). Discovery skips them and lists them under
`skipped_generated_dirs`, always present, so "nothing skipped" reads apart
from "never ran". A path named explicitly is never filtered. Nothing here
deletes; the export warns about leftovers when it next writes there.
"""
from __future__ import annotations

import re
from collections.abc import Iterable
from pathlib import Path

#: `publish.rs`'s text between destination and kind, and its kinds in the
#: order of its `STAGING` and `PREVIOUS` constants.
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
    """Split `parent`'s direct child directories into candidates (holding
    `fields.parquet`) and generated siblings, the latter listed even when
    empty (a staging directory killed early may hold nothing yet). Both
    sorted and unresolved."""
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


def discover_exports(inputs: Iterable[Path], skipped: list[Path] | None = None, *,
                     error: type[Exception] = ValueError,
                     no_children: str = "no direct child exports in") -> list[Path]:
    """Exports named directly, or the direct child exports of a parent.

    Sorted, resolved and unique. A parent's generated siblings are never
    exports; they are appended to `skipped` when it is given. `error` and
    `no_children` keep each caller's own exception class and wording.
    """
    exports: set[Path] = set()
    for root in inputs:
        if not root.is_dir():
            raise error(f"not an export directory or parent: {root}")
        if (root / "fields.parquet").is_file():
            exports.add(root.resolve())
            continue
        children, leftovers = child_exports(root)
        if skipped is not None:
            skipped.extend(leftovers)
        if not children:
            raise error(f"{no_children} {root}{leftover_note(leftovers)}")
        exports.update(child.resolve() for child in children)
    return sorted(exports)


def generated_ancestor(path: Path, root: Path) -> Path | None:
    """The generated sibling directory `path` lies in below `root`, if any,
    for a recursive search: only the directories between the two count."""
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
