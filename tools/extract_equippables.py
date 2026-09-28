#!/usr/bin/env python3
"""Generate tools/equippable_table.py from the C# parser's resolver.

Weapon display names ("Vandal", "Sheriff") exist nowhere in the replay wire
format. The game ships them as client-side assets; the C# reference parser
carries a hand-maintained table in

    ValorantReplayParser/src/Replay.Valorant/Combat/ValorantEquippableResolver.cs

as a list of Define(classPath, name, category) entries. That file is vendored
with the rest of the descriptor sources, at
third_party/vrp/Replay.Valorant/Combat/, and is the default input. Reproducing
shot.equippable.name therefore requires a table, and this generator extracts it
from that authoritative source rather than having anyone retype the paths.

This does NOT weaken the parser's "no hardcoded names" invariant: the Rust
crates stay free of name tables and emit class_path only. The mapping lives on
the presentation side, in the Python adapter, where turning
"AssaultRifle_AK" into "Vandal" is a labelling concern rather than a parsing
rule. See docs/archive/PROJECT_STATUS.md section 8 and
docs/archive/NEXT_STEPS_FINDINGS.md.

The names are the C# table's, not the game's, and one of them differs.
Compared with the installed 13.06 game's own display names (string tables
read statically, 2026-09-28): 21 of the 25 match, "Spike" and "Tour de Force"
differ only in case ("SPIKE", "Tour De Force"), Equippable_Unarmed has no
equippable data asset, and CompactPistol_C is "Bandit" (string-table key
CompactPistol_DisplayName), not "Compact Pistol". The table is deliberately
not overridden here:

- A hand edit of the output fails --check, and editing the vendored resolver
  breaks its byte-for-byte provenance (third_party/vrp/README.md).
- The only consumer here, tools/to_valplay_bundle.py, emits the name as
  shot.equippable.name to match the C# parser's bundle, and valplay prices
  weapons by that name. valplay leaves "Compact Pistol" unpriced on purpose
  and lists "Bandit" at 600 as unverified, so emitting "Bandit" would give
  the gun that price there. valplay's tests pin the literal strings, not
  this table, so they would stay green. Renaming it is valplay's decision,
  made together with the price.

Usage:
    python tools/extract_equippables.py [--csharp-root <path>] [--check]

--csharp-root takes the vendored root (third_party/vrp, the default) or an
upstream clone's root. --check exits non-zero if the generated file is stale;
CI runs it.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

# The input is vendored (third_party/vrp/README.md). It used to default to
# VRFKIT_CSHARP_DIR, which no one set, so a plain run -- and the --check this
# docstring advertised for CI -- stopped at "resolver not found".
DEFAULT_CSHARP_ROOT = Path(__file__).resolve().parent.parent / "third_party" / "vrp"
RESOLVER_RELPATH = Path("Combat/ValorantEquippableResolver.cs")  # below Replay.Valorant
# Written into the generated header. Fixed rather than taken from the input, so
# --check agrees for any root holding the same resolver.
SOURCE_LABEL = "third_party/vrp/Replay.Valorant/Combat/ValorantEquippableResolver.cs"
OUTPUT_PATH = Path(__file__).parent / "equippable_table.py"

# Define("<class path>", "<display name>", ValorantEquippableCategory.<Category>)
DEFINE_RE = re.compile(
    r'Define\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*'
    r"ValorantEquippableCategory\.(\w+)\s*\)"
)

# The game renamed this one asset directory between the 13.01 and 13.02
# recordings. Both spellings occur in the corpus and identify the same class;
# keep the equivalence exact so an unrelated path's casing is never guessed.
PATH_VARIANTS = (
    (
        "/Game/Equippables/Guns/SniperRifles/Dmr/DMR.DMR_C",
        "/Game/Equippables/Guns/SniperRifles/DMR/DMR.DMR_C",
    ),
)


def pascal_to_snake(name: str) -> str:
    """SniperRifle -> sniper_rifle, Smg -> smg.

    Matches the category strings the C# JSON writer emits, verified against
    02d4d478's reference bundle: machine_gun, sniper_rifle, sidearm, smg,
    rifle, shotgun, ability.
    """
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def parse_definitions(source: str) -> list[tuple[str, str, str]]:
    """Extract (class_path, display_name, category) triples in source order."""
    return [(class_path, display_name, pascal_to_snake(category))
            for class_path, display_name, category in DEFINE_RE.findall(source)]


def path_aliases(class_path: str) -> tuple[str, ...]:
    """Return the other measured spellings of one source-defined path."""
    for variants in PATH_VARIANTS:
        if class_path in variants:
            return tuple(path for path in variants if path != class_path)
    return ()


def render(definitions: list[tuple[str, str, str]], source_rel: str) -> str:
    """Render the generated Python module."""
    lines = [
        '"""Equippable class path -> display name and category.',
        "",
        "GENERATED FILE -- DO NOT EDIT BY HAND.",
        "Regenerate with: python tools/extract_equippables.py",
        f"Source: {source_rel}",
        "",
        "Keys cover the three path shapes that appear in replay data, mirroring",
        "the C# CreateDefinitions(): the full 'Package.Class_C' path, the package",
        "path alone, and the 'Default__Class_C' archetype form. Measured, exact",
        "path aliases cover known game asset renames without case-folding keys.",
        '"""',
        "",
        "# fmt: off",
        "",
        "EQUIPPABLE_DEFINITIONS = [",
    ]
    for class_path, name, category in definitions:
        lines.append(f"    ({class_path!r}, {name!r}, {category!r}),")
    lines += [
        "]",
        "",
        "EQUIPPABLE_PATH_ALIASES = {",
    ]
    for class_path, _, _ in definitions:
        aliases = path_aliases(class_path)
        if aliases:
            lines.append(f"    {class_path!r}: {aliases!r},")
    lines += [
        "}",
        "",
        "",
        "def _build_lookup():",
        '    """Known path shapes -> (name, category, canonical source path)."""',
        "    out = {}",
        "    for class_path, name, category in EQUIPPABLE_DEFINITIONS:",
        "        value = (name, category, class_path)",
        "        out[class_path] = value",
        "        if '.' in class_path:",
        "            package, _, class_name = class_path.rpartition('.')",
        "            out[package] = value",
        "            out['Default__' + class_name] = value",
        "        for alias in EQUIPPABLE_PATH_ALIASES.get(class_path, ()):",
        "            out[alias] = value",
        "            if '.' in alias:",
        "                package, _, _ = alias.rpartition('.')",
        "                out[package] = value",
        "    return out",
        "",
        "",
        "EQUIPPABLE_BY_PATH = _build_lookup()",
        "",
        "# fmt: on",
        "",
    ]
    return "\n".join(lines)


