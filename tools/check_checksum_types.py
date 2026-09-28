#!/usr/bin/env python3
"""Check vrfkit's overlay types against the replay's own `compatible_checksum`.

Every `NetFieldExport` a replay declares carries a `compatible_checksum`, and
Unreal computes it from the property's NAME and its C++ TYPE
(`GetRepLayoutCmdCompatibleChecksum` in UE 5.3 `RepLayout.cpp`):

    crc = StrCrc32(lower(name), parent)       # every TCHAR fed as 4 bytes, LE
    crc = StrCrc32(lower(cpp_type), crc)      # e.g. "int32", "fvector", "aactor*"
    crc = MemCrc32(<u32 LE static index>, crc)

`StrCrc32` and `MemCrc32` are the standard reflected CRC-32 (zlib's), so this
is `zlib.crc32` over the UTF-32LE bytes of the lower-cased strings, then the
little-endian static-array index. `parent` is 0 for a class property or an
RPC parameter; a member of a flattened (non-NetSerialize) struct continues
from its struct property's checksum, and a `TArray`'s inner element from the
array's. The formula was measured against 13.06 replays by the game-file
analysis of 2026-09-28 (194 Blueprint fields and several native identities
reproduce); this tool re-implements it and reports, per build, how many
declared identities it reproduces, which is what carries it to other builds.

So a checksum is evidence about a TYPE that no descriptor, table or decoder
in this repo contributed. This tool turns it into a check of the overlay:

 1. Collect every declared identity -- `(group, field name, handle,
    checksum)` from `manifest.json`'s `net_field_export_groups` and, when
    present, `checkpoint_export_groups/fields.parquet` -- of one or more
    exports.
 2. Resolve the `FieldType` vrfkit gives it, by parsing the generated tables
    (`table.rs`, `scoped_types.rs`, `checksum_table.rs`) and the resolution
    constants in `overlay.rs`, and following `overlay::resolve_entry`'s order:
    name, `b`-prefixed name, handle (refused when the wire names something
    else), the same three against the aliased class, exact
    `(name, group, checksum)` scoped types, the engine object references,
    and last the checksum table. Validated 2026-09-28: 12,937 of 12,937
    distinct corpus identities resolve to the same `FieldType` as the Rust
    `resolve_field_type_with_checksum`, and on 7 exports (11.06-13.06, main
    and checkpoint) the identities this calls typed are exactly the ones
    whose rows carry a `value_*` (see docs/CHECKSUM_TYPES.md).
 3. Map the `FieldType` to the C++ spellings it can stand for (`CPP_TYPES`)
    and recompute the checksum under every KNOWN parent seed: 0, and the
    struct chains in `PARENT_CHAINS` -- short name-level facts, each with its
    provenance.

Each typed identity lands in exactly one of three buckets:

    match       vrfkit's type reproduces the declared checksum
    mismatch    vrfkit's type does not, but another C++ spelling in
                `ALTERNATIVE_TYPES` (or an object pointer) does, under the
                same known seeds -- the checksum names a different type
    untestable  nothing reproduces it, so the checksum says nothing here

`untestable` is never a match. The reasons are counted separately:

  * enum-capable types (`Byte`, `EnumByte`, `EnumRemainingBits`, a
    `SerializedInt` whose bound is not a whole unsigned width). A C++ enum
    spelling (`TEnumAsByte<E>`, `E`, `E::Type`) has never been seen to
    reproduce, so an enum property is untestable; a plain `uint8` still
    counts, and an alternative that reproduces is still a mismatch.
  * object references whose class is not among the candidates. The C++
    type is `A<Class>*` / `U<Class>*`; candidates are the classes the input
    itself declares groups for plus `ENGINE_CLASSES`, so a reference to an
    undeclared class (a data asset, say) cannot be spelled.
  * members of a flattened struct or array whose parent chain is not in
    `PARENT_CHAINS`: the parent seed is unknown, so no spelling reproduces.
  * a bare FName index (`"108"`) not in `HARDCODED_FNAMES`, a non-ASCII name,
    or a declared checksum of 0.

What it cannot tell apart, by construction:

  * `Bool` reproduced as `uint8` is a bitfield bool (`uint8 bFoo:1`) or a
    byte; the checksum names the storage type, and only the wire width
    separates the two. Counted on its own line.
  * A match says the name and C++ type are what the declaration hashed. It
    says nothing about the wire FORMAT a NetSerialize type uses, a
    quantization scale the C++ type does not name, or the meaning of a
    value -- `decode errors: 0` and the value checks remain the evidence
    for those.
  * Groups it never saw. Coverage is the input's; run it on the corpus.

It also checks every `checksum_table.rs` entry the same way: the table has no
names, so each entry is recomputed under every name that declares its checksum
in the input. Entries no input declaration carries are counted, not failed.

Counters are printed with their zeros. Exit status: 0 when nothing
mismatches, 1 when anything does or nothing was checked, 2 when an input or
a generated table cannot be read completely (an entry that does not parse,
or a `FieldType` variant `CPP_TYPES` does not classify).

Usage:
    python tools/check_checksum_types.py --export out/probe [--export ...]
    python tools/check_checksum_types.py --corpus DIR   # DIR/<export>/manifest.json
    python tools/check_checksum_types.py --corpus DIR --json report.json
"""
from __future__ import annotations

import argparse
import collections
import json
import re
import struct
import sys
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import NamedTuple

if __package__:
    from .export_scan import is_generated_sibling
else:  # direct script execution
    from export_scan import is_generated_sibling

REPO = Path(__file__).resolve().parents[1]
DECODE_SRC = REPO / "crates" / "vrf-decode" / "src"
TABLE_RS = DECODE_SRC / "table.rs"
SCOPED_RS = DECODE_SRC / "scoped_types.rs"
CHECKSUM_RS = DECODE_SRC / "checksum_table.rs"
OVERLAY_RS = DECODE_SRC / "overlay.rs"
DECODE_RS = DECODE_SRC / "decode.rs"

CLASS_NET_CACHE = "_ClassNetCache"

