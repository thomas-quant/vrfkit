"""Audit two C# descriptor inputs without changing either checkout.

The inputs are either directory paths, or ``REPOSITORY::REVISION``.  A revision
is read with ``git archive`` into a temporary directory; the tool never fetches,
checks out, resets, or writes to either repository.  Its JSON result contains
the resolved commits, dirty state, exact inputs, and *review candidates*.

Example (the vendored table input against an upstream revision):
  python tools/compare_descriptor_sources.py \
    --baseline third_party/vrp \
    --candidate C:/src/ValorantReplayParser::b51d674... \
    --downstream-table crates/vrf-decode/src/table.rs --output audit.json
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
from datetime import datetime, timezone
from pathlib import Path

if __package__:
    from .apply_type_corrections import normalize_type
    from .atomic_io import atomic_write_text
    from .extract_descriptors import csharp_code_view
else:  # direct script execution; avoid ImportWarning under python -W error.
    from apply_type_corrections import normalize_type
    from atomic_io import atomic_write_text
    from extract_descriptors import csharp_code_view


TOOL_VERSION = 3
ENTRY_START_RE = re.compile(r"\bOverlayEntry\s*\{")
HANDLE_START_RE = re.compile(r"\bOverlayHandleEntry\s*\{")
GROUP_RE = re.compile(r'group_path:\s*"(?P<value>[^"]+)"')
FIELD_RE = re.compile(r'field_name:\s*"(?P<value>[^"]+)"')
HANDLE_RE = re.compile(r"handle:\s*(?P<value>\d+)")
TYPE_MARKER = "field_type:"


def run_git(repo: Path, *args: str) -> str:
    # errors="replace": git's messages follow the locale (cp949 on a Korean
    # Windows). Strict UTF-8 turned a failing call into a decode error, or on
    # Windows a None stderr, instead of git's message.
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                            text=True, encoding="utf-8", errors="replace", check=False)
    if result.returncode:
        raise ValueError(f"git {' '.join(args)} failed for {repo}: {result.stderr.strip()}")
    return result.stdout.strip()


def descriptor_directory(root: Path) -> Path:
    direct = root / "Replay.Valorant"
    nested = root / "src" / "Replay.Valorant"
    if direct.is_dir():
        return direct
    if nested.is_dir():
        return nested
    raise ValueError(f"no Replay.Valorant directory below {root}")


def entry_blocks(content: str, marker: re.Pattern[str]):
    """Yield brace-balanced struct bodies for formatted or generated Rust."""
    for match in marker.finditer(content):
        depth = 0
        for index in range(match.end() - 1, len(content)):
            character = content[index]
            if character == "{":
                depth += 1
            elif character == "}":
                depth -= 1
                if depth == 0:
                    # Keep the entry's closing brace: field_type_of needs it
                    # to distinguish the outer struct from nested FieldType
                    # braces.
                    yield content[match.end():index + 1]
                    break
        else:
            raise ValueError("unterminated overlay entry")


def field_type_of(block: str) -> str | None:
    """Read one FieldType with nested braces without regex truncation."""
    start = block.find(TYPE_MARKER)
    if start == -1:
        return None
    start += len(TYPE_MARKER)
    depth = 0
    for index, character in enumerate(block[start:], start):
        if character == "{":
            depth += 1
        elif character == "}":
            if depth == 0:
                return normalize_type(block[start:index])
            depth -= 1
    return None


def parse_table(path: Path) -> tuple[dict[tuple[str, str], str], dict[tuple[str, int], str]]:
    text = path.read_text(encoding="utf-8")
    entries: dict[tuple[str, str], str] = {}
    handles: dict[tuple[str, int], str] = {}
    for block in entry_blocks(text, ENTRY_START_RE):
        group, field, field_type = GROUP_RE.search(block), FIELD_RE.search(block), field_type_of(block)
        if not (group and field and field_type):
            raise ValueError(f"could not parse an OverlayEntry in {path}")
        key = (group["value"], field["value"])
        if key in entries:
            raise ValueError(f"duplicate overlay entry {key!r} in {path}")
        entries[key] = field_type
    for block in entry_blocks(text, HANDLE_START_RE):
        group, handle, field = GROUP_RE.search(block), HANDLE_RE.search(block), FIELD_RE.search(block)
        if not (group and handle and field):
            raise ValueError(f"could not parse an OverlayHandleEntry in {path}")
        key = (group["value"], int(handle["value"]))
        if key in handles:
            raise ValueError(f"duplicate overlay handle {key!r} in {path}")
        handles[key] = field["value"]
    if "OVERLAY_TABLE" in text and not entries:
        raise ValueError(f"could not parse overlay entries from {path}")
    return entries, handles


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def descriptor_manifest(source_dir: Path) -> tuple[str, int]:
    """Digest every source path and contents, including dirty local edits."""
    files = sorted(source_dir.rglob("*.cs"))
    digest = hashlib.sha256()
    for path in files:
        digest.update(path.relative_to(source_dir).as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(sha256_file(path).encode("ascii"))
        digest.update(b"\n")
    return digest.hexdigest(), len(files)


def extract_table(source_dir: Path, output: Path) -> tuple[dict[tuple[str, str], str], dict[tuple[str, int], str]]:
    extractor = Path(__file__).with_name("extract_descriptors.py")
    result = subprocess.run([sys.executable, str(extractor), str(source_dir), str(output)],
                            capture_output=True, text=True, encoding="utf-8", check=False)
    if result.returncode:
        raise ValueError("descriptor extraction failed for " + str(source_dir) + ":\n" + result.stderr)
    return parse_table(output)


def record_csharp_source_changes(baseline_dir: Path, candidate_dir: Path) -> list[dict[str, object]]:
    """Keep an extractor blind spot visible instead of calling it no change.

    The generator deliberately accepts only C# shapes it understands.  A newly
    added descriptor using a path constant can therefore add no overlay row;
    hashing the input files makes that fact reviewable without pretending the
    source addition was a parsed schema entry.
    """
    def files(root: Path) -> dict[str, str]:
        return {path.relative_to(root).as_posix():
                hashlib.sha256(path.read_bytes()).hexdigest()
                for path in root.rglob("*.cs")}
    old, new = files(baseline_dir), files(candidate_dir)
    changes = []
    for path in sorted(old.keys() | new.keys()):
        if old.get(path) == new.get(path):
            continue
        changes.append({"kind": "added" if path not in old else "removed" if path not in new else "modified",
                        "path": path, "disposition": "review_candidate"})
    return changes


def record_versioned_decoders(source_dir: Path) -> list[dict[str, object]]:
    """Expose version-selected custom decoders kept opaque by the overlay."""
    marker = re.compile(r'\bnew\s+VersionedDefinition\s*<\s*IFieldDecoderDescriptor\s*>')
    records: list[dict[str, object]] = []
    for path in sorted(source_dir.rglob("*.cs")):
        view = csharp_code_view(path.read_text(encoding="utf-8-sig"))
        for match in marker.finditer(view):
            records.append({
                "path": path.relative_to(source_dir).as_posix(),
                "line": view.count("\n", 0, match.start()) + 1,
                "disposition": "review_candidate",
                "reason": "version-selected custom decoder remains FieldType::Raw",
            })
    return records


def source_from_spec(spec: str, workspace: Path) -> tuple[Path, dict[str, object]]:
    if "::" not in spec:
        root = Path(spec).resolve()
        descriptor = descriptor_directory(root)
        try:
            metadata: dict[str, object] = {
                "input": spec, "kind": "directory", "resolved_path": str(root),
                "descriptor_root": str(descriptor), "commit": run_git(root, "rev-parse", "HEAD"),
                "dirty": bool(run_git(root, "status", "--porcelain=v1")),
            }
        except ValueError:
            metadata = {"input": spec, "kind": "directory", "resolved_path": str(root),
                        "descriptor_root": str(descriptor), "commit": None, "dirty": None}
        manifest, file_count = descriptor_manifest(descriptor)
        metadata.update({"csharp_manifest_sha256": manifest, "csharp_file_count": file_count})
        return descriptor, metadata

    repo_text, revision = spec.rsplit("::", 1)
    repo = Path(repo_text).resolve()
    if not revision:
        raise ValueError(f"missing revision in {spec!r}")
    commit = run_git(repo, "rev-parse", f"{revision}^{{commit}}")
    archive = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", commit],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if archive.returncode:
        raise ValueError(f"git archive failed for {spec}: "
                         f"{archive.stderr.decode(errors='replace').strip()}")
    unpacked = workspace / f"source-{len(list(workspace.iterdir()))}"
    unpacked.mkdir()
    # Unpacked in-process, not with whatever `tar` is first on PATH. On Windows
    # that is Git for Windows' GNU tar inside Git Bash, which reads the drive
    # colon in `-C C:\...` as a remote host and refuses, and System32's bsdtar
    # everywhere else -- so the same command passed from PowerShell and failed
    # from Git Bash. The `data` filter refuses absolute and escaping members.
    try:
        with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as tar:
            tar.extractall(unpacked, filter="data")
    except (tarfile.TarError, OSError) as exc:
        raise ValueError(f"could not unpack git archive: {exc}") from exc
    descriptor = descriptor_directory(unpacked)
    manifest, file_count = descriptor_manifest(descriptor)
    return descriptor, {"input": spec, "kind": "git_revision", "repository": str(repo),
                        "requested_ref": revision, "commit": commit, "dirty": None,
                        "descriptor_root": "src/Replay.Valorant (archived)",
                        "csharp_manifest_sha256": manifest, "csharp_file_count": file_count}


def record_differences(baseline: dict[tuple[str, str], str], candidate: dict[tuple[str, str], str]) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    for key in sorted(baseline.keys() | candidate.keys()):
        old, new = baseline.get(key), candidate.get(key)
        if old is None:
            kind = "added"
        elif new is None:
            kind = "removed"
        elif old != new:
            kind = "type_changed"
        else:
            continue
        records.append({"kind": kind, "group_path": key[0], "field_name": key[1],
                        "baseline_type": old, "candidate_type": new,
                        "disposition": "review_candidate"})
    return records


def record_handle_differences(baseline: dict[tuple[str, int], str], candidate: dict[tuple[str, int], str]) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    for key in sorted(baseline.keys() | candidate.keys()):
        old, new = baseline.get(key), candidate.get(key)
        if old == new:
            continue
        records.append({"kind": "added" if old is None else "removed" if new is None else "renamed",
                        "group_path": key[0], "handle": key[1], "baseline_name": old,
                        "candidate_name": new, "disposition": "review_candidate"})
    return records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="directory or REPOSITORY::REVISION")
    parser.add_argument("--candidate", required=True, help="directory or REPOSITORY::REVISION")
    parser.add_argument("--downstream-table", type=Path, help="existing vrfkit overlay to protect")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        with tempfile.TemporaryDirectory(prefix="vrfkit-descriptor-audit-") as temp:
            scratch = Path(temp)
            baseline_dir, baseline_source = source_from_spec(args.baseline, scratch)
            candidate_dir, candidate_source = source_from_spec(args.candidate, scratch)
            baseline_entries, baseline_handles = extract_table(baseline_dir, scratch / "baseline.rs")
            candidate_entries, candidate_handles = extract_table(candidate_dir, scratch / "candidate.rs")
            downstream_risks: list[dict[str, object]] = []
            downstream_metadata = None
            if args.downstream_table:
                downstream_entries, downstream_handles = parse_table(args.downstream_table)
                downstream_metadata = {"path": str(args.downstream_table.resolve()),
                                       "sha256": sha256_file(args.downstream_table)}
                for key in sorted(downstream_entries.keys() - candidate_entries.keys()):
                    downstream_risks.append({"table": "entries", "group_path": key[0], "field_name": key[1],
                        "current_type": downstream_entries[key],
                        "also_in_baseline_descriptor": key in baseline_entries,
                        "reason": "would_be_lost_by_wholesale_regeneration",
                        "disposition": "review_candidate"})
                for key in sorted(downstream_entries.keys() & candidate_entries.keys()):
                    if downstream_entries[key] != candidate_entries[key]:
                        downstream_risks.append({"table": "entries", "group_path": key[0], "field_name": key[1],
                            "current_type": downstream_entries[key], "candidate_type": candidate_entries[key],
                            "also_in_baseline_descriptor": key in baseline_entries,
                            "reason": "would_be_overwritten_by_wholesale_regeneration",
                            "disposition": "review_candidate"})
                for key in sorted(downstream_handles.keys() - candidate_handles.keys()):
                    downstream_risks.append({"table": "handles", "group_path": key[0], "handle": key[1],
                        "current_name": downstream_handles[key],
                        "also_in_baseline_descriptor": key in baseline_handles,
                        "reason": "would_be_lost_by_wholesale_regeneration",
                        "disposition": "review_candidate"})
                for key in sorted(downstream_handles.keys() & candidate_handles.keys()):
                    if downstream_handles[key] != candidate_handles[key]:
                        downstream_risks.append({"table": "handles", "group_path": key[0], "handle": key[1],
                            "current_name": downstream_handles[key], "candidate_name": candidate_handles[key],
                            "also_in_baseline_descriptor": key in baseline_handles,
                            "reason": "would_be_remapped_by_wholesale_regeneration",
                            "disposition": "review_candidate"})
            report = {"schema_version": TOOL_VERSION,
                "generated_at_utc": datetime.now(timezone.utc).isoformat(),
                "read_only": True,
                "inputs": {"baseline": args.baseline, "candidate": args.candidate,
                           "downstream_table": downstream_metadata,
                           "extractor": {"path": str(Path(__file__).with_name("extract_descriptors.py").resolve()),
                                         "sha256": sha256_file(Path(__file__).with_name("extract_descriptors.py"))}},
                "sources": {"baseline": baseline_source, "candidate": candidate_source},
                "counts": {"baseline_entries": len(baseline_entries), "candidate_entries": len(candidate_entries),
                           "baseline_handles": len(baseline_handles), "candidate_handles": len(candidate_handles)},
                "field_changes": record_differences(baseline_entries, candidate_entries),
                "handle_changes": record_handle_differences(baseline_handles, candidate_handles),
                "csharp_source_changes": record_csharp_source_changes(baseline_dir, candidate_dir),
                "versioned_custom_decoders": record_versioned_decoders(candidate_dir),
                "downstream_regeneration_risks": downstream_risks,
                "note": "All records are review candidates. Version-selected custom decoders stay Raw; this audit never approves or regenerates an overlay."}
            atomic_write_text(args.output, json.dumps(report, indent=2, sort_keys=True) + "\n")
            print(f"wrote {args.output}: {len(report['field_changes'])} field changes, "
                  f"{len(report['handle_changes'])} handle changes, {len(downstream_risks)} downstream risks, "
                  f"{len(report['versioned_custom_decoders'])} versioned custom decoders")
    except ValueError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