def find_resolver(root: Path) -> Path | None:
    """The resolver below a vendored root or an upstream clone's root.

    The vendored copy keeps Replay.Valorant directly under its root; upstream
    has it under src/. Same two layouts compare_descriptor_sources.py accepts.
    """
    for descriptors in (root / "Replay.Valorant", root / "src" / "Replay.Valorant"):
        if (descriptors / RESOLVER_RELPATH).is_file():
            return descriptors / RESOLVER_RELPATH
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csharp-root", type=Path, default=DEFAULT_CSHARP_ROOT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if the generated file is stale",
    )
    args = parser.parse_args()

    resolver = find_resolver(args.csharp_root)
    if resolver is None:
        print(f"resolver not found below {args.csharp_root}: looked for "
              f"Replay.Valorant/{RESOLVER_RELPATH.as_posix()} and "
              f"src/Replay.Valorant/{RESOLVER_RELPATH.as_posix()}", file=sys.stderr)
        return 2

    definitions = parse_definitions(resolver.read_text(encoding="utf-8"))
    if not definitions:
        print(f"no Define(...) entries matched in {resolver}", file=sys.stderr)
        return 2

    rendered = render(definitions, SOURCE_LABEL)

    if args.check:
        if not OUTPUT_PATH.exists():
            print(f"{OUTPUT_PATH} missing", file=sys.stderr)
            return 1
        if OUTPUT_PATH.read_text(encoding="utf-8") != rendered:
            print(f"{OUTPUT_PATH} is stale -- rerun the generator", file=sys.stderr)
            return 1
        print(f"{OUTPUT_PATH.name} up to date ({len(definitions)} definitions)")
        return 0

    atomic_write_text(OUTPUT_PATH, rendered)
    print(f"wrote {OUTPUT_PATH} ({len(definitions)} definitions)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