# --------------------------------------------------------------------------
# The formula


def compatible_checksum(name: str, cpp_type: str, static_index: int = 0,
                        parent: int = 0) -> int:
    """UE 5.3 `GetRepLayoutCmdCompatibleChecksum` for one property.

    `parent` is the checksum the property's command continues from: 0 at the
    top of a class or an RPC's parameters, the struct property's checksum for
    a member of a flattened struct, the array's for a `TArray`'s inner
    element. ASCII only: `ToLower` and the TCHAR width both differ from
    Python's outside ASCII, so a non-ASCII input raises instead of hashing to
    a plausible wrong value.
    """
    if not (name.isascii() and cpp_type.isascii()):
        raise ValueError(f"non-ASCII input: {name!r} {cpp_type!r}")
    crc = zlib.crc32(name.lower().encode("utf-32-le"), parent)
    crc = zlib.crc32(cpp_type.lower().encode("utf-32-le"), crc)
    return zlib.crc32(struct.pack("<I", static_index), crc)


def chain_checksum(links) -> int:
    """The checksum at the end of `(name, cpp_type)` links, outermost first.

    That is the parent seed of the last link's members: a struct's members
    continue from the struct property, an array's element from the array.
    """
    crc = 0
    for name, cpp_type in links:
        crc = compatible_checksum(name, cpp_type, 0, crc)
    return crc


# --------------------------------------------------------------------------
# Name-level facts. Nothing below is a dump: each entry is a name and a type,
# with where it came from, and the tool prints how many declared checksums
# each one reproduces so a fact that stops being true is visible.

#: Hardcoded Unreal FNames the replay writes as a bare index (`"249"`). From
#: UE 5.3 `UnrealNames.inl`; the indices were read back from the 13.06
#: executable's name table by the 2026-09-28 game-file analysis. 248 and 253
#: are also confirmed by the replay: `Location` reproduces as `FVector` at
#: the top of the effect RPCs, `ID` as `int32` in `GroundVolumeFragment`.
HARDCODED_FNAMES = {
    "58": "Name", "59": "Vector", "100": "Object", "102": "Actor",
    "215": "Role", "216": "RemoteRole", "241": "Team", "248": "Location",
    "249": "Rotation", "253": "ID",
}


class Chain(NamedTuple):
    """A path of struct/array properties from the top of a class or an RPC's
    parameters; its members continue from `chain_checksum(links)`."""
    links: tuple
    source: str


PARENT_CHAINS = (
    Chain((("Transform", "FTransform"),),
          "UE 5.3 FTransform {Rotation: FQuat, Translation, Scale3D: FVector}; the "
          "property/parameter name from exe reflection 13.06: "
          "UEffectManagerComponent:MulticastPlay{Continuous,OneShot}Effect, "
          "AAresEquippable:MulticastPlay*EffectFromClient, UTransformTransitionContext"),
    Chain((("SpawnTransform", "FTransform"),),
          "exe reflection 13.06: AAresGameStateBase:MulticastResetForRespawn"),
    Chain((("AttachmentReplication", "FRepAttachment"),),
          "UE 5.3 AActor::AttachmentReplication (FRepAttachment)"),
    Chain((("Handle", "FForceModuleHandle"),),
          "exe reflection 13.06: UForceModuleManagerComponent:NetMulticast"
          "{Apply,Remove}ForceModule; FForceModuleHandle {HandleNumber: uint32, "
          "ModuleType: enum}"),
    Chain((("TimeStamp", "FNetworkedMovementTimestamp"),),
          "exe reflection 13.06: UForceModuleManagerComponent:NetMulticastApplyForceModule"),
    Chain((("AuthServerCorrectRepVariables", "FInventoryServerCorrectRepVariables"),),
          "exe reflection 13.06: UAresInventory"),
    Chain((("EffectID", "FEffectID"),),
          "exe reflection 13.06: the EffectID parameter of the UEffectManagerComponent "
          "and ULocationalEffectManagerComponent effect RPCs; FEffectID {EffectID: "
          "int64, SourceID: FName, bLocalEffect, bTransient: bool}"),
    Chain((("CurrentEffectID", "FEffectID"),),
          "exe reflection 13.06: UReplayEffectComponent:ReplayPlayContinuousEffectAtLocation"),
    Chain((("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo")),
          "exe reflection 13.06: UEffectManagerComponent.ServerActiveEffects"),
    Chain((("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo"),
           ("EffectID", "FEffectID")),
          "exe reflection 13.06: FActiveEffectInfo.EffectID"),
    Chain((("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo"),
           ("Transform", "FTransform")),
          "exe reflection 13.06: FActiveEffectInfo.Transform"),
    Chain((("AuthBlindManagerState", "FBlindManagerState"),),
          "exe reflection 13.06: UBlindManagerComponent.AuthBlindManagerState"),
    Chain((("AuthBlindManagerState", "FBlindManagerState"), ("ActiveBlinds", "TArray"),
           ("ActiveBlinds", "FActiveBlind")),
          "exe reflection 13.06: FBlindManagerState.ActiveBlinds (TArray<FActiveBlind>)"),
    Chain((("AuthBlindManagerState", "FBlindManagerState"), ("ActiveBlinds", "TArray"),
           ("ActiveBlinds", "FActiveBlind"), ("BlindEffectID", "FEffectID")),
          "exe reflection 13.06: FActiveBlind.BlindEffectID"),
    Chain((("FragmentInfo", "FGroundVolumeFragmentArray"), ("Items", "TArray"),
           ("Items", "FGroundVolumeFragment")),
          "exe reflection 13.06: UGroundVolumeComponent.FragmentInfo.Items"),
    Chain((("FragmentInfo", "FGroundVolumeFragmentArray"), ("Items", "TArray"),
           ("Items", "FGroundVolumeFragment"), ("GridPos", "FIntPoint")),
          "exe reflection 13.06: FGroundVolumeFragment.GridPos"),
)

