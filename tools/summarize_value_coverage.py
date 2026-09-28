"""Count physical typed rows, with optional reviewed semantic-evidence rows.

Inputs are export directories or parents whose direct children are exports.
Read-only: the JSON report goes to stdout, and no export is modified. A typed
row has at least one non-null value_i64/f64/bool/str, including 0, False and
empty strings. Multiple populated columns still count as one typed row and
are reported separately. This is not a percentage of game facts understood.
``--semantic-evidence`` accepts an explicit reviewed-evidence catalog; it
never promotes a field name or typed value to a semantic verdict on its own.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
import math
from pathlib import Path
import re
import sys

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .export_scan import discover_exports, skipped_report
else:
    from export_scan import discover_exports, skipped_report

VALUE_COLUMNS = ("value_i64", "value_f64", "value_bool", "value_str")
TABLES = ("fields", "checkpoint_fields")
SEMANTIC_STATUSES = {"reviewed", "unknown", "unsupported"}
FIELD_PATH_TEMPLATE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:\[\])?(?:\.[A-Za-z_][A-Za-z0-9_]*(?:\[\])?)*")


class EvidenceError(ValueError):
    """An evidence catalog is malformed or cannot be checked against an export."""


def count_table(path: Path) -> dict[str, int]:
    with path.open("rb") as source:
        return _count_parquet(pq.ParquetFile(source), path.name)


def _count_parquet(parquet: pq.ParquetFile, filename: str) -> dict[str, int]:
    missing = set(VALUE_COLUMNS) - set(parquet.schema_arrow.names)
    if missing:
        raise ValueError(f"{filename}: missing value columns {sorted(missing)}")
    rows = typed = multi = 0
    for batch in parquet.iter_batches(batch_size=65536, columns=list(VALUE_COLUMNS), use_threads=False):
        populated = pc.cast(pc.is_valid(batch.column(0)), pa.uint8())
        for index in range(1, len(VALUE_COLUMNS)):
            populated = pc.add(populated, pc.cast(pc.is_valid(batch.column(index)), pa.uint8()))
        rows += batch.num_rows
        typed += pc.sum(pc.greater(populated, 0)).as_py() or 0
        multi += pc.sum(pc.greater(populated, 1)).as_py() or 0
    if rows != parquet.metadata.num_rows:
        raise ValueError(f"{filename}: scanned rows disagree with footer")
    return {"rows": rows, "typed_rows": typed, "untyped_rows": rows - typed, "multi_value_rows": multi}


#: Export directories named directly, or the direct child exports of a parent.
discover = discover_exports


def count_export(directory: Path) -> dict[str, dict[str, int]]:
    result = {"fields": count_table(directory / "fields.parquet")}
    checkpoint = directory / "checkpoint_fields.parquet"
    if checkpoint.exists():
        result["checkpoint_fields"] = count_table(checkpoint)
    return result


def _require_string(value: object, location: str) -> str:
    if not isinstance(value, str) or not value:
        raise EvidenceError(f"{location} must be a non-empty string")
    return value


def load_semantic_evidence(path: Path) -> dict:
    """Read an explicit catalog of reviewed semantic assertions.

    Each source records provenance; each reviewed claim selects rows by exact
    JSON-scalar equality or a schema-2 literal indexed path template. Unknown
    and unsupported claims remain visible but are never counted as semantic
    verification.
    """
    try:
        raw_document = path.read_bytes()
        document = json.loads(raw_document)
    except (OSError, json.JSONDecodeError) as exc:
        raise EvidenceError(f"cannot read semantic evidence {path}: {exc}") from exc
    if not isinstance(document, dict) or document.get("schema_version") not in (1, 2):
        raise EvidenceError("semantic evidence schema_version must be 1 or 2")
    schema_version = document["schema_version"]
    _require_string(document.get("catalog_version"), "catalog_version")
    sources = document.get("sources")
    claims = document.get("claims")
    if not isinstance(sources, list) or not isinstance(claims, list):
        raise EvidenceError("semantic evidence sources and claims must be lists")
    source_ids: set[str] = set()
    for index, source in enumerate(sources):
        location = f"sources[{index}]"
        if not isinstance(source, dict):
            raise EvidenceError(f"{location} must be an object")
        source_id = _require_string(source.get("id"), f"{location}.id")
        if source_id in source_ids:
            raise EvidenceError(f"duplicate semantic evidence source id {source_id!r}")
        source_ids.add(source_id)
        _require_string(source.get("version"), f"{location}.version")
        if not isinstance(source.get("scope"), dict) or not source["scope"]:
            raise EvidenceError(f"{location}.scope must be a non-empty object")
    claim_ids: set[str] = set()
    for index, claim in enumerate(claims):
        location = f"claims[{index}]"
        if not isinstance(claim, dict):
            raise EvidenceError(f"{location} must be an object")
        claim_id = _require_string(claim.get("id"), f"{location}.id")
        if claim_id in claim_ids:
            raise EvidenceError(f"duplicate semantic evidence claim id {claim_id!r}")
        claim_ids.add(claim_id)
        if claim.get("source_id") not in source_ids:
            raise EvidenceError(f"{location}.source_id does not name a source")
        if claim.get("table") not in TABLES:
            raise EvidenceError(f"{location}.table must be one of {TABLES}")
        status = claim.get("evidence_status")
        if status not in SEMANTIC_STATUSES:
            raise EvidenceError(f"{location}.evidence_status must be reviewed, unknown, or unsupported")
        template = claim.get("field_path_template")
        if template is not None:
            if schema_version == 1:
                raise EvidenceError(f"{location}.field_path_template requires schema_version 2")
            _require_string(template, f"{location}.field_path_template")
            if not FIELD_PATH_TEMPLATE.fullmatch(template) or "[]" not in template:
                raise EvidenceError(f"{location}.field_path_template must be literal dot segments with at least one []")
        criteria = claim.get("criteria")
        if not isinstance(criteria, dict) or not criteria:
            raise EvidenceError(f"{location}.criteria must be a non-empty object")
        for field, value in criteria.items():
            if not isinstance(field, str) or not field:
                raise EvidenceError(f"{location}.criteria keys must be non-empty strings")
            if (value is not None and not isinstance(value, (str, int, float, bool))) or (
                    isinstance(value, float) and not math.isfinite(value)):
                raise EvidenceError(f"{location}.criteria.{field} must be a JSON scalar")
        if status == "reviewed":
            _require_string(claim.get("semantic_label"), f"{location}.semantic_label")
            _require_string(claim.get("reviewed_at"), f"{location}.reviewed_at")
            _require_string(claim.get("evidence"), f"{location}.evidence")
            if not isinstance(criteria.get("group_path"), str) or not criteria["group_path"]:
                raise EvidenceError(f"{location}.criteria must include exact group_path")
            has_field_name = "field_name" in criteria
            if has_field_name == (template is not None):
                raise EvidenceError(f"{location} must select exactly one of criteria.field_name or field_path_template")
            if has_field_name:
                _require_string(criteria["field_name"], f"{location}.criteria.field_name")
            applicability = claim.get("applicability")
            if not isinstance(applicability, dict):
                raise EvidenceError(f"{location}.applicability must enforce export_ids and/or replay_builds")
            export_ids = applicability.get("export_ids")
            builds = applicability.get("replay_builds")
            if export_ids is not None and (not isinstance(export_ids, list) or not export_ids or
                                           not all(isinstance(item, str) and item for item in export_ids)):
                raise EvidenceError(f"{location}.applicability.export_ids must be a non-empty string list")
            if builds is not None and (not isinstance(builds, list) or not builds or
                                       not all(isinstance(item, str) and item for item in builds)):
                raise EvidenceError(f"{location}.applicability.replay_builds must be a non-empty string list")
            if export_ids is None and builds is None:
                raise EvidenceError(f"{location}.applicability must enforce export_ids and/or replay_builds")
    document["_catalog_sha256"] = hashlib.sha256(raw_document).hexdigest()
    return document


def _criterion_mask(column: pa.Array, expected: object) -> pa.Array:
    mask = pc.is_null(column) if expected is None else pc.equal(column, pa.scalar(expected))
    return pc.fill_null(mask, False)


def _field_path_mask(column: pa.Array, template: str) -> pa.Array:
    """Match a whole indexed path; template names are always literal."""
    pieces = []
    for segment in template.split("."):
        indexed = segment.endswith("[]")
        pieces.append(re.escape(segment[:-2] if indexed else segment) + (r"\[[0-9]+\]" if indexed else ""))
    # Real exports dictionary-encode names. The string kernel requires their
    # values rather than dictionary indices; \z excludes trailing newlines.
    return pc.fill_null(pc.match_substring_regex(
        pc.cast(column, pa.string()), r"\A" + r"\.".join(pieces) + r"\z"), False)


def applicable_claims(directory: Path, claims: list[dict]) -> list[dict]:
    """Select claims whose enforceable applicability includes this export."""
    reviewed = [claim for claim in claims if claim["evidence_status"] == "reviewed"]
    if not reviewed:
        return claims
    needs_build = any("replay_builds" in claim["applicability"] for claim in reviewed)
    build = None
    if needs_build:
        try:
            manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
            build = manifest["replay_build"]
        except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
            raise EvidenceError(f"cannot enforce replay_build applicability: {exc}") from exc
        if not isinstance(build, str) or not build:
            raise EvidenceError("cannot enforce replay_build applicability: manifest replay_build is absent")
    selected = []
    for claim in claims:
        if claim["evidence_status"] != "reviewed":
            selected.append(claim)
            continue
        scope = claim["applicability"]
        export_match = "export_ids" not in scope or directory.name in scope["export_ids"]
        build_match = "replay_builds" not in scope or build in scope["replay_builds"]
        if export_match and build_match:
            selected.append(claim)
    return selected


def count_semantic_table(path: Path, claims: list[dict]) -> dict[str, object]:
    """Count the union of rows covered by explicit reviewed criteria only."""
    reviewed = [claim for claim in claims if claim["evidence_status"] == "reviewed"]
    with path.open("rb") as source:
        parquet = pq.ParquetFile(source)
        available = set(parquet.schema_arrow.names)
        for claim in claims:
            missing = set(claim["criteria"]) - available
            if missing:
                raise EvidenceError(f"{path.name}: claim {claim['id']!r} criteria fields absent: {sorted(missing)}")
        if not reviewed:
            return {"reviewed_rows": 0, "reviewed_typed_rows": 0, "claims": []}
        needed = set(VALUE_COLUMNS)
        for claim in reviewed:
            needed.update(claim["criteria"])
            if "field_path_template" in claim:
                needed.add("field_name")
        columns = sorted(needed)
        per_claim = {claim["id"]: 0 for claim in reviewed}
        reviewed_rows = reviewed_typed_rows = 0
        for batch in parquet.iter_batches(batch_size=65536, columns=columns, use_threads=False):
            by_name = {name: batch.column(index) for index, name in enumerate(columns)}
            union = pa.array([False] * batch.num_rows)
            for claim in reviewed:
                matches = pa.array([True] * batch.num_rows)
                for field, expected in claim["criteria"].items():
                    matches = pc.fill_null(pc.and_(matches, _criterion_mask(by_name[field], expected)), False)
                if "field_path_template" in claim:
                    matches = pc.fill_null(pc.and_(matches, _field_path_mask(
                        by_name["field_name"], claim["field_path_template"])), False)
                per_claim[claim["id"]] += pc.sum(pc.cast(matches, pa.int64())).as_py() or 0
                union = pc.fill_null(pc.or_(union, matches), False)
            populated = pc.cast(pc.is_valid(by_name[VALUE_COLUMNS[0]]), pa.uint8())
            for field in VALUE_COLUMNS[1:]:
                populated = pc.add(populated, pc.cast(pc.is_valid(by_name[field]), pa.uint8()))
            reviewed_rows += pc.sum(pc.cast(union, pa.int64())).as_py() or 0
            reviewed_typed_rows += pc.sum(pc.cast(pc.and_(union, pc.greater(populated, 0)), pa.int64())).as_py() or 0
    return {"reviewed_rows": reviewed_rows, "reviewed_typed_rows": reviewed_typed_rows,
            "claims": [{"id": claim["id"], "rows": per_claim[claim["id"]]} for claim in reviewed]}


def summarize_semantic_evidence(exports: list[Path], catalog: dict) -> dict:
    tables: dict[str, dict] = {}
    errors = []
    for table in TABLES:
        claims = [claim for claim in catalog["claims"] if claim["table"] == table]
        total = {"exports_with_table": 0, "reviewed_rows": 0, "reviewed_typed_rows": 0,
                 "applicable_exports": 0,
                 "reviewed_claim_count": sum(c["evidence_status"] == "reviewed" for c in claims),
                 "unknown_or_unsupported_claim_count": sum(c["evidence_status"] != "reviewed" for c in claims),
                 "claims": {claim["id"]: 0 for claim in claims if claim["evidence_status"] == "reviewed"}}
        for directory in exports:
            path = directory / f"{table}.parquet"
            if not path.exists():
                continue
            try:
                selected = applicable_claims(directory, claims)
                counts = count_semantic_table(path, selected)
            except (OSError, ValueError, pa.ArrowException) as exc:
                errors.append({"export": str(directory), "table": table, "error": str(exc)})
                continue
            total["exports_with_table"] += 1
            total["applicable_exports"] += bool(any(c["evidence_status"] == "reviewed" for c in selected))
            total["reviewed_rows"] += counts["reviewed_rows"]
            total["reviewed_typed_rows"] += counts["reviewed_typed_rows"]
            for claim in counts["claims"]:
                total["claims"][claim["id"]] += claim["rows"]
        total["claims"] = [{"id": claim_id, "rows": rows} for claim_id, rows in sorted(total["claims"].items())]
        tables[table] = total
    return {"catalog_version": catalog["catalog_version"], "catalog_sha256": catalog["_catalog_sha256"],
            "source_count": len(catalog["sources"]), "sources": catalog["sources"], "claims": catalog["claims"],
            "denominator": "only physical rows matching reviewed, explicit catalog criteria; this is not semantic completeness",
            "tables": tables, "errors": errors}


def summarize(exports: list[Path], jobs: int, semantic_evidence: dict | None = None) -> dict:
    if jobs < 1:
        raise ValueError("jobs must be positive")
    totals = {name: {"exports_with_table": 0, "rows": 0, "typed_rows": 0,
                     "untyped_rows": 0, "multi_value_rows": 0} for name in TABLES}
    errors = []
    successful = 0
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        pending = {pool.submit(count_export, p): p for p in exports}
        for future in as_completed(pending):
            try:
                measured = future.result()
            except (OSError, ValueError, pa.ArrowException) as exc:
                errors.append({"export": str(pending[future]), "error": str(exc)})
                continue
            successful += 1
            for name, counts in measured.items():
                totals[name]["exports_with_table"] += 1
                for key, value in counts.items():
                    totals[name][key] += value
    for counts in totals.values():
        counts["typed_fraction"] = counts["typed_rows"] / counts["rows"] if counts["rows"] else None
    report = {"schema_version": 1, "complete": not errors, "export_count": len(exports), "successful_exports": successful,
              "denominator": "physical field rows in successfully read export directories",
              "interpretation": "non-null typed values, not semantic verification, raw preservation, or framing coverage",
              "tables": totals, "errors": sorted(errors, key=lambda e: e["export"])}
    if semantic_evidence is not None:
        semantic_report = summarize_semantic_evidence(exports, semantic_evidence)
        report["semantic_evidence"] = semantic_report
        if semantic_report["errors"]:
            report["complete"] = False
            report["errors"].extend(semantic_report["errors"])
            report["errors"].sort(key=lambda e: (e["export"], e.get("table", "")))
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", type=Path, nargs="+")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--semantic-evidence", type=Path, help="opt-in reviewed semantic-evidence catalog JSON")
    args = parser.parse_args(argv)
    try:
        evidence = load_semantic_evidence(args.semantic_evidence) if args.semantic_evidence else None
        skipped: list[Path] = []
        report = summarize(discover(args.inputs, skipped), args.jobs, evidence)
        report["skipped_generated_dirs"] = skipped_report(skipped)
    except ValueError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
