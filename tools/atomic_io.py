"""Small fail-closed helpers for generated files and output directories."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path



def sha256_file(path: Path) -> str:
    """Stream a file's SHA-256.

    Every extractor records one of these as provenance, and several compare
    theirs before and after a run to refuse a result produced while their own
    source was being edited. That makes this function part of those guards, not
    merely a utility: a tool whose integrity check already hashes `atomic_io.py`
    keeps covering this code, and a tool whose check does not must keep its own
    copy. `extract_kill_observations.py` records its hash without comparing it,
    and `extract_fastarray_observations.py` does not hash this file at all, so
    both deliberately keep theirs.
    """
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def aliases(path: Path, protected) -> bool:
    """Whether `path` names one of `protected`: the same resolved path, or
    the same file through a hard link (checked only when both exist)."""
    for item in protected:
        try:
            if path.exists() and item.exists() and path.samefile(item):
                return True
        except OSError:
            pass
        if path.resolve() == item.resolve():
            return True
    return False


def require_descendant(path: Path, root: Path, *, allow_root: bool = False) -> Path:
    """Resolve *path* and require it to stay below the resolved *root*."""
    resolved_root = root.resolve()
    resolved_path = path.resolve()
    if resolved_path == resolved_root:
        if allow_root:
            return resolved_path
        raise ValueError(f"refusing to operate on containment root: {resolved_path}")
    if not resolved_path.is_relative_to(resolved_root):
        raise ValueError(
            f"path escapes containment root {resolved_root}: {resolved_path}"
        )
    return resolved_path


def remove_tree(path: Path, root: Path) -> None:
    """Remove a directory only after resolving and checking containment."""
    resolved = require_descendant(path, root)
    if resolved.exists():
        shutil.rmtree(resolved)


def atomic_write_text(
    path: Path,
    content: str,
    *,
    encoding: str = "utf-8",
) -> None:
    """Replace a text file atomically, leaving the old file on failure."""
    path.parent.mkdir(parents=True, exist_ok=True)
    parent = path.parent.resolve()
    target = require_descendant(path, parent)
    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding=encoding, newline="") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, target)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def staged_output(export_dir: Path, out_dir: Path, inputs, write, *, prefix: str) -> dict:
    """Create `out_dir`, outside the export, from a staging directory that
    `write(stage)` fills and whose receipt it returns. The receipt gains the
    export `inputs`' hashes before and after; the stage is renamed into place
    only if they are equal, and removed otherwise."""
    export_dir, out_dir = export_dir.resolve(), out_dir.resolve()
    if out_dir == export_dir or out_dir.is_relative_to(export_dir):
        raise ValueError("output must be outside the source export")
    if out_dir.exists():
        raise ValueError("output directory already exists")
    before = {name: sha256_file(export_dir / name) for name in inputs}
    out_dir.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=prefix, dir=out_dir.parent))
    try:
        receipt = write(stage)
        after = {name: sha256_file(export_dir / name) for name in inputs}
        if before != after:
            raise ValueError("input changed during read")
        receipt.update(input_sha256_before=before, input_sha256_after=after)
        (stage / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n",
                                            encoding="utf-8", newline="\n")
        os.rename(stage, out_dir)
        return receipt
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def atomic_write_file(path: Path, write) -> None:
    """Replace a file atomically with what `write(handle)` writes to a binary
    handle -- `atomic_write_text` for writers such as pq.write_table."""
    path.parent.mkdir(parents=True, exist_ok=True)
    parent = path.parent.resolve()
    target = require_descendant(path, parent)
    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "wb") as handle:
            write(handle)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, target)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