#: Engine classes an object reference commonly points at. The candidates for
#: `A<Class>*` / `U<Class>*` are these plus every class the input declares a
#: group for; public UE 5.3 names, nothing game-specific.
ENGINE_CLASSES = (
    "Object", "Class", "Actor", "Pawn", "Character", "Controller",
    "PlayerController", "AIController", "PlayerState", "GameStateBase",
    "GameState", "Info", "HUD", "PlayerCameraManager", "WorldSettings",
    "ActorComponent", "SceneComponent", "PrimitiveComponent", "MeshComponent",
    "StaticMeshComponent", "SkeletalMeshComponent", "MovementComponent",
    "CharacterMovementComponent", "DamageType", "DataAsset",
    "PrimaryDataAsset", "StaticMesh", "SkeletalMesh", "Texture2D",
    "MaterialInterface", "SoundBase", "AnimMontage", "CurveFloat",
)

# --------------------------------------------------------------------------
# FieldType -> C++ spellings


@dataclass(frozen=True)
class FieldTypeSpec:
    """How to test one `FieldType` variant.

    `expected` are the C++ spellings a value of this type can have. When none
    reproduces, `unreproduced` is the untestable reason -- unless an
    alternative spelling reproduces instead, which is a mismatch.
    """
    expected: tuple = ()
    unreproduced: str = "not reproduced under any known seed"
    objects: bool = False
    #: Whether another spelling reproducing it contradicts it. Not for a type
    #: with no spelling of its own: its C++ type is unknown, so nothing can.
    alternatives: bool = True


NOT_REPRODUCED = "not reproduced under any known seed (nested member, static array element or spelling outside the candidates)"
ENUM_CAPABLE = "enum-capable type not reproduced as uint8 (C++ enum spellings do not reproduce)"

#: One entry per `FieldType` variant in `decode.rs`. A variant that is not
#: here stops the run (exit 2): it would otherwise be silently untestable.
CPP_TYPES = {
    "Bool": FieldTypeSpec(("bool", "uint8"), NOT_REPRODUCED),
    "Byte": FieldTypeSpec(("uint8",), ENUM_CAPABLE),
    "EnumByte": FieldTypeSpec(("uint8",), ENUM_CAPABLE),
    "EnumRemainingBits": FieldTypeSpec(("uint8",), ENUM_CAPABLE),
    "Int32": FieldTypeSpec(("int32",), NOT_REPRODUCED),
    "UInt32": FieldTypeSpec(("uint32",), NOT_REPRODUCED),
    # Not in decode.rs at 9f92756; mapped so a branch that adds it is checked
    # rather than refused.
    "Int64": FieldTypeSpec(("int64",), NOT_REPRODUCED),
    "UInt64": FieldTypeSpec(("uint64",), NOT_REPRODUCED),
    "Float": FieldTypeSpec(("float",), NOT_REPRODUCED),
    "Double": FieldTypeSpec(("double",), NOT_REPRODUCED),
    "FString": FieldTypeSpec(("FString",), NOT_REPRODUCED),
    "FText": FieldTypeSpec(("FText",), NOT_REPRODUCED),
    "FName": FieldTypeSpec(("FName",), NOT_REPRODUCED),
    "ObjectNetGuid": FieldTypeSpec(("UClass*",), "object class not among the candidates, or a nested member", True),
    "Guid": FieldTypeSpec(("FGuid",), NOT_REPRODUCED),
    "GameplayTag": FieldTypeSpec(("FGameplayTag",), NOT_REPRODUCED),
    "VectorFloat": FieldTypeSpec(("FVector3f",), NOT_REPRODUCED),
    "VectorDouble": FieldTypeSpec(("FVector",), NOT_REPRODUCED),
    "VectorNetQuantizeNormal": FieldTypeSpec(("FVector_NetQuantizeNormal",), NOT_REPRODUCED),
    "RotationShort": FieldTypeSpec(("FRotator",), NOT_REPRODUCED),
    "RotationByte": FieldTypeSpec(("FRotator",), NOT_REPRODUCED),
    "Transform": FieldTypeSpec(("FTransform",), NOT_REPRODUCED),
    "RepMovement": FieldTypeSpec(("FRepMovement",), NOT_REPRODUCED),
    # Parametrised: see `spec_for`.
    "SerializedInt": FieldTypeSpec((), ENUM_CAPABLE),
    "VectorNetQuantize": FieldTypeSpec((), NOT_REPRODUCED),
    "ByteArray": FieldTypeSpec((), "no single C++ spelling", alternatives=False),
}

#: `FieldType`s the overlay never decodes: not typed, never checked.
UNTYPED_VARIANTS = ("Raw", "Skip")

#: `VectorNetQuantize { scale }` names its C++ type; any other scale has none.
QUANTIZE_SPELLINGS = {1: "FVector_NetQuantize", 10: "FVector_NetQuantize10",
                      100: "FVector_NetQuantize100"}
#: `SerializedInt { max }` at a whole unsigned width reads exactly that many
#: bits, so it is that unsigned type; any other bound is an enum's.
SERIALIZED_INT_SPELLINGS = {256: "uint8", 65536: "uint16", 4294967296: "uint32"}

#: The spellings a mismatch is looked for among. Only a spelling that
#: REPRODUCES the checksum is ever reported, so a long list costs lottery
#: tickets (printed below), not false findings.
ALTERNATIVE_TYPES = (
    "bool", "int8", "uint8", "int16", "uint16", "int32", "uint32", "int64",
    "uint64", "float", "double", "FString", "FText", "FName", "FVector",
    "FVector3f", "FVector2D", "FRotator", "FQuat", "FTransform", "FGuid",
    "FGameplayTag", "FRepMovement", "FVector_NetQuantize",
    "FVector_NetQuantize10", "FVector_NetQuantize100",
    "FVector_NetQuantizeNormal", "FIntPoint", "FLinearColor", "FColor",
    "TArray", "UClass*",
)

#: Static-array indices tried for vrfkit's own spellings; alternatives are
#: tried at index 0 only.
MAX_STATIC_INDEX = 15

VERDICTS = ("match", "mismatch", "untestable")


def parse_field_type(text: str) -> tuple[str, dict]:
    """`FieldType::X { k: v, .. }` (any whitespace) -> `("X", {k: v})`."""
    text = " ".join(text.split())
    m = re.fullmatch(r"(?:FieldType::)?(\w+)(?:\s*\{(.*)\})?", text)
    if not m:
        raise ValueError(f"unparsable FieldType: {text!r}")
    params = {}
    for part in (m.group(2) or "").split(","):
        part = part.strip()
        if not part:
            continue
        key, _, value = part.partition(":")
        value = value.strip().rsplit("::", 1)[-1]
        params[key.strip()] = int(value) if value.isdigit() else value
    return m.group(1), params


def canonical_type(text: str) -> str:
    """One spelling per `FieldType`, whatever the source's whitespace."""
    variant, params = parse_field_type(text)
    if not params:
        return variant
    return f"{variant} {{ " + ", ".join(f"{k}: {v}" for k, v in params.items()) + " }"


def spec_for(field_type: str) -> FieldTypeSpec:
    variant, params = parse_field_type(field_type)
    if variant not in CPP_TYPES:
        raise KeyError(variant)
    spec = CPP_TYPES[variant]
    if variant == "VectorNetQuantize":
        spelling = QUANTIZE_SPELLINGS.get(params.get("scale"))
        if spelling is None:
            return FieldTypeSpec((), "quantization scale with no C++ spelling")
        return FieldTypeSpec((spelling,), spec.unreproduced)
    if variant == "SerializedInt":
        spelling = SERIALIZED_INT_SPELLINGS.get(params.get("max"))
        return FieldTypeSpec((spelling,) if spelling else (), spec.unreproduced)
    return spec


# --------------------------------------------------------------------------
# The generated tables, and the resolution order they are read in

_LIT = r'"((?:[^"\\]|\\.)*)"'
_TYPE = r"(FieldType::\w+(?:\s*\{[^}]*\})?)"
ENTRY_RE = re.compile(r"OverlayEntry \{\s*group_path: " + _LIT + r",\s*field_name: "
                      + _LIT + r",\s*field_type: " + _TYPE, re.S)
HANDLE_RE = re.compile(r"OverlayHandleEntry \{\s*group_path: " + _LIT
                       + r",\s*handle: (\d+),\s*field_name: " + _LIT, re.S)
SCOPED_RE = re.compile(r"\(\s*" + _LIT + r",\s*" + _LIT + r",\s*(\d+),\s*" + _TYPE
                       + r",?\s*\)", re.S)
CHECKSUM_RE = re.compile(r"\((\d+), " + _TYPE + r"\),", re.S)


def unescape(raw: str) -> str:
    """Undo the escaping the generators write into a Rust string literal."""
    return re.sub(r"\\\r?\n\s*", "", raw).replace('\\"', '"').replace("\\\\", "\\")


class TableError(Exception):
    """A generated table or a resolution constant could not be read whole."""


def _declared_len(src: str, static: str) -> int:
    m = re.search(r"pub(?:\(crate\))? static " + static + r": \[[^;]+; (\d+)\]", src)
    if not m:
        raise TableError(f"no length declared for {static}")
    return int(m.group(1))


def parse_overlay_table(src: str):
    """`({(group, name): type}, {(group, handle): name})` from `table.rs`.

    Refuses a table it cannot read whole: the declared array length must equal
    the entries parsed, or an entry the pattern missed would never be checked.
    """
    entries = {(unescape(g), unescape(n)): canonical_type(t) for g, n, t in ENTRY_RE.findall(src)}
    handles = {(unescape(g), int(h)): unescape(n) for g, h, n in HANDLE_RE.findall(src)}
    for static, got in (("OVERLAY_TABLE", len(entries)), ("OVERLAY_HANDLE_TABLE", len(handles))):
        want = _declared_len(src, static)
        if got != want:
            raise TableError(f"table.rs {static}: declares {want} entries, parsed {got}")
    return entries, handles


def parse_scoped_types(src: str) -> dict:
    scoped = {(unescape(n), unescape(g), int(c)): canonical_type(t)
              for n, g, c, t in SCOPED_RE.findall(src)}
    want = _declared_len(src, "SCOPED_TYPES")
    if len(scoped) != want:
        raise TableError(f"scoped_types.rs: declares {want} entries, parsed {len(scoped)}")
    return scoped


def parse_checksum_table(src: str) -> dict:
    table = {int(c): canonical_type(t) for c, t in CHECKSUM_RE.findall(src)}
    want = _declared_len(src, "CHECKSUM_TYPES")
    if len(table) != want:
        raise TableError(f"checksum_table.rs: declares {want} entries, parsed {len(table)}")
    return table


def parse_resolution_constants(src: str):
    """`(GROUP_ALIASES, ENGINE_OBJECT_REFS)` from `overlay.rs`."""
    m = re.search(r"const GROUP_ALIASES: &\[\(&str, &str\)\] = &\[(.*?)\n\];", src, re.S)
    if not m:
        raise TableError("overlay.rs: GROUP_ALIASES not found")
    body = m.group(1)
    pairs = re.findall(r"\(\s*" + _LIT + r",\s*" + _LIT + r",?\s*\)", body, re.S)
    if len(pairs) != body.count("("):
        raise TableError("overlay.rs: a GROUP_ALIASES entry did not parse")
    aliases = {unescape(a): unescape(b) for a, b in pairs}
    m = re.search(r"const ENGINE_OBJECT_REFS: \[&str; (\d+)\] = \[(.*?)\];", src, re.S)
    if not m:
        raise TableError("overlay.rs: ENGINE_OBJECT_REFS not found")
    refs = tuple(re.findall(_LIT, m.group(2)))
    if len(refs) != int(m.group(1)):
        raise TableError("overlay.rs: ENGINE_OBJECT_REFS length and entries disagree")
    return aliases, refs


def parse_field_type_variants(src: str) -> tuple:
    """The variant names of `pub enum FieldType` in `decode.rs`."""
    m = re.search(r"pub enum FieldType \{(.*?)\n\}", src, re.S)
    if not m:
        raise TableError("decode.rs: `pub enum FieldType` not found")
    body = re.sub(r"//[^\n]*", "", m.group(1))
    body = re.sub(r"\{[^}]*\}", "", body)
    variants = tuple(re.findall(r"^\s*(\w+)\s*,?\s*$", body, re.M))
    if not set(UNTYPED_VARIANTS) <= set(variants):
        # An empty or partial read would make "every variant is classified"
        # true of nothing.
        raise TableError(f"decode.rs: read FieldType as {variants!r}, without Raw and Skip")
    return variants


def is_unresolved_fname_index(name: str) -> bool:
    """`overlay::is_unresolved_fname_index`: a bare decimal names nothing."""
    return bool(name) and name.isascii() and name.isdigit()


class Resolver:
    """Python port of `overlay::resolve_entry` over the parsed tables."""

    def __init__(self, entries, handles, scoped, checksums, aliases, engine_refs):
        self.entries, self.handles = entries, handles
        self.scoped, self.checksums = scoped, checksums
        self.aliases, self.engine_refs = aliases, engine_refs

    @classmethod
    def from_repo(cls) -> "Resolver":
        entries, handles = parse_overlay_table(TABLE_RS.read_text(encoding="utf-8"))
        aliases, refs = parse_resolution_constants(OVERLAY_RS.read_text(encoding="utf-8"))
        return cls(entries, handles, parse_scoped_types(SCOPED_RS.read_text(encoding="utf-8")),
                   parse_checksum_table(CHECKSUM_RS.read_text(encoding="utf-8")), aliases, refs)

    def _in_group(self, group, name, handle):
        """Name, then `b` + name, then handle -- and the handle refusal."""
        if name is not None:
            for probe in (name, "b" + name):
                field_type = self.entries.get((group, probe))
                if field_type is not None:
                    return field_type, "name" if probe == name else "b-prefix"
        if handle is None:
            return None, None
        descriptor_name = self.handles.get((group, handle))
        if descriptor_name is None:
            return None, None
        if name is not None and name != descriptor_name and not is_unresolved_fname_index(name):
            return None, None  # refused: the wire declares something else here
        field_type = self.entries.get((group, descriptor_name))
        return (field_type, "handle") if field_type is not None else (None, None)

    def resolve(self, group, name, handle, checksum):
        """`(FieldType, source)` as vrfkit resolves it, or `(None, None)`."""
        field_type, how = self._in_group(group, name, handle)
        if field_type is not None:
            return field_type, how
        aliased = self.aliases.get(group)
        if aliased is not None:
            field_type, how = self._in_group(aliased, name, handle)
            if field_type is not None:
                return field_type, "alias " + how
        if name is None:
            return None, None
        if checksum is not None:
            field_type = self.scoped.get((name, group, checksum))
            if field_type is not None:
                return field_type, "scoped"
        if name in self.engine_refs:
            return "ObjectNetGuid", "engine reference"
        if checksum is not None and checksum in self.checksums:
            return self.checksums[checksum], "checksum table"
        return None, None


# --------------------------------------------------------------------------
# Declarations


@dataclass
class Identity:
    """One declared `(group, name, handle, checksum)` and where it was seen."""
    group: str
    name: str
    handle: int
    checksum: int
    builds: set = field(default_factory=set)
    exports: int = 0


def export_dirs(corpus: Path | None, exports) -> tuple[list[Path], list[Path]]:
    """`(export directories, skipped generated siblings)`.

    A `--corpus` child counts when it holds `manifest.json` (a declaration
    corpus keeps only that and the checkpoint declaration tables); staging and
    backup siblings an interrupted export leaves are skipped and listed.
    """
    found = [Path(e) for e in exports or []]
    skipped = []
    if corpus is not None:
        for child in sorted(corpus.iterdir()):
            if not child.is_dir():
                continue
            if is_generated_sibling(child.name):
                skipped.append(child)
            elif (child / "manifest.json").is_file():
                found.append(child)
    return found, skipped


def load_declarations(dirs) -> tuple[dict, collections.Counter]:
    """`{(group, name, handle, checksum): Identity}` and per-source counts."""
    ids: dict = {}
    counts = collections.Counter()

    def add(build, group, name, handle, checksum, seen):
        key = (group, name, handle, checksum)
        if key in seen:
            return
        seen.add(key)
        ident = ids.get(key)
        if ident is None:
            ident = ids[key] = Identity(group, name, handle, checksum)
        ident.builds.add(build)
        ident.exports += 1

    for d in dirs:
        manifest = json.loads((d / "manifest.json").read_text(encoding="utf-8"))
        build = str(manifest.get("replay_build") or "?").removeprefix("++Ares-Core+release-")
        seen: set = set()
        counts["exports"] += 1
        for group in manifest.get("net_field_export_groups") or []:
            for f in group.get("fields") or []:
                counts["main declarations"] += 1
                add(build, group["path"], f["name"], f["handle"], f["compatible_checksum"], seen)
        groups_pq, fields_pq = d / "checkpoint_export_groups.parquet", d / "checkpoint_export_fields.parquet"
        if groups_pq.is_file() and fields_pq.is_file():
            counts["exports with checkpoint declarations"] += 1
            declared, orphans, unique = checkpoint_declarations(groups_pq, fields_pq)
            counts["checkpoint declarations"] += declared
            counts["checkpoint declarations without a group"] += orphans
            for group, name, handle, checksum in unique:
                add(build, group, name, handle, checksum, seen)
    return ids, counts


def checkpoint_declarations(groups_pq: Path, fields_pq: Path):
    """`(declarations, declarations without a group, unique identities)`.

    Every checkpoint re-declares the schema -- 54.9M rows over the 1,018-replay
    corpus for 12,937 distinct identities -- so the join and the de-duplication
    run in Arrow rather than row by row in Python.
    """
    import pyarrow.compute as pc
    import pyarrow.parquet as pq

    groups = pq.read_table(groups_pq, columns=["checkpoint_index", "ordinal", "group_path"])
    groups = groups.rename_columns(["checkpoint_index", "group_ordinal", "group_path"])
    fields = pq.read_table(fields_pq, columns=["checkpoint_index", "group_ordinal", "handle",
                                               "compatible_checksum", "rendered_name"])
    joined = fields.join(groups, keys=["checkpoint_index", "group_ordinal"], join_type="left outer")
    if joined.num_rows != fields.num_rows:
        # Two groups under one (checkpoint, ordinal): which one a field
        # belongs to is ambiguous, and guessing would type it against either.
        raise TableError(f"{groups_pq}: a (checkpoint_index, ordinal) key names more than one group")
    orphans = joined.num_rows - pc.sum(pc.is_valid(joined["group_path"])).as_py() if joined.num_rows else 0
    keys = ["group_path", "rendered_name", "handle", "compatible_checksum"]
    unique = joined.filter(pc.is_valid(joined["group_path"])).group_by(keys).aggregate([])
    cols = [unique.column(k).to_pylist() for k in keys]
    return fields.num_rows, orphans, list(zip(*cols))


def class_candidates(groups) -> list[str]:
    """Class names the input declares groups for, plus `ENGINE_CLASSES`.

    Only object paths (`/Script/Pkg.Class`, `/Game/.../Asset.Asset_C`) name a
    class; a bare component instance name does not, so it is not offered.
    """
    names = set(ENGINE_CLASSES)
    for group in groups:
        path = group.split(":", 1)[0].removesuffix(CLASS_NET_CACHE)
        if path.startswith("/") and "." in path:
            leaf = path.rsplit(".", 1)[-1]
            if leaf and leaf.isascii():
                names.add(leaf)
    return sorted(names)


def object_spellings(classes) -> tuple:
    return tuple(f"{prefix}{name}*" for name in classes for prefix in "AU")


# --------------------------------------------------------------------------
# Classification


class Hit(NamedTuple):
    cpp_type: str
    seed: str
    static_index: int


@dataclass
class Verdict:
    verdict: str                  # one of VERDICTS
    detail: str                   # reason (untestable) or the reproducing spelling
    hit: Hit | None = None


class Seeds:
    """Named parent seeds: 0 plus every `PARENT_CHAINS` path."""

    def __init__(self, chains=PARENT_CHAINS):
        self.named = [("top level", 0)]
        for chain in chains:
            label = " > ".join(f"{n}:{t}" for n, t in chain.links)
            self.named.append((label, chain_checksum(chain.links)))


class Checker:
    """Classify `(FieldType, name, checksum)` keys; memoised, since the same
    property is declared by hundreds of groups.

    `trials` counts every (spelling, seed, index) recomputed. Each is a
    1-in-2^32 chance of reproducing a checksum by accident, so the report
    prints `trials / 2^32` -- the number of chance reproductions to expect --
    beside the verdicts. It has to stay far below 1 for a verdict to mean
    anything, and it is the price of every spelling, seed or index added.
    """

    def __init__(self, seeds: Seeds, objects: tuple):
        self.seeds = seeds
        self.objects = tuple(objects)
        self.trials = 0
        self._memo: dict = {}
        self._name_states: dict = {}
        self._encoded: dict = {}

    def _states(self, name):
        """`(seed label, CRC state after the name)` for every known seed."""
        states = self._name_states.get(name)
        if states is None:
            encoded = name.lower().encode("utf-32-le")
            states = [(label, zlib.crc32(encoded, seed)) for label, seed in self.seeds.named]
            self._name_states[name] = states
        return states

    def _tails(self, spelling, indices):
        key = (spelling, indices)
        tails = self._encoded.get(key)
        if tails is None:
            body = spelling.lower().encode("utf-32-le")
            tails = self._encoded[key] = [(i, body + struct.pack("<I", i)) for i in indices]
        return tails

    def _find(self, name, checksum, spellings, indices, states):
        """Every `Hit` among `spellings` x `states` x `indices`."""
        hits = []
        for label, state in states:
            for spelling in spellings:
                for index, tail in self._tails(spelling, indices):
                    self.trials += 1
                    if zlib.crc32(tail, state) == checksum:
                        hits.append(Hit(spelling, label, index))
        return hits

    def classify(self, field_type: str, name: str, checksum: int) -> Verdict:
        key = (field_type, name, checksum)
        verdict = self._memo.get(key)
        if verdict is None:
            verdict = self._memo[key] = self._classify(field_type, name, checksum)
        return verdict

    def _classify(self, field_type, name, checksum):
        spec = spec_for(field_type)
        if is_unresolved_fname_index(name):
            if name not in HARDCODED_FNAMES:
                return Verdict("untestable", f"bare FName index {name} not in HARDCODED_FNAMES")
            name = HARDCODED_FNAMES[name]
        if not name.isascii():
            return Verdict("untestable", "non-ASCII name")
        if checksum == 0:
            return Verdict("untestable", "declared checksum is 0")
        states = self._states(name)
        # vrfkit's own spellings: every seed, static indices 0..MAX; object
        # pointers at index 0 only, since there are thousands of them.
        hits = self._find(name, checksum, spec.expected, tuple(range(MAX_STATIC_INDEX + 1)), states)
        if spec.objects:
            hits += self._find(name, checksum, self.objects, (0,), states)
        if hits:
            return Verdict("match", hits[0].cpp_type, hits[0])
        if not spec.alternatives:
            return Verdict("untestable", spec.unreproduced)
        alternatives = tuple(t for t in ALTERNATIVE_TYPES if t not in spec.expected)
        hits = self._find(name, checksum, alternatives, (0,), states)
        if not spec.objects:
            # An object pointer as the alternative, at the top level only: the
            # candidates run to thousands and the chain seeds would multiply them.
            hits += self._find(name, checksum, self.objects, (0,), states[:1])
        if hits:
            return Verdict("mismatch", hits[0].cpp_type, hits[0])
        return Verdict("untestable", spec.unreproduced)


# --------------------------------------------------------------------------
# The run


@dataclass
class Report:
    identities: int = 0
    #: `(group, name, checksum)` that resolve to more than one FieldType,
    #: depending on the handle it was declared at (the handle fallback).
    handle_dependent: int = 0
    by_verdict: collections.Counter = field(default_factory=collections.Counter)
    by_reason: collections.Counter = field(default_factory=collections.Counter)
    match_rules: collections.Counter = field(default_factory=collections.Counter)
    not_checked: collections.Counter = field(default_factory=collections.Counter)
    by_build: dict = field(default_factory=lambda: collections.defaultdict(collections.Counter))
    by_source: collections.Counter = field(default_factory=collections.Counter)
    chain_hits: collections.Counter = field(default_factory=collections.Counter)
    mismatches: list = field(default_factory=list)
    rows: list = field(default_factory=list)


def match_rule(field_type: str, hit: Hit) -> str:
    variant = parse_field_type(field_type)[0]
    if variant == "Bool" and hit.cpp_type == "uint8":
        return "Bool as uint8 (bitfield bool or byte; the width decides)"
    if variant == "ObjectNetGuid":
        return "ObjectNetGuid with its class" if hit.cpp_type != "UClass*" else "ObjectNetGuid as UClass*"
    if hit.static_index:
        return "static-array element (index > 0)"
    return "exact spelling"


def check_identities(ids: dict, resolver: Resolver, checker: Checker) -> Report:
    """Resolve and classify every declared identity."""
    report = Report()
    seen: dict = {}
    for key in sorted(ids):
        ident = ids[key]
        if ident.group.endswith(CLASS_NET_CACHE):
            # A ClassNetCache group declares functions, not properties: the
            # overlay never types one of its fields, and its checksum is a
            # different hash.
            report.not_checked["function slots (ClassNetCache groups)"] += 1
            continue
        field_type, source = resolver.resolve(ident.group, ident.name, ident.handle, ident.checksum)
        if field_type is None:
            report.not_checked["not typed by vrfkit (no resolution)"] += 1
            continue
        field_type = canonical_type(field_type)
        variant = parse_field_type(field_type)[0]
        if variant in UNTYPED_VARIANTS:
            report.not_checked[f"not typed by vrfkit ({variant})"] += 1
            continue
        identity = (ident.group, ident.name, ident.checksum, field_type)
        if identity in seen:
            seen[identity]["builds"] |= ident.builds
            continue
        verdict = checker.classify(field_type, ident.name, ident.checksum)
        row = {"group": ident.group, "name": ident.name, "checksum": ident.checksum,
               "handle": ident.handle, "field_type": field_type, "source": source,
               "verdict": verdict.verdict, "detail": verdict.detail,
               "seed": verdict.hit.seed if verdict.hit else None,
               "static_index": verdict.hit.static_index if verdict.hit else None,
               "builds": set(ident.builds)}
        seen[identity] = row
        report.rows.append(row)
    types_of = collections.defaultdict(set)
    for row in report.rows:
        types_of[(row["group"], row["name"], row["checksum"])].add(row["field_type"])
    report.handle_dependent = sum(1 for types in types_of.values() if len(types) > 1)
    for row in report.rows:
        report.identities += 1
        report.by_verdict[row["verdict"]] += 1
        report.by_source[(row["source"], row["verdict"])] += 1
        for build in row["builds"]:
            report.by_build[build][row["verdict"]] += 1
        if row["verdict"] == "untestable":
            report.by_reason[row["detail"]] += 1
        elif row["verdict"] == "match":
            hit = Hit(row["detail"], row["seed"], row["static_index"])
            report.match_rules[match_rule(row["field_type"], hit)] += 1
        if row["seed"] is not None and row["seed"] != "top level":
            report.chain_hits[row["seed"]] += 1
        if row["verdict"] == "mismatch":
            report.mismatches.append(row)
    return report


def check_checksum_table(ids: dict, resolver: Resolver, checker: Checker):
    """`(counts, mismatch rows)` for every `checksum_table.rs` entry.

    The table maps a checksum to a type with no name, so each entry is
    recomputed under every name that declares its checksum in the input.
    An entry is a mismatch if any carrier reproduces a different spelling,
    a match if any carrier reproduces the entry's own, else untestable;
    entries nothing in the input declares are counted apart.
    """
    carriers = collections.defaultdict(set)
    for ident in ids.values():
        if not ident.group.endswith(CLASS_NET_CACHE):
            carriers[ident.checksum].add(ident.name)
    counts = collections.Counter()
    bad = []
    for checksum, field_type in sorted(resolver.checksums.items()):
        names = sorted(carriers.get(checksum, ()))
        if not names:
            counts["no carrier in the input"] += 1
            continue
        if len(names) > 1:
            counts["of which carried by more than one name"] += 1
        verdicts = [(n, checker.classify(field_type, n, checksum)) for n in names]
        kinds = {v.verdict for _, v in verdicts}
        verdict = "mismatch" if "mismatch" in kinds else "match" if "match" in kinds else "untestable"
        counts[verdict] += 1
        if verdict == "mismatch":
            bad.extend((checksum, field_type, n, v.detail, v.hit.seed) for n, v in verdicts
                       if v.verdict == "mismatch")
    return counts, bad


def print_report(report: Report, table_counts, table_bad, checker: Checker, declared_counts,
                 skipped, n_ids) -> None:
    p = print
    p(f"inputs: {declared_counts['exports']} export(s), {declared_counts['main declarations']} main "
      f"declarations, {declared_counts['checkpoint declarations']} checkpoint declarations "
      f"({declared_counts['exports with checkpoint declarations']} exports carry them, "
      f"{declared_counts['checkpoint declarations without a group']} without a group), "
      f"{len(skipped)} generated sibling dir(s) skipped")
    p(f"declared identities (group, name, handle, checksum): {n_ids}")
    p("not checked:")
    for reason in ("function slots (ClassNetCache groups)", "not typed by vrfkit (no resolution)",
                   "not typed by vrfkit (Raw)", "not typed by vrfkit (Skip)"):
        p(f"  {report.not_checked[reason]:>7}  {reason}")
    p(f"\ntyped identities (group, name, checksum, FieldType): {report.identities}")
    for verdict in VERDICTS:
        p(f"  {report.by_verdict[verdict]:>7}  {verdict}")
    p(f"  {report.handle_dependent:>7}  (group, name, checksum) typed differently at different "
      f"handles, each counted once per type")
    p("match, by rule:")
    for rule in ("exact spelling", "Bool as uint8 (bitfield bool or byte; the width decides)",
                 "ObjectNetGuid with its class", "ObjectNetGuid as UClass*",
                 "static-array element (index > 0)"):
        p(f"  {report.match_rules[rule]:>7}  {rule}")
    p("untestable, by reason:")
    reasons = sorted(set(report.by_reason) | {NOT_REPRODUCED, ENUM_CAPABLE},
                     key=lambda r: (-report.by_reason[r], r))
    for reason in reasons:
        p(f"  {report.by_reason[reason]:>7}  {reason}")
    p("by resolution source (match / mismatch / untestable):")
    for source in sorted({s for s, _ in report.by_source}):
        cells = " / ".join(str(report.by_source[(source, v)]) for v in VERDICTS)
        p(f"  {cells:>17}  {source}")
    p("by build (identities declared in that build; match / mismatch / untestable):")
    for build in sorted(report.by_build):
        cells = " / ".join(str(report.by_build[build][v]) for v in VERDICTS)
        p(f"  {build:>6}  {cells}")
    p("parent chains (typed identities whose verdict came from that seed):")
    for label, _ in checker.seeds.named[1:]:
        p(f"  {report.chain_hits[label]:>7}  {label}")
    partition = ("match", "mismatch", "untestable", "no carrier in the input")
    p(f"\nchecksum_table.rs: {sum(table_counts[k] for k in partition)} entries")
    for key in partition + ("of which carried by more than one name",):
        p(f"  {table_counts[key]:>7}  {key}")
    p(f"\nchecksum trials: {checker.trials}; expected chance reproductions "
      f"{checker.trials / 2 ** 32:.4f} (trials / 2^32)")
    p(f"\nmismatches: {len(report.mismatches)} identities, {len(table_bad)} checksum_table.rs carriers")
    for row in report.mismatches:
        p(f"  MISMATCH {row['group']} | {row['name']} | {row['checksum']}: vrfkit "
          f"{row['field_type']} ({row['source']}), checksum reproduces {row['detail']} "
          f"under {row['seed']}; builds {','.join(sorted(row['builds']))}")
    for checksum, field_type, name, spelling, seed in table_bad:
        p(f"  MISMATCH checksum_table.rs {checksum} -> {field_type}: carrier {name} "
          f"reproduces {spelling} under {seed}")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--export", type=Path, action="append", default=[],
                    help="directory written by `vrfkit export` (repeatable)")
    ap.add_argument("--corpus", type=Path,
                    help="directory whose children are exports (manifest.json each)")
    ap.add_argument("--json", type=Path, help="also write every classified identity here")
    args = ap.parse_args(argv)

    if args.corpus is not None and not args.corpus.is_dir():
        print(f"FAILED: --corpus {args.corpus} is not a directory", file=sys.stderr)
        return 2
    dirs, skipped = export_dirs(args.corpus, args.export)
    if not dirs:
        print("FAILED: no export to read (give --export or --corpus)", file=sys.stderr)
        return 2
    missing = [d for d in dirs if not (d / "manifest.json").is_file()]
    if missing:
        print(f"FAILED: no manifest.json in {missing[0]}", file=sys.stderr)
        return 2

    try:
        resolver = Resolver.from_repo()
        variants = parse_field_type_variants(DECODE_RS.read_text(encoding="utf-8"))
    except TableError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 2
    unknown = sorted(set(variants) - set(CPP_TYPES) - set(UNTYPED_VARIANTS))
    if unknown:
        print(f"FAILED: FieldType variant(s) {unknown} are not classified in CPP_TYPES; "
              f"add their C++ spelling (or an untestable reason) first", file=sys.stderr)
        return 2
    tabled = {parse_field_type(t)[0] for t in (*resolver.entries.values(), *resolver.scoped.values(),
                                               *resolver.checksums.values())}
    stray = sorted(tabled - set(variants))
    if stray:
        print(f"FAILED: the tables use FieldType variant(s) {stray} that decode.rs does not "
              f"declare", file=sys.stderr)
        return 2

    try:
        ids, declared_counts = load_declarations(dirs)
    except TableError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 2
    checker = Checker(Seeds(), object_spellings(class_candidates({i.group for i in ids.values()})))
    report = check_identities(ids, resolver, checker)
    table_counts, table_bad = check_checksum_table(ids, resolver, checker)
    print_report(report, table_counts, table_bad, checker, declared_counts, skipped, len(ids))

    if args.json is not None:
        rows = [{**r, "builds": sorted(r["builds"])} for r in report.rows]
        args.json.write_text(json.dumps({"identities": rows, "checksum_table_mismatches": [
            {"checksum": c, "field_type": t, "name": n, "reproduces": s, "seed": seed}
            for c, t, n, s, seed in table_bad]}, indent=1), encoding="utf-8")

    if report.identities == 0:
        print("\nFAILED: nothing checked -- no declared identity is typed by vrfkit, so this "
              "input is empty or not an export", file=sys.stderr)
        return 1
    if report.mismatches or table_bad:
        print(f"\nFAILED: {len(report.mismatches)} typed identit"
              f"{'y' if len(report.mismatches) == 1 else 'ies'} and {len(table_bad)} "
              f"checksum_table.rs carrier(s) hash as a different C++ type than vrfkit "
              f"decodes. Each line above names the spelling that reproduces the replay's "
              f"own checksum.", file=sys.stderr)
        return 1
    print(f"\nOK: {report.by_verdict['match']} typed identities reproduce their checksum, "
          f"none reproduces a different type; {report.by_verdict['untestable']} are "
          f"untestable and say nothing either way.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
