"""Emit the Rust overlay table (group_path, field_name) -> FieldType from the
C# descriptors.

Scans every .cs file under Replay.Valorant for ExportGroupDescriptor
subclasses, class by class in multi-class files, and reads each one's Path and
its AddProperty(...).Type() calls; fields propagate through inheritance (agent
descriptors get GenericAgentDescriptor's). ClassNetCache descriptors emit Skip
entries for their AddFunction names so coverage counts them, and the parameter
descriptors defined beside them are read like any other. A custom decoder is
Raw unless PAYLOAD_DECODER_TYPES names its wire type; .Ignore() is Skip. Each
class's ExportGroupKind decides whether its properties can be wire names at
all (EXPORT_GROUP_KIND_POLICY).

Hard failures, never silent omissions: an AddProperty whose type method is
unclassified or absent (except the DECODERLESS_PROPERTIES, which the summary
lists), an unparseable Path, Kind or Categories override, and two classes
typing one (path, field) differently. Literal AddPropertyHandle declarations
also fill a handle -> name table, only a runtime fallback for replays whose
field name differs from the descriptor's label; lookup stays name-first.

Usage:
    python tools/extract_descriptors.py <replay_valorant_dir> <out.rs>

The committed table comes from the vendored descriptors:
    python tools/extract_descriptors.py third_party/vrp/Replay.Valorant crates/vrf-decode/src/table.rs
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from collections import Counter

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

CSHARP_IDENTIFIER = r'[A-Za-z_]\w*'
CSHARP_IDENTIFIER_TOKEN = rf'@?{CSHARP_IDENTIFIER}'


def normalize_csharp_identifier(identifier: str) -> str:
    """Return the semantic name of a regular or escaped C# identifier."""
    return identifier[1:] if identifier.startswith("@") else identifier


PRIMITIVE_TYPES = {
    "Int32": "FieldType::Int32",
    "UInt32": "FieldType::UInt32",
    "UInt64": "FieldType::UInt64",
    "Float": "FieldType::Float",
    "Double": "FieldType::Double",
    "Bool": "FieldType::Bool",
    "Byte": "FieldType::Byte",
    "EnumByte": "FieldType::EnumByte",
    "FString": "FieldType::FString",
    "FName": "FieldType::FName",
    "ObjectNetGuid": "FieldType::ObjectNetGuid",
    "Guid": "FieldType::Guid",
    "EnumRemainingBits": "FieldType::EnumRemainingBits",
    "FGameplayTag": "FieldType::GameplayTag",
    "FVector": "FieldType::VectorDouble",
    "FVectorNetQuantize": "FieldType::VectorNetQuantize { scale: 1 }",
    "FVectorNetQuantize10": "FieldType::VectorNetQuantize { scale: 10 }",
    "FVectorNetQuantize100": "FieldType::VectorNetQuantize { scale: 100 }",
    "FVectorNetQuantizeNormal": "FieldType::VectorNetQuantizeNormal",
    "FRotatorShort": "FieldType::RotationShort",
    "Transform": "FieldType::Transform",
    "Ignore": "FieldType::Skip",
}

# Every explicit Path override must parse, or a new expression shape would
# quietly remove its whole group from the table.
PATH_OVERRIDE_MARKER_RE = re.compile(r'\boverride\s+string\s+Path\b')
PATH_EXPRESSION_RE = re.compile(
    r'override\s+string\s+Path\s*=>(?P<expression>[^;]+);', re.DOTALL
)
STATIC_CLASS_RE = re.compile(
    rf'\b(?:public|internal)\s+static\s+class\s+@?(?P<name>{CSHARP_IDENTIFIER})\s*\{{'
)
STRING_CONSTANT_RE = re.compile(
    rf'\b(?:public|internal|private)\s+const\s+string\s+'
    rf'@?(?P<name>{CSHARP_IDENTIFIER})\s*=(?P<expression>[^;]+);',
    re.DOTALL,
)


def resolve_path_expression(
    expression: str, constants: dict[str, str], owner: str | None = None,
    resolving: frozenset[str] = frozenset(),
) -> str:
    """Evaluate only literal and class-scoped const string concatenation."""
    position = 0
    parts: list[str] = []
    need_term = True
    while True:
        position = _skip_csharp_trivia(expression, position)
        if position == len(expression):
            if need_term:
                raise ValueError(f"unsupported path expression {expression.strip()!r}")
            return "".join(parts)
        if not need_term:
            if expression[position] != "+":
                raise ValueError(f"unsupported path expression {expression.strip()!r}")
            position += 1
            need_term = True
            continue
        literal = _parse_csharp_string_literal(expression, position)
        if literal is not None:
            value, position = literal
            parts.append(value)
        else:
            identifier = re.match(
                rf'@?{CSHARP_IDENTIFIER}(?:\s*\.\s*@?{CSHARP_IDENTIFIER})?',
                expression[position:],
            )
            if identifier is None:
                raise ValueError(f"unsupported path expression {expression.strip()!r}")
            token = re.sub(r'\s+', '', identifier.group()).replace('@', '')
            key = token if "." in token else f"{owner}.{token}"
            if key not in constants or key in resolving:
                raise ValueError(f"unresolved path constant {key!r}")
            constant_owner = key.split(".", 1)[0]
            parts.append(resolve_path_expression(
                constants[key], constants, constant_owner, resolving | {key}
            ))
            position += identifier.end()
        need_term = False

# The optional ``@`` sits outside the name capture so every dictionary uses the
# semantic C# name. Groups are named: a positional group(2) shifts when a group
# is added.
MULTI_CLASS_RE = re.compile(
    rf'(?:public|internal)\s+(?:sealed\s+)?(?:abstract\s+)?class\s+'
    rf'@?(?P<name>{CSHARP_IDENTIFIER})'
    r'(?P<generics><[^>]+>)?\s*'
    r'(?::\s*(?P<base>[\w@.<>,\s]+?))?'
    r'(?:\s+where\s+[^\{]+?)?'
    r'\s*\{',
    re.DOTALL,
)


def class_key(reference: str) -> str:
    """The key a class is stored under: its name and generic arity.

    C# tells `Foo` from `Foo<T>`, and the vendored tree declares both for
    ResourceComponentDescriptor. Keyed by bare name they merged into one
    record whose fields depended on file order, so the generic one is
    `Foo`1` (the .NET spelling). `reference` is a declaration (`Foo<T>`) or a
    use (`Foo<List<int>, Bar>`).
    """
    name, bracket, arguments = reference.partition("<")
    name = normalize_csharp_identifier(name.strip())
    if not bracket:
        return name
    depth, arity = 0, 1
    for ch in arguments:
        if ch == "<":
            depth += 1
        elif ch == ">":
            if depth == 0:
                break
            depth -= 1
        elif ch == "," and depth == 0:
            arity += 1
    return f"{name}`{arity}"


def declared_class_key(match: re.Match[str]) -> str:
    """`class_key` of one MULTI_CLASS_RE declaration."""
    return class_key(match.group("name") + (match.group("generics") or ""))


def base_class_key(bases: str) -> str:
    """`class_key` of the first entry of a base list, which is the base class."""
    depth = 0
    for i, ch in enumerate(bases):
        depth += {"<": 1, ">": -1}.get(ch, 0)
        if ch == "," and depth == 0:
            return class_key(bases[:i])
    return class_key(bases)


# Whether each ExportGroupKind's property names can be wire field names.
# `Kind` is metadata in the C#: only the JSON writers and export statistics read
# it (via DescriptorCatalogIndex and ExportBindingRegistry), no decode path
# branches on it, so it is the author's declaration of what the descriptor
# describes and cannot have been bent to make decoding work.
#   "emit"          the group's net field exports are these properties.
#   "drop"          not a net field export group (FastArray: an array element
#                   struct), so no property can be an overlay key.
#   "attribute_set" only the generic pair below replicates; per-attribute
#                   properties are C# labels, not wire names.
# Unknown MUST emit: it is the C# default from the protected parameterless
# constructor, CoveAbility, DarkCoverAbility, ProjectileSmokeScreen and
# SmokeScreenManager descriptors never override it, and BaseReplayPlayerState
# declares it; their fields decode. An unlisted Kind is a hard failure, so a
# new C# enum member is classified by someone, not swept into "emit".
EXPORT_GROUP_KIND_POLICY = {
    "Unknown": "emit",
    "Actor": "emit",
    "PlayerController": "emit",
    "Component": "emit",
    "AttributeSet": "attribute_set",
    "FastArray": "drop",
    "ClassNetCache": "emit",
}

# The two members of UE's FGameplayAttributeData, which every attribute
# subobject of an AttributeSet group carries. The C# descriptor also names each
# attribute (Health, MaxHealth, Shield, ...); those names never reach the wire.
# Across the 11 cross-validated replays /Script/ShooterGame.AresAttributeSet
# presents BaseValue (133,284 rows) and CurrentValue (132,808), all decoded,
# and no other field name -- 0 rows for any of the six.
ATTRIBUTE_SET_WIRE_PROPERTIES = ("BaseValue", "CurrentValue")

# Kind when no class in the chain overrides it: ExportGroupDescriptor's
# protected parameterless constructor leaves `_kind` at
# `default(ExportGroupKind)`, and Unknown is ordinal 0.
DEFAULT_EXPORT_GROUP_KIND = "Unknown"

EXPORT_CATEGORY_NAMES = {
    "None",
    "Movement",
    "Ability",
    "Gunplay",
    "Agent",
    "GameState",
    "Inventory",
    "Economy",
    "Effects",
    "Visibility",
    "Debug",
    "All",
}
EXPORT_CATEGORY_TYPE = (
    r'(?:global\s*::\s*)?'
    r'(?:[A-Za-z_]\w*\s*\.\s*)*ExportCategory'
)
CATEGORY_RETURN_TYPE = (
    r'@?[A-Za-z_]\w*(?:\s*(?:::|\.)\s*@?[A-Za-z_]\w*)*'
)
CATEGORY_OVERRIDE_MARKER_RE = re.compile(
    rf'\boverride\s+(?P<return_type>{CATEGORY_RETURN_TYPE})\s+@?Categories\b'
)
CATEGORY_OVERRIDE_RE = re.compile(
    rf'\boverride\s+{EXPORT_CATEGORY_TYPE}\s+Categories\s*=>\s*'
    r'(?P<expression>[^;]+)\s*;'
)
CATEGORY_EXPRESSION_RE = re.compile(
    rf'\s*{EXPORT_CATEGORY_TYPE}\s*\.\s*\w+'
    rf'(?:\s*\|\s*{EXPORT_CATEGORY_TYPE}\s*\.\s*\w+)*\s*'
)
CATEGORY_MEMBER_RE = re.compile(
    rf'{EXPORT_CATEGORY_TYPE}\s*\.\s*(\w+)'
)

EXPORT_GROUP_KIND_TYPE = (
    r'(?:global\s*::\s*)?'
    r'(?:[A-Za-z_]\w*\s*\.\s*)*ExportGroupKind'
)
# Detect the override, then parse it, as for Categories: an unreadable `Kind`
# override read as absent would resolve to Unknown, which emits everything --
# a failure that looks exactly like success.
KIND_OVERRIDE_MARKER_RE = re.compile(
    rf'\boverride\s+(?P<return_type>{CATEGORY_RETURN_TYPE})\s+@?Kind\b'
)
KIND_OVERRIDE_RE = re.compile(
    rf'\boverride\s+{EXPORT_GROUP_KIND_TYPE}\s+@?Kind\s*=>\s*'
    rf'{EXPORT_GROUP_KIND_TYPE}\s*\.\s*(?P<kind>\w+)\s*;'
)

# `rejected` label for an AddProperty whose type method IS classified but whose
# export name is not extractable (e.g. a named constant, not a string literal or
# lambda leaf), so the report does not read as "add this to PRIMITIVE_TYPES".
_UNNAMED_FIELD = "<unresolvable field name>"

# `rejected` label for an AddProperty with no type method `_extract_type_name`
# can name: a bare `AddProperty(x => x.Foo);` or a call shape it cannot match.
_NO_TYPE_METHOD = "<no type method>"

# The only AddProperty declarations allowed no type method, keyed (descriptor
# class, C# property); any other is a hard failure, and so is a listed property
# its class now declares WITH a type. They contribute no entry. Upstream binds
# each with no decoder: the builder's Decoder stays null,
# ReplayExportBinder.ResolveFieldDecoder returns null and FieldPayloadParser
# skips the bits (upstream's parser at the vendored commit 8824794), so there
# is no type to import -- and a Skip or Raw entry would pre-empt the scoped,
# engine-reference and checksum fallbacks in overlay.rs for that name.
# Measured 2026-09-28 by logging every statement the splitter produced over
# third_party/vrp and re-running each through the ladder: 464 distinct
# statements, 461 yield a field, these 3 yield nothing and are not rejected,
# and nothing else takes that path. Upstream 2d2e05e, b51d674, 2b66c65, 2103d92
# and 8b7afcb declare the same three this way and no other, and none reached
# the wire in the 1,018-replay corpus exported by 259ed10 (0 rows in
# fields.parquet and checkpoint_fields.parquet under its exact group path).
DECODERLESS_PROPERTIES = frozenset({
    # /Script/ShooterGame.AresAbilitySystemComponent, Kind Component. The wire
    # names there are OwnerActor, AvatarActor, SpawnedAttributes and
    # CachedAttributeSet.
    ("AresAbilitySystemComponentDescriptor", "AresAttributeSet"),
    # BaseReplayController_C, Kind PlayerController.
    ("BaseReplayControllerDescriptor", "RemoteCharacterUpdatesArray"),
    # /Script/ShooterGame.RemoteCharacterUpdate, Kind FastArray, so the whole
    # group is dropped anyway; crates/vrf-movement decodes this stream itself.
    ("RemoteCharacterUpdateDescriptor", "ComponentDataStream"),
})

SERIALIZED_INT_RE = re.compile(
    r'\.SerializedInt\(\s*(?:maxValue:\s*)?(\d+)\s*\)'
)

BYTE_ARRAY_RE = re.compile(
    r'\.ByteArray\(\s*(?:maxBytes:\s*)?(\d+)\s*\)'
)

REP_MOVEMENT_RE = re.compile(
    r'\.ReplicatedMovement\(\s*ERotatorQuantization\.(?P<quant>\w+)\s*\)'
)

REP_MOVEMENT_DEFAULT_RE = re.compile(
    r'\.ReplicatedMovement\(\s*\)'
)
REP_MOVEMENT_PROPERTY_RE = re.compile(
    rf'\.ReplicatedMovement\(\s*(?P<property>{CSHARP_IDENTIFIER_TOKEN})\s*\)'
)
#: The location quantization every `RepMovement` entry is emitted with. The
#: descriptors declare only the rotator width; the location level is a
#: per-class choice (Unreal's `LocationQuantizationLevel`) the wire does not
#: carry, and the C# reader decodes every class at two decimals
#: (`ReplicatedMovementDecoder` reads `VectorNetQuantize100`). On 25 of the 26
#: classes the table declares, the packed integer joined to the actor's
#: actors.parquet spawn IS the world coordinate (median |packed| / |spawn|
#: 1.000 on every class and build carrying it; 1,018 replays, 21 builds,
#: 2026-09-28), so the default is whole units, also FRepMovement's own;
#: apply_type_corrections.py pins the one two-decimal class, and docs/DATA.md
#: has the per-class figures. For an unmeasured class this is a prior:
#: `tests::overlay` lists every RepMovement group (table or scoped) with its
#: measured level and fails on one it does not list.
REP_MOVEMENT_LOCATION = "VectorQuantization::RoundWholeNumber"


def rep_movement_type(rotation: str) -> str:
    """The `FieldType::RepMovement` literal for one rotator width; all three
    ReplicatedMovement call shapes spell it through here."""
    if rotation not in {"ByteComponents", "ShortComponents"}:
        raise ValueError(f"unsupported rotator quantization {rotation!r}")
    return (
        f"FieldType::RepMovement {{ rotation: RotatorQuantization::{rotation}, "
        f"location: {REP_MOVEMENT_LOCATION} }}"
    )


MOVEMENT_TYPE_PREFIX = "<virtual movement rotation:"
MOVEMENT_OVERRIDE_RE = re.compile(
    rf'\b(?:virtual|override)\s+ERotatorQuantization\s+'
    rf'@?(?P<property>{CSHARP_IDENTIFIER})\s*=>\s*'
    r'ERotatorQuantization\.(?P<value>\w+)\s*;'
)

# RepLayoutDynamicArray<T>(): an opaque TArray this table cannot decode
# generically, so Raw.
REP_LAYOUT_DYN_ARRAY_RE = re.compile(
    r'\.RepLayoutDynamicArray<\w+>\(\s*\)'
)

# A custom `.Decode(...)` decoder: Raw unless PAYLOAD_DECODER_TYPES names it.
DECODE_RE = re.compile(
    r'\.Decode\('
)

# `.Decode(ValorantPayloadDecoders.X(...))` decoders whose NAME states the wire
# type, so reading it is not inference. Without this map a regeneration
# downgrades eight committed typed entries to Raw (the C# moved them from direct
# `.FVectorNetQuantize100()` calls onto decoder objects). `RawPayload` and
# `CapturedPayload` are opaque and stay Raw: a decoder that names no type means
# the type is unknown, which is not the same as raw.
PAYLOAD_DECODER_TYPES = {
    "VectorNetQuantize": "FieldType::VectorNetQuantize { scale: 1 }",
    "VectorNetQuantize10": "FieldType::VectorNetQuantize { scale: 10 }",
    "VectorNetQuantize100": "FieldType::VectorNetQuantize { scale: 100 }",
    "VectorNetQuantizeNormal": "FieldType::VectorNetQuantizeNormal",
    # An equippable arrives as the object's net GUID; the decoder resolves it
    # to a weapon afterwards.
    "Equippable": "FieldType::ObjectNetGuid",
}

PAYLOAD_DECODER_RE = re.compile(
    r'\.Decode\(\s*(?:global\s*::\s*)?(?:[A-Za-z_]\w*\s*\.\s*)*'
    r'ValorantPayloadDecoders\s*\.\s*(?P<decoder>\w+)'
)

ADD_FUNCTION_RE = re.compile(
    r'AddFunction(?:Handle)?(?:<(?P<params>\w+)>)?\s*\(\s*'
    r'(?:(?P<handle>\d+)\s*,\s*)?'
    r'"(?P<name>[^"]+)"'
)

# Expression-bodied wrappers that delegate to AddPropertyHandle(...).Decode(...),
# found by their body rather than by a baked-in name (the live one is
# DamageParameters<T>.AddRaw).
RAW_WRAPPER_DEF_RE = re.compile(
    r'(?:public|internal|protected|private)\s+(?:static\s+)?void\s+'
    rf'@?(?P<name>{CSHARP_IDENTIFIER})\s*\([^)]*\)\s*=>\s*'
    r'AddPropertyHandle\([^;]+\)\s*\.Decode\(',
    re.DOTALL,
)




def _mask_raw_wrapper_definitions(code_view: str) -> str:
    """Blank out a raw-wrapper's OWN definition, not any call to it.

    The definition (`protected void AddRaw(uint handle, ... property, string
    typeName) => AddPropertyHandle(handle, property, ...).Decode(`) has the
    shape of a real call site, with formal parameters where the field name
    goes, so left in place it would start an unresolvable statement. Blanked
    rather than deleted, so the line/column offsets the module relies on hold.
    """
    chars = list(code_view)
    for m in RAW_WRAPPER_DEF_RE.finditer(code_view):
        for i in range(m.start(), m.end()):
            if chars[i] != "\n":
                chars[i] = " "
    return "".join(chars)


def _split_statements(
    block: str, wrapper_re: re.Pattern[str] | None
) -> list[tuple[str, str]]:
    """`(raw text, code view)` of every AddProperty or raw-wrapper statement:
    from a line whose live code starts with one, joined until a line ends with
    `;` (so a `.Type()` or `.Decode()` on a later line stays with it); a
    statement still open when the block ends is kept."""
    code_lines = _mask_raw_wrapper_definitions(csharp_code_view(block)).splitlines()
    statements: list[tuple[str, str]] = []
    raw: list[str] = []
    code: list[str] = []
    for raw_line, code_line in zip(block.splitlines(), code_lines):
        stripped = code_line.strip()
        if stripped.startswith("AddProperty") or (wrapper_re and wrapper_re.match(stripped)):
            live_start = len(code_line) - len(code_line.lstrip())
            raw, code = [raw_line[live_start:]], [code_line[live_start:]]
        elif raw:
            raw.append(raw_line)
            code.append(code_line)
        else:
            continue
        if stripped.endswith(";"):
            statements.append(("\n".join(raw), "\n".join(code)))
            raw, code = [], []
    if raw:
        statements.append(("\n".join(raw), "\n".join(code)))
    return statements


def _statement_type(code_line: str) -> tuple[str | None, str | None]:
    """`(rust type, None)` for a statement the ladder classifies, else
    `(None, rejection label)`. The checks run in the ladder's order."""
    if serialized := SERIALIZED_INT_RE.search(code_line):
        return f"FieldType::SerializedInt {{ max: {serialized.group(1)} }}", None
    if byte_array := BYTE_ARRAY_RE.search(code_line):
        return f"FieldType::ByteArray {{ max_bytes: {byte_array.group(1)} }}", None
    if movement := REP_MOVEMENT_RE.search(code_line):
        if movement.group("quant") not in {"ByteComponents", "ShortComponents"}:
            return None, "ReplicatedMovement"
        return rep_movement_type(movement.group("quant")), None
    if REP_MOVEMENT_DEFAULT_RE.search(code_line):
        return rep_movement_type("ShortComponents"), None
    if movement_property := REP_MOVEMENT_PROPERTY_RE.search(code_line):
        return MOVEMENT_TYPE_PREFIX + movement_property.group("property") + ">", None
    if REP_LAYOUT_DYN_ARRAY_RE.search(code_line):
        return "FieldType::Raw", None
    if DECODE_RE.search(code_line):
        decoder = PAYLOAD_DECODER_RE.search(code_line)
        return PAYLOAD_DECODER_TYPES.get(
            decoder.group("decoder") if decoder else "", "FieldType::Raw"
        ), None
    type_name = _extract_type_name(code_line)
    if type_name in PRIMITIVE_TYPES:
        return PRIMITIVE_TYPES[type_name], None
    return None, type_name or _NO_TYPE_METHOD


def extract_fields_from_block(
    block: str,
    raw_wrapper_names: set[str],
    rejected: set[tuple[str, str]],
) -> list[tuple[str, str, int | None]]:
    """Extract (field_export_name, rust_type, literal_handle) tuples.

    ``literal_handle`` is set only when the declaration supplies a decimal
    handle. ``rejected`` collects ``(label, statement)`` for every AddProperty
    this file cannot fully classify: an unknown type method, no type method
    (``_NO_TYPE_METHOD``) or no extractable name (``_UNNAMED_FIELD``). Each
    would otherwise untype a field silently -- one new upstream method
    (``.Int64()``) untypes every field declaring it -- so the caller fails on
    any, less DECODERLESS_PROPERTIES. The statement is the set key, so the
    Configure scan and the whole-class helper scan report it once.
    """
    wrapper_re = re.compile(
        r'@?(' + '|'.join(map(re.escape, sorted(raw_wrapper_names))) + r')\s*\('
    ) if raw_wrapper_names else None
    fields = []
    for raw_line, code_line in _split_statements(block, wrapper_re):
        wrapper = wrapper_re.match(code_line) if wrapper_re else None
        if wrapper:
            field_type, label = "FieldType::Raw", None
            name = _extract_lambda_field_name(code_line)
            handle = _extract_literal_handle(code_line, wrapper.group(1))
        else:
            field_type, label = _statement_type(code_line)
            name = _extract_field_name(raw_line, code_line) if field_type else None
            handle = _extract_literal_handle(code_line)
        statement = " ".join(code_line.split())
        if label is not None:
            rejected.add((label, statement))
        elif name:
            fields.append((name, field_type, handle))
        else:
            rejected.add((_UNNAMED_FIELD, statement))
    return fields


def extract_cnc_functions(block: str) -> list[str]:
    """The function export names ("MulticastEndRound", ...) that a
    ClassNetCache Configure() block adds."""
    names = []
    for m in ADD_FUNCTION_RE.finditer(block):
        names.append(m.group("name"))
    return names


def extract_called_cnc_helper_functions(
    class_body: str, configure_body: str
) -> list[str]:
    """AddFunction names from the parameterless helpers Configure calls; only
    called ones, so an unused method cannot become a table entry."""
    names = []
    helper_calls = re.findall(r'(?m)^\s*(\w+)\s*\(\s*\)\s*;', configure_body)
    for helper_name in helper_calls:
        declaration = re.search(
            rf'(?:public|internal|protected|private)\s+(?:static\s+)?void\s+'
            rf'{re.escape(helper_name)}\s*\(\s*\)',
            class_body,
        )
        if declaration is None:
            continue
        body_start = class_body.find('{', declaration.end())
        if body_start == -1:
            continue
        _, body_end = find_class_body_range(class_body, declaration.start())
        names.extend(extract_cnc_functions(class_body[body_start:body_end]))
    return names


def _skip_csharp_trivia(source: str, position: int) -> int:
    """Skip whitespace and comments from an aligned raw-source position."""
    while position < len(source):
        if source[position].isspace():
            position += 1
            continue
        if source.startswith("//", position):
            newline = source.find("\n", position + 2)
            return len(source) if newline == -1 else _skip_csharp_trivia(
                source, newline + 1
            )
        if source.startswith("/*", position):
            closing = source.find("*/", position + 2)
            if closing == -1:
                return len(source)
            position = closing + 2
            continue
        break
    return position


def _parse_csharp_string_literal(
    source: str, position: int
) -> tuple[str, int] | None:
    """Parse a plain/verbatim string at ``position`` after C# trivia."""
    position = _skip_csharp_trivia(source, position)
    if source.startswith('@"', position):
        cursor = position + 2
        value: list[str] = []
        while cursor < len(source):
            if source.startswith('""', cursor):
                value.append('"')
                cursor += 2
            elif source[cursor] == '"':
                return "".join(value), cursor + 1
            else:
                value.append(source[cursor])
                cursor += 1
        return None
    if position >= len(source) or source[position] != '"':
        return None

    cursor = position + 1
    while cursor < len(source):
        if source[cursor] == "\\":
            cursor += 2
        elif source[cursor] == '"':
            return source[position + 1:cursor], cursor + 1
        else:
            cursor += 1
    return None


def _extract_field_name(raw_line: str, code_line: str) -> str | None:
    """Extract a live explicit export name or semantic lambda property leaf."""
    invocation = re.match(
        rf'@?(?P<method>AddProperty\w*)\s*\(', code_line
    )
    if invocation is not None:
        argument_start = invocation.end()
        if invocation.group("method") == "AddPropertyHandle":
            comma = code_line.find(",", argument_start)
            if comma != -1:
                argument_start = comma + 1
        literal = _parse_csharp_string_literal(raw_line, argument_start)
        if literal is not None:
            value, literal_end = literal
            next_token = _skip_csharp_trivia(raw_line, literal_end)
            if (
                next_token == len(raw_line)
                or raw_line[next_token] in ",)"
            ):
                return value

    return _extract_lambda_field_name(code_line)


def _extract_lambda_field_name(line: str) -> str | None:
    """Extract and normalize a live lambda's optional escaped property leaf."""
    match = re.search(
        rf'@?{CSHARP_IDENTIFIER}\s*=>\s*'
        rf'(?:@?{CSHARP_IDENTIFIER}\s*\.\s*)?'
        rf'@?(?P<name>{CSHARP_IDENTIFIER})',
        line,
    )
    return match.group("name") if match else None


def _extract_literal_handle(
    line: str, declaration_name: str = "AddPropertyHandle"
) -> int | None:
    """Return a handle declaration's leading decimal argument, if present."""
    match = re.match(
        rf'@?{re.escape(declaration_name)}\s*\(\s*(\d+)\s*,', line
    )
    return int(match.group(1)) if match else None


def _extract_type_name(line: str) -> str | None:
    """The type method name (the `.Type()` call) in a line.

    Type arguments are skipped, so a generic `.Enum<EFoo>()` is rejected by
    name as an unknown method, not as no method. The parameterless
    `RepLayoutDynamicArray<T>()`, the descriptors' only generic call, has its
    own ladder branch; its element-decoder overload or a qualified `<Ns.T>`
    lands here and is rejected by name.
    """
    m = re.search(r'\)\s*\.(\w+)\s*(?:<[^()]*>)?\s*\(', line)
    return m.group(1) if m else None


def find_class_body_range(source: str, class_start: int) -> tuple[int, int]:
    """(body_start, body_end) of the first braced body at or after
    `class_start`: just after its `{`, and at its closing `}`."""
    brace_pos = source.find('{', class_start)
    if brace_pos == -1:
        return (class_start, len(source))

    depth = 1
    pos = brace_pos + 1
    while pos < len(source) and depth > 0:
        ch = source[pos]
        if ch == '{':
            depth += 1
        elif ch == '}':
            depth -= 1
        pos += 1

    return (brace_pos + 1, pos - 1)


def extract_parameterless_method_body(
    source: str, method_name: str, code_view: str
) -> str | None:
    """Return the unique live parameterless method's block or expression body."""
    declaration_re = re.compile(
        rf'\b(?:public|internal|protected|private)\s+(?:static\s+)?'
        rf'[\w@.<>,?\[\]]+\s+@?{re.escape(method_name)}\s*\(\s*\)\s*'
    )
    declarations = list(declaration_re.finditer(code_view))
    if not declarations:
        return None
    if len(declarations) != 1:
        raise ValueError(
            f"ambiguous method declaration ({len(declarations)} matches)"
        )
    declaration = declarations[0]

    body_start = declaration.end()
    if code_view.startswith("=>", body_start):
        expression_start = body_start + 2
        expression_end = code_view.find(";", expression_start)
        if expression_end == -1:
            return None
        return source[expression_start:expression_end]

    if body_start >= len(code_view) or code_view[body_start] != "{":
        return None

    _, body_end = find_class_body_range(code_view, declaration.start())
    if body_end < body_start or body_end >= len(source):
        return None
    return source[body_start + 1:body_end]


def csharp_code_view(source: str) -> str:
    """Return a same-length view with comments and literal bodies masked."""
    masked = list(source)
    source_len = len(source)

    def mask_non_newlines(start: int, end: int) -> None:
        for index in range(start, end):
            if source[index] not in "\r\n":
                masked[index] = " "

    def scan_regular_literal(opening_quote: int, quote: str) -> int:
        cursor = opening_quote + 1
        while cursor < source_len:
            if source[cursor] == "\\":
                cursor += 2
            elif source[cursor] == quote:
                return cursor + 1
            else:
                cursor += 1
        return source_len

    def scan_verbatim_string(opening_quote: int) -> int:
        cursor = opening_quote + 1
        while cursor < source_len:
            if source.startswith('""', cursor):
                cursor += 2
            elif source[cursor] == '"':
                return cursor + 1
            else:
                cursor += 1
        return source_len

    interpolated_prefixes = ("$@\"", "@$\"", "$\"")

    def run_length(start: int, character: str) -> int:
        cursor = start
        while cursor < source_len and source[cursor] == character:
            cursor += 1
        return cursor - start

    def raw_delimiter_at(start: int) -> tuple[int, int] | None:
        cursor = start
        while cursor < source_len and source[cursor] == "$":
            cursor += 1
        dollar_count = cursor - start
        if dollar_count == 0 and (
            start >= source_len or source[start] != '"'
        ):
            return None
        quote_count = run_length(cursor, '"')
        if quote_count < 3:
            return None
        return dollar_count, quote_count

    def scan_comment(start: int) -> int | None:
        if source.startswith("//", start):
            newline = source.find("\n", start + 2)
            return source_len if newline == -1 else newline
        if source.startswith("/*", start):
            closing = source.find("*/", start + 2)
            return source_len if closing == -1 else closing + 2
        return None

    def scan_raw_interpolation(start: int, dollar_count: int) -> int:
        cursor = start
        brace_depth = 0
        while cursor < source_len:
            comment_end = scan_comment(cursor)
            if comment_end is not None:
                cursor = comment_end
                continue

            literal_end = scan_literal_at(cursor)
            if literal_end is not None:
                cursor = literal_end
                continue

            if source[cursor] == "{":
                opening_count = run_length(cursor, "{")
                brace_depth += opening_count
                cursor += opening_count
                continue

            if source[cursor] == "}":
                closing_count = run_length(cursor, "}")
                structural_closings = min(brace_depth, closing_count)
                brace_depth -= structural_closings
                delimiter_closings = closing_count - structural_closings
                cursor += closing_count
                if (
                    brace_depth == 0
                    and dollar_count <= delimiter_closings < 2 * dollar_count
                ):
                    return cursor
                continue

            cursor += 1
        return source_len

    def scan_raw_string(
        start: int, dollar_count: int, quote_count: int
    ) -> int:
        cursor = start + dollar_count + quote_count
        while cursor < source_len:
            if source[cursor] == '"':
                closing_count = run_length(cursor, '"')
                if closing_count == quote_count:
                    return cursor + closing_count
                cursor += closing_count
                continue

            if dollar_count and source[cursor] == "{":
                opening_count = run_length(cursor, "{")
                if dollar_count <= opening_count < 2 * dollar_count:
                    cursor = scan_raw_interpolation(
                        cursor + opening_count, dollar_count
                    )
                else:
                    cursor += opening_count
                continue

            cursor += 1
        return source_len

    def scan_interpolated_string(start: int, prefix: str) -> int:
        cursor = start + len(prefix)
        expression_depth = 0
        verbatim = "@" in prefix
        while cursor < source_len:
            if expression_depth == 0:
                if verbatim and source.startswith('""', cursor):
                    cursor += 2
                elif not verbatim and source[cursor] == "\\":
                    cursor += 2
                elif source[cursor] == '"':
                    return cursor + 1
                elif source.startswith("{{", cursor) or source.startswith("}}", cursor):
                    cursor += 2
                elif source[cursor] == "{":
                    expression_depth = 1
                    cursor += 1
                else:
                    cursor += 1
                continue

            comment_end = scan_comment(cursor)
            if comment_end is not None:
                cursor = comment_end
                continue

            literal_end = scan_literal_at(cursor)
            if literal_end is not None:
                cursor = literal_end
            elif source[cursor] == "{":
                expression_depth += 1
                cursor += 1
            elif source[cursor] == "}":
                expression_depth -= 1
                cursor += 1
            else:
                cursor += 1
        return source_len

    def scan_literal_at(start: int) -> int | None:
        raw_delimiter = raw_delimiter_at(start)
        if raw_delimiter is not None:
            return scan_raw_string(start, *raw_delimiter)

        interpolated_prefix = next(
            (
                prefix
                for prefix in interpolated_prefixes
                if source.startswith(prefix, start)
            ),
            None,
        )
        if interpolated_prefix is not None:
            return scan_interpolated_string(start, interpolated_prefix)
        if source.startswith('@"', start):
            return scan_verbatim_string(start + 1)
        if start < source_len and source[start] in "\"'":
            return scan_regular_literal(start, source[start])
        return None

    pos = 0
    while pos < source_len:
        comment_end = scan_comment(pos)
        if comment_end is not None:
            mask_non_newlines(pos, comment_end)
            pos = comment_end
            continue

        literal_end = scan_literal_at(pos)
        if literal_end is not None:
            start = pos
            pos = literal_end
            mask_non_newlines(start, pos)
            continue

        pos += 1

    return "".join(masked)


def direct_member_code_view(source: str) -> str:
    """Mask block contents so only declarations in the current type remain."""
    code_view = csharp_code_view(source)
    masked = list(code_view)
    depth = 0
    for index, character in enumerate(code_view):
        if character == "{":
            if depth > 0:
                masked[index] = " "
            depth += 1
            continue
        if character == "}":
            if depth > 1:
                masked[index] = " "
            if depth > 0:
                depth -= 1
            continue
        if depth > 0 and character not in "\r\n":
            masked[index] = " "
    return "".join(masked)


NESTED_TYPE_RE = re.compile(
    r'\b(?:class|struct|interface|enum|record(?:\s+(?:class|struct))?)\s+'
    rf'{CSHARP_IDENTIFIER_TOKEN}(?:\s*<[^>{{}}]*>)?[^;{{}}]*\{{',
    re.DOTALL,
)


def find_innermost_type_body_range(
    code_view: str, position: int
) -> tuple[int, int] | None:
    """Return the innermost class/record/struct body containing ``position``."""
    candidates: list[tuple[int, int]] = []
    for declaration in NESTED_TYPE_RE.finditer(code_view):
        body_start, body_end = find_class_body_range(
            code_view, declaration.start()
        )
        if body_start <= position < body_end:
            candidates.append((body_start, body_end))
    return max(candidates, key=lambda body_range: body_range[0], default=None)


def mask_nested_type_bodies(source: str) -> str:
    """Return source text with direct nested type declarations masked."""
    code_view = csharp_code_view(source)
    member_view = direct_member_code_view(source)
    masked = list(source)
    for declaration in NESTED_TYPE_RE.finditer(member_view):
        _, body_end = find_class_body_range(code_view, declaration.start())
        for index in range(declaration.start(), min(body_end + 1, len(source))):
            if source[index] not in "\r\n":
                masked[index] = " "
    return "".join(masked)


def _single_override(class_name, class_body, marker_re, override_re, what, shape):
    """The class's one readable `override` of a member, or None without one.
    Any other shape is a hard failure naming the return types, never absent."""
    member_view = direct_member_code_view(class_body)
    markers = list(marker_re.finditer(member_view))
    if not markers:
        return None
    overrides = list(override_re.finditer(member_view))
    if len(markers) != 1 or len(overrides) != 1 or markers[0].start() != overrides[0].start():
        return_types = ", ".join(sorted({m.group("return_type").strip() for m in markers}))
        raise SystemExit(f"{class_name}: unsupported {what} override "
                         f"using return type {return_types!r}; {shape}")
    return overrides[0]


def extract_category_override(
    class_name: str, class_body: str
) -> frozenset[str] | None:
    """Parse a class's explicit category flags, rejecting unsupported forms."""
    override = _single_override(class_name, class_body, CATEGORY_OVERRIDE_MARKER_RE,
                                CATEGORY_OVERRIDE_RE, "ExportCategory",
                                "expected an expression joined with |")
    if override is None:
        return None
    expression = override.group("expression")
    if CATEGORY_EXPRESSION_RE.fullmatch(expression) is None:
        raise SystemExit(
            f"{class_name}: unsupported ExportCategory override expression "
            f"{expression.strip()!r}"
        )

    categories = frozenset(CATEGORY_MEMBER_RE.findall(expression))
    unknown = sorted(categories - EXPORT_CATEGORY_NAMES)
    if unknown:
        raise SystemExit(
            f"{class_name}: unknown ExportCategory {', '.join(unknown)}"
        )
    return categories


def extract_kind_override(
    class_name: str, class_body: str
) -> str | None:
    """Parse a class's explicit ExportGroupKind, rejecting unsupported forms."""
    override = _single_override(class_name, class_body, KIND_OVERRIDE_MARKER_RE,
                                KIND_OVERRIDE_RE, "ExportGroupKind",
                                "expected => ExportGroupKind.<Member>;")
    if override is None:
        return None
    kind = override.group("kind")
    if kind not in EXPORT_GROUP_KIND_POLICY:
        raise SystemExit(
            f"{class_name}: unhandled ExportGroupKind {kind!r}. "
            "Decide whether this kind's properties can be wire field names and "
            "add it to EXPORT_GROUP_KIND_POLICY; the generator will not guess."
        )
    return kind


def fields_for_export_group_kind(
    class_name: str,
    path: str,
    kind: str,
    fields: list[tuple[str, str, int | None]],
) -> list[tuple[str, str, int | None]]:
    """Keep only the declared fields this kind can actually put on the wire.

    `kind` is a key of EXPORT_GROUP_KIND_POLICY: `extract_kind_override`
    refuses any other, and the default is one.
    """
    policy = EXPORT_GROUP_KIND_POLICY[kind]
    if policy == "emit":
        return fields
    if policy == "drop":
        return []
    if policy == "attribute_set":
        kept = [
            field
            for field in fields
            if field[0] in ATTRIBUTE_SET_WIRE_PROPERTIES
        ]
        if not kept:
            # A renamed pair or an unexpected descriptor shape needs a human;
            # a silently emptied group would lose 266,092 decoded values.
            raise SystemExit(
                f"{class_name} ({path}): AttributeSet descriptor declares none "
                f"of {', '.join(ATTRIBUTE_SET_WIRE_PROPERTIES)}. Check whether "
                "the replicated attribute pair was renamed upstream."
            )
        return kept
    raise SystemExit(
        f"{class_name}: unknown policy {policy!r} for ExportGroupKind {kind!r}"
    )


def parse_supported_string_expression(
    raw_expression: str, code_expression: str
) -> tuple[str, str] | None:
    """Parse exactly one literal or semantic identifier, excluding prefixes."""
    code_value = code_expression.strip()
    constant = re.fullmatch(
        rf'@?(?P<name>{CSHARP_IDENTIFIER})', code_value
    )
    if constant is not None:
        return "constant", constant.group("name")
    if code_value:
        return None

    literal = _parse_csharp_string_literal(raw_expression, 0)
    if literal is None:
        return None
    value, literal_end = literal
    if _skip_csharp_trivia(raw_expression, literal_end) != len(raw_expression):
        return None
    return "literal", value


def extract_returned_rpc_name_initializer(
    factory_body: str,
) -> tuple[str, str]:
    """Return the exhaustive Name initializer of one returned RpcDescriptor."""
    factory_view = csharp_code_view(factory_body)
    direct_factory_view = direct_member_code_view(factory_body)
    object_shape = (
        r'new\s+(?:@?RpcDescriptor\s*(?:\(\s*\))?|\(\s*\))\s*\{'
    )
    returned_re = re.compile(rf'\breturn\s+{object_shape}')
    candidates = list(returned_re.finditer(direct_factory_view))
    if not candidates:
        expression = re.match(rf'\s*{object_shape}', direct_factory_view)
        if expression is not None:
            candidates = [expression]
    if len(candidates) != 1:
        all_initializers = list(re.finditer(object_shape, direct_factory_view))
        if len(all_initializers) > 1:
            raise ValueError(
                f"ambiguous RpcDescriptor initializer "
                f"({len(all_initializers)} matches)"
            )
        raise ValueError("returned RpcDescriptor initializer not found")

    _, initializer_end = find_class_body_range(
        factory_view, candidates[0].start()
    )
    opening_brace = factory_view.find("{", candidates[0].start())
    if opening_brace == -1 or initializer_end < opening_brace:
        raise ValueError("returned RpcDescriptor initializer not found")
    initializer_source = factory_body[opening_brace + 1:initializer_end]
    initializer_view = direct_member_code_view(initializer_source)
    name_markers = list(
        re.finditer(r'(?<!\w)@?Name\s*=', initializer_view)
    )
    if not name_markers:
        raise ValueError("RpcDescriptor.Name not found in returned initializer")
    if len(name_markers) != 1:
        raise ValueError(
            f"ambiguous RpcDescriptor.Name ({len(name_markers)} matches)"
        )

    value_start = name_markers[0].end()
    value_end = initializer_view.find(",", value_start)
    if value_end == -1:
        value_end = len(initializer_view)
    parsed = parse_supported_string_expression(
        initializer_source[value_start:value_end],
        initializer_view[value_start:value_end],
    )
    if parsed is None:
        raise ValueError("unsupported RpcDescriptor.Name initializer")
    return parsed


def resolve_direct_string_constant(
    owning_source: str, owning_member_view: str, constant_name: str
) -> str:
    """Resolve one exhaustive direct-member const string initializer."""
    declaration_re = re.compile(
        rf'\bconst\s+string\s+@?(?P<name>{CSHARP_IDENTIFIER})\s*='
        r'(?P<initializer>[^;]*);',
        re.DOTALL,
    )
    declarations = [
        declaration
        for declaration in declaration_re.finditer(owning_member_view)
        if declaration.group("name") == constant_name
    ]
    if not declarations:
        raise ValueError(
            f"RpcDescriptor.Name constant {constant_name} could not be resolved"
        )
    if len(declarations) != 1:
        raise ValueError(
            f"RpcDescriptor.Name constant {constant_name} is ambiguous "
            f"({len(declarations)} matches)"
        )

    initializer_start, initializer_end = declarations[0].span("initializer")
    parsed = parse_supported_string_expression(
        owning_source[initializer_start:initializer_end],
        owning_member_view[initializer_start:initializer_end],
    )
    if parsed is None or parsed[0] != "literal":
        raise ValueError(
            f"unsupported constant {constant_name} initializer"
        )
    return parsed[1]


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        raise SystemExit(__doc__)

    src_dir = Path(argv[1])
    out_path = Path(argv[2])

    if not src_dir.is_dir():
        raise SystemExit(f"source directory not found: {src_dir}")

    # Phase 1: scan every file (some declare several classes) into per-class
    # paths, fields and bases.
    class_paths: dict[str, str] = {}        # class_name -> path
    class_fields: dict[str, list[tuple[str, str, int | None]]] = {}
    class_bases: dict[str, str] = {}        # class_name -> base_class_name
    class_category_overrides: dict[str, frozenset[str]] = {}
    class_kind_overrides: dict[str, str] = {}  # class_name -> ExportGroupKind
    movement_overrides: dict[str, dict[str, str]] = {}
    cnc_functions: dict[str, list[str]] = {}  # class_name -> function names
    runtime_cnc_specs: list[tuple[str, str]] = []  # (path suffix, function name)
    # (type_method, statement) for every AddProperty this file cannot classify.
    rejected_types: set[tuple[str, str]] = set()
    # DECODERLESS_PROPERTIES keys actually declared without a type method.
    decoderless_seen: set[tuple[str, str]] = set()

    cs_files = sorted(src_dir.rglob("*.cs"))
    if not cs_files:
        raise SystemExit(f"no .cs source files found under: {src_dir}")
    sources = []
    for cs_file in cs_files:
        source = cs_file.read_text(encoding="utf-8-sig")
        sources.append((cs_file, source, csharp_code_view(source)))

    path_constants: dict[str, str] = {}
    cache_factories: dict[tuple[str, str], tuple[str, str]] = {}
    supported_cache_factory_starts: set[tuple[Path, int]] = set()
    for cs_file, source, code_view in sources:
        # Constants in concrete descriptors are scoped to that class too.
        # Reveal descriptors all call their own constant DescriptorPath.
        constant_owners = [
            (match.group("name"), match.start())
            for match in STATIC_CLASS_RE.finditer(code_view)
        ] + [
            (declared_class_key(match), match.start())
            for match in MULTI_CLASS_RE.finditer(code_view)
        ]
        for owner, start in constant_owners:
            body_start, body_end = find_class_body_range(
                code_view, start
            )
            member_view = direct_member_code_view(source[body_start:body_end])
            for constant in STRING_CONSTANT_RE.finditer(member_view):
                key = f"{owner}.{constant.group('name')}"
                if key in path_constants:
                    raise SystemExit(f"duplicate path constant {key} at {cs_file}")
                path_constants[key] = source[
                    body_start + constant.start("expression"):
                    body_start + constant.end("expression")
                ]
        for class_match in STATIC_CLASS_RE.finditer(code_view):
            body_start, body_end = find_class_body_range(
                code_view, class_match.start()
            )
            member_view = direct_member_code_view(source[body_start:body_end])
            factory_re = re.compile(
                rf'\bpublic\s+static\s+ClassNetCacheDescriptor\s+'
                rf'@?(?P<name>{CSHARP_IDENTIFIER})\s*\('
                rf'\s*string\s+(?P<parameter>{CSHARP_IDENTIFIER})'
                r'(?:\s*,\s*uint\s+\w+)?\s*\)\s*=>\s*new\s*\('
            )
            for factory in factory_re.finditer(member_view):
                supported_cache_factory_starts.add(
                    (cs_file, body_start + factory.start())
                )
                end = member_view.find(";", factory.end())
                if end == -1:
                    raise SystemExit(f"{cs_file}: unterminated cache factory")
                body = source[body_start + factory.start():body_start + end]
                parameter = factory.group("parameter")
                path = re.search(
                    rf'\b{re.escape(parameter)}\s*\+\s*"(?P<suffix>[^"]+)"',
                    body,
                )
                names = re.findall(r'\bName\s*=\s*"([^"]+)"', body)
                if (path is None or len(names) != 1 or
                        len(re.findall(r'\bnew\s+RpcDescriptor\b', body)) != 1):
                    raise SystemExit(
                        f"{cs_file}: unsupported ClassNetCache factory "
                        f"{class_match.group('name')}.{factory.group('name')}"
                    )
                cache_factories[(class_match.group("name"), factory.group("name"))] = (
                    path.group("suffix"), names[0]
                )

    # Every access level: upstream 8b7afcb builds Raze's caches through
    # `private static ClassNetCacheDescriptor Rpc(...)` helpers, which a
    # public/internal-only marker let through with the caches silently absent.
    cache_factory_marker = re.compile(
        rf'\b(?:public|internal|protected|private)\s+static\s+ClassNetCacheDescriptor\s+'
        rf'@?(?P<name>{CSHARP_IDENTIFIER})\s*\('
    )
    for cs_file, _, code_view in sources:
        for marker in cache_factory_marker.finditer(code_view):
            if (cs_file, marker.start()) not in supported_cache_factory_starts:
                raise SystemExit(
                    f"{cs_file}: unsupported ClassNetCache factory "
                    f"{marker.group('name')}"
                )

    static_cache_specs: list[tuple[str, str]] = []
    for cs_file, _, code_view in sources:
        for (factory_class, factory_name), (suffix, function_name) in cache_factories.items():
            calls = re.finditer(
                rf'\b{re.escape(factory_class)}\s*\.\s*'
                rf'{re.escape(factory_name)}\s*\((?P<arguments>[^)]*)\)',
                code_view,
            )
            for call in calls:
                arguments = re.fullmatch(
                    rf'\s*(?P<path>{CSHARP_IDENTIFIER}\s*\.\s*{CSHARP_IDENTIFIER})'
                    r'\s*(?:,\s*\d+\s*)?',
                    call.group("arguments"),
                )
                if arguments is None:
                    raise SystemExit(
                        f"{cs_file}: unsupported ClassNetCache factory call "
                        f"{factory_class}.{factory_name}"
                    )
                try:
                    path = resolve_path_expression(
                        arguments.group("path"), path_constants
                    )
                except ValueError as error:
                    raise SystemExit(f"{cs_file}: {error}") from error
                static_cache_specs.append((path + suffix, function_name))

    # Inheritance and raw-wrapper ownership come before fields, so a derived
    # descriptor can use a wrapper declared in any file while an unrelated
    # class with a same-named method cannot inherit its meaning.
    raw_wrapper_names_by_class: dict[str, set[str]] = {}
    class_declarations_by_name: dict[str, list[Path]] = {}
    for cs_file, source, code_view in sources:
        for class_match in MULTI_CLASS_RE.finditer(code_view):
            class_name = declared_class_key(class_match)
            class_declarations_by_name.setdefault(class_name, []).append(cs_file)
            raw_base = class_match.group("base")
            if raw_base:
                class_bases[class_name] = base_class_key(raw_base)

            body_start, body_end = find_class_body_range(
                code_view, class_match.start()
            )
            class_body = source[body_start:body_end]
            member_view = direct_member_code_view(class_body)
            wrapper_names = {
                match.group("name")
                for match in RAW_WRAPPER_DEF_RE.finditer(member_view)
            }
            if wrapper_names:
                raw_wrapper_names_by_class.setdefault(class_name, set()).update(
                    wrapper_names
                )

    for class_name in sorted(raw_wrapper_names_by_class):
        declarations = class_declarations_by_name.get(class_name, [])
        if len(declarations) > 1:
            locations = ", ".join(str(path) for path in declarations)
            raise SystemExit(
                f"ambiguous raw-wrapper owner {class_name}: "
                f"duplicate class declarations at {locations}"
            )

    def ancestors(cls: str):
        """`cls`, then each base in turn; stops at the root or on a cycle."""
        seen: set[str] = set()
        while cls is not None and cls not in seen:
            seen.add(cls)
            yield cls
            cls = class_bases.get(cls)

    def nearest(overrides: dict, cls: str, default):
        """The override of the nearest class in `cls`'s chain that has one."""
        return next((overrides[c] for c in ancestors(cls) if c in overrides), default)

    def effective_raw_wrapper_names(class_name: str) -> set[str]:
        return set().union(
            *(raw_wrapper_names_by_class.get(c, ()) for c in ancestors(class_name))
        )

    # Runtime-created ClassNetCaches have no descriptor class or literal path:
    # read the constructor's suffix and RpcDescriptor.Name from source, and
    # apply them to every Agent-category descriptor path in phase 3c.
    runtime_cache_re = re.compile(
        rf'\bnew\s+'
        rf'(?:(?:global\s*::\s*)?(?:{CSHARP_IDENTIFIER_TOKEN}\s*\.\s*)*)'
        rf'@?ClassNetCacheDescriptor\s*\(\s*'
        rf'{CSHARP_IDENTIFIER_TOKEN}\s*\.\s*@?Path\s*\+\s*'
        r'"(?P<suffix>[^"]+)"\s*,\s*'
        r'\[',
        re.DOTALL,
    )
    factory_call_re = re.compile(
        rf'\s*@?(?P<factory>{CSHARP_IDENTIFIER})\s*\(\s*\)\s*'
    )
    # A live `new ClassNetCacheDescriptor(...)` the shape above cannot read is
    # unsupported, not absent: upstream 8b7afcb's `CreateFunctions(agent)`,
    # where a `[...]` list is expected, dropped all 29 agent `_ClassNetCache`
    # entries while the run succeeded (compare_descriptor_sources.py on the
    # vendored tree plus that file, 2026-09-28).
    runtime_cache_marker_re = re.compile(
        rf'\bnew\s+'
        rf'(?:(?:global\s*::\s*)?(?:{CSHARP_IDENTIFIER_TOKEN}\s*\.\s*)*)'
        rf'@?ClassNetCacheDescriptor\s*\('
    )
    for cs_file, source, code_view in sources:
        readable = {match.start() for match in runtime_cache_re.finditer(source)}
        for marker in runtime_cache_marker_re.finditer(code_view):
            if marker.start() not in readable:
                raise SystemExit(
                    f"{cs_file}: unsupported runtime ClassNetCache construction; "
                    'expected new ClassNetCacheDescriptor(<descriptor>.Path + '
                    '"<suffix>", [<factory>()])'
                )
    for _, source, code_view in sources:
        for runtime_match in runtime_cache_re.finditer(source):
            if code_view[runtime_match.start()] == " ":
                continue
            factories_start = runtime_match.end()
            factories_end = code_view.find("]", factories_start)
            if factories_end == -1:
                raise SystemExit(
                    "runtime ClassNetCache: unterminated factory list"
                )
            factory_call = factory_call_re.fullmatch(
                code_view[factories_start:factories_end]
            )
            if factory_call is None:
                raise SystemExit(
                    "runtime ClassNetCache: unsupported factory list "
                    f"{source[factories_start:factories_end].strip()!r}"
                )
            factory_name = factory_call.group("factory")

            owning_range = find_innermost_type_body_range(
                code_view, runtime_match.start()
            )
            if owning_range is None:
                raise SystemExit(
                    f"runtime ClassNetCache factory {factory_name}: "
                    "owning type not found"
                )
            owning_start, owning_end = owning_range
            owning_source = source[owning_start:owning_end]
            owning_code_view = csharp_code_view(owning_source)
            owning_member_view = direct_member_code_view(owning_source)

            relative_runtime_start = runtime_match.start() - owning_start
            containing_blocks: list[tuple[int, int]] = []
            for index, character in enumerate(owning_member_view):
                if character != "{":
                    continue
                block_start, block_end = find_class_body_range(
                    owning_member_view, index
                )
                if block_start <= relative_runtime_start < block_end:
                    containing_blocks.append((block_start, block_end))
            if containing_blocks:
                method_start, method_end = max(
                    containing_blocks, key=lambda body_range: body_range[0]
                )
                containing_method_view = owning_code_view[
                    method_start:method_end
                ]
                local_factory_re = re.compile(
                    rf'\b[\w@.<>,?\[\]]+\s+@?{re.escape(factory_name)}'
                    r'\s*\(\s*\)\s*(?:=>|\{)'
                )
                if local_factory_re.search(containing_method_view):
                    raise SystemExit(
                        f"runtime ClassNetCache factory {factory_name}: "
                        "local factory shadows direct member"
                    )
                local_factory_delegate_re = re.compile(
                    rf'\bFunc\s*<\s*RpcDescriptor\s*>\s+'
                    rf'@?{re.escape(factory_name)}\s*='
                )
                if local_factory_delegate_re.search(
                    direct_member_code_view(containing_method_view)
                ):
                    raise SystemExit(
                        f"runtime ClassNetCache factory {factory_name}: "
                        "local factory delegate shadows direct member"
                    )

            try:
                factory_body = extract_parameterless_method_body(
                    owning_source, factory_name, owning_member_view
                )
            except ValueError as error:
                raise SystemExit(
                    f"runtime ClassNetCache factory {factory_name}: {error}"
                ) from error
            if factory_body is None:
                raise SystemExit(
                    f"runtime ClassNetCache factory {factory_name}: "
                    "method body not found"
                )
            try:
                initializer_kind, initializer_value = (
                    extract_returned_rpc_name_initializer(factory_body)
                )
                if initializer_kind == "literal":
                    function_name = initializer_value
                else:
                    local_constant_re = re.compile(
                        rf'\bconst\s+string\s+'
                        rf'@?{re.escape(initializer_value)}\b'
                    )
                    if local_constant_re.search(
                        direct_member_code_view(factory_body)
                    ):
                        raise ValueError(
                            f"local constant {initializer_value} "
                            "shadows direct member"
                        )
                    function_name = resolve_direct_string_constant(
                        owning_source,
                        owning_member_view,
                        initializer_value,
                    )
            except ValueError as error:
                raise SystemExit(
                    f"runtime ClassNetCache factory {factory_name}: {error}"
                ) from error
            runtime_cnc_specs.append(
                (runtime_match.group("suffix"), function_name)
            )

    for cs_file, source, code_view in sources:

        class_matches = list(MULTI_CLASS_RE.finditer(code_view))

        for cm in class_matches:
            class_name = declared_class_key(cm)
            body_start, body_end = find_class_body_range(code_view, cm.start())
            class_body = source[body_start:body_end]
            class_body_code_view = code_view[body_start:body_end]
            member_view = direct_member_code_view(class_body)
            scoped_class_body = mask_nested_type_bodies(class_body)
            raw_wrapper_names = effective_raw_wrapper_names(class_name)

            category_override = extract_category_override(class_name, class_body)
            if category_override is not None:
                class_category_overrides[class_name] = category_override

            kind_override = extract_kind_override(class_name, class_body)
            if kind_override is not None:
                class_kind_overrides[class_name] = kind_override

            for movement_match in MOVEMENT_OVERRIDE_RE.finditer(member_view):
                property_name = movement_match.group("property")
                quantization = movement_match.group("value")
                if quantization not in {"ByteComponents", "ShortComponents"}:
                    raise SystemExit(
                        f"{class_name}.{property_name}: unsupported movement "
                        f"quantization {quantization}"
                    )
                movement_overrides.setdefault(class_name, {})[
                    property_name
                ] = quantization

            path_match = next(
                (
                    match
                    for match in PATH_EXPRESSION_RE.finditer(class_body_code_view)
                    if member_view.startswith("override", match.start())
                ),
                None,
            )
            if PATH_OVERRIDE_MARKER_RE.search(member_view) and path_match is None:
                raise SystemExit(f"{class_name}.Path: unsupported override shape")
            if path_match:
                expression = class_body[
                    path_match.start("expression"):path_match.end("expression")
                ]
                try:
                    class_paths[class_name] = resolve_path_expression(
                        expression, path_constants, class_name
                    )
                except ValueError as error:
                    raise SystemExit(f"{class_name}.Path: {error}") from error

            raw_base = cm.group("base")
            is_cnc = raw_base and "ClassNetCacheDescriptor" in raw_base
            # Both scans below report into this, so a statement they both see
            # is sorted into rejected_types / decoderless_seen once.
            class_rejected: set[tuple[str, str]] = set()

            configure_re = re.compile(
                r'(?:protected\s+)?override\s+void\s+Configure\(\)'
            )
            configure_match = configure_re.search(member_view)
            if configure_match:
                token_start = configure_match.end()
                while (
                    token_start < len(member_view)
                    and member_view[token_start].isspace()
                ):
                    token_start += 1

                configure_body: str | None = None
                if member_view.startswith("{", token_start):
                    _, cfg_end = find_class_body_range(
                        class_body_code_view, token_start
                    )
                    configure_body = class_body[token_start:cfg_end + 1]
                elif member_view.startswith("=>", token_start):
                    expression_start = token_start + 2
                    expression_end = member_view.find(";", expression_start)
                    if expression_end != -1:
                        configure_body = class_body[
                            expression_start:expression_end + 1
                        ]

                if configure_body is not None:
                    if is_cnc:
                        funcs = extract_cnc_functions(configure_body)
                        funcs.extend(
                            extract_called_cnc_helper_functions(
                                scoped_class_body, configure_body
                            )
                        )
                        if funcs:
                            cnc_functions[class_name] = funcs
                    else:
                        fields = extract_fields_from_block(
                            configure_body, raw_wrapper_names, class_rejected
                        )
                        if fields:
                            class_fields[class_name] = fields

            # Helper methods Configure() calls (AddSharedFields,
            # AddDeathFields, ...) declare fields elsewhere in the class body.
            if not is_cnc:
                helper_fields = extract_fields_from_block(
                    scoped_class_body, raw_wrapper_names, class_rejected
                )
                if helper_fields and class_name not in class_fields:
                    class_fields[class_name] = helper_fields
                elif helper_fields and class_name in class_fields:
                    # Merge helper fields not already captured by Configure()
                    existing_names = {n for n, _, _ in class_fields[class_name]}
                    for n, t, h in helper_fields:
                        if n not in existing_names:
                            class_fields[class_name].append((n, t, h))
                            existing_names.add(n)

            # No type method is a rejection like any other, unless
            # DECODERLESS_PROPERTIES names this class and property.
            for method, statement in class_rejected:
                key = (class_name, _extract_lambda_field_name(statement))
                if method == _NO_TYPE_METHOD and key in DECODERLESS_PROPERTIES:
                    decoderless_seen.add(key)
                else:
                    rejected_types.add((method, statement))

    # An unclassifiable declaration is an unknown, not an absence (the rule
    # EXPORT_GROUP_KIND_POLICY applies to a Kind). It fails before the write,
    # so the previous table survives.
    if rejected_types:
        methods = sorted({method for method, _stmt in rejected_types})
        print(
            f"{len(rejected_types)} AddProperty declaration(s) this "
            f"extractor cannot fully classify: {', '.join(methods)}",
            file=sys.stderr,
        )
        for method, statement in sorted(rejected_types):
            print(f"  .{method}(): {statement}", file=sys.stderr)
        print(
            f"'{_UNNAMED_FIELD}' entries have a known type method but no "
            "extractable field name (a named constant instead of a string "
            "literal or lambda leaf) -- teach `_extract_field_name` that "
            f"shape. '{_NO_TYPE_METHOD}' entries name no method this file can "
            "see: if upstream binds the property with no decoder on purpose, "
            "add (class, property) to DECODERLESS_PROPERTIES with the "
            "evidence; otherwise teach the ladder the call shape. Every other "
            "entry needs its type method added to PRIMITIVE_TYPES (or to the "
            "ladder in extract_fields_from_block) -- dropping any of them "
            "would ship a table that silently omits every field declared that "
            "way.",
            file=sys.stderr,
        )
        return 1

    # The other direction, also before the write: a listed property its class
    # now types would leave DECODERLESS_PROPERTIES describing nothing.
    now_typed = sorted(
        f"{cls}.{prop}"
        for cls, prop in DECODERLESS_PROPERTIES
        if any(name == prop for name, _, _ in class_fields.get(cls, []))
    )
    if now_typed:
        raise SystemExit(
            "DECODERLESS_PROPERTIES lists properties the sources now declare "
            f"with a type: {', '.join(now_typed)}. Remove them there."
        )

    def effective_export_group_kind(cls: str) -> str:
        """C# `Kind` is virtual: the nearest override up the chain (agent
        abilities get Actor from GenericAgentDescriptor), else the default."""
        return nearest(class_kind_overrides, cls, DEFAULT_EXPORT_GROUP_KIND)

    # Phase 2: inheritance. A class inherits its base's fields, merged under
    # its own when it has some (the RPC parameter pattern: child.Configure()
    # calls the parent's AddSharedFields()).
    def get_fields(
        cls: str, visited: set[str] | None = None
    ) -> list[tuple[str, str, int | None]]:
        if visited is None:
            visited = set()
        if cls in visited:
            return []
        visited.add(cls)
        own_fields = class_fields.get(cls, [])
        base = class_bases.get(cls)
        if base:
            parent_fields = get_fields(base, visited)
            if parent_fields and own_fields:
                # Merge: parent fields first, then child (child may override).
                own_names = {name for name, _, _ in own_fields}
                merged = [
                    (n, t, h)
                    for n, t, h in parent_fields
                    if n not in own_names
                ]
                merged.extend(own_fields)
                return merged
            if parent_fields:
                return parent_fields
        return own_fields

    def resolve_virtual_movement(cls: str, field_type: str) -> str:
        if not field_type.startswith(MOVEMENT_TYPE_PREFIX):
            return field_type
        property_name = field_type[len(MOVEMENT_TYPE_PREFIX):-1]
        value = next((movement_overrides[c][property_name] for c in ancestors(cls)
                      if property_name in movement_overrides.get(c, {})), None)
        if value is None:
            raise SystemExit(
                f"{cls}: cannot resolve virtual movement quantization "
                f"{property_name}"
            )
        return rep_movement_type(value)

    # Phase 3: build final entries
    entries: list[tuple[str, str, str]] = []  # (group_path, field_name, rust_type)
    handle_entries: list[tuple[str, int, str]] = []
    groups_seen: set[str] = set()

    # 3a: ExportGroupDescriptor entries (RepLayout + RPC parameter groups).
    # Only this phase applies EXPORT_GROUP_KIND_POLICY: 3b/3c come from
    # ClassNetCacheDescriptor, a C# hierarchy with no Kind.
    kind_dropped: Counter[str] = Counter()
    #: (group_path, field_name) -> the declared type. Classes sharing a Path
    #: must agree, or class sort order would pick the type (the dedup below
    #: keeps the first entry). Scoped to 3a: 3b/3c emit Skip for function
    #: names, which the dedup rightly settles.
    declared_type: dict[tuple[str, str], tuple[str, str]] = {}
    for class_name, path in sorted(class_paths.items()):
        fields = get_fields(class_name)
        if not fields:
            continue
        # Skip classes that are CNC descriptors (they go to 3b)
        base = class_bases.get(class_name, "")
        if "ClassNetCacheDescriptor" in base:
            continue
        kind = effective_export_group_kind(class_name)
        kept = fields_for_export_group_kind(class_name, path, kind, fields)
        if len(kept) != len(fields):
            kind_dropped[kind] += len(fields) - len(kept)
        fields = kept
        if not fields:
            # Nothing in the group can be looked up, so it leaves groups_seen.
            continue
        groups_seen.add(path)
        for field_name, rust_type, literal_handle in fields:
            rust_type = resolve_virtual_movement(class_name, rust_type)
            previous = declared_type.get((path, field_name))
            if previous is not None and previous[0] != rust_type:
                raise SystemExit(
                    "conflicting declared types for "
                    f"{path} field {field_name!r}: {previous[0]} (from "
                    f"{previous[1]}) vs {rust_type} (from {class_name})"
                )
            declared_type[(path, field_name)] = (rust_type, class_name)
            entries.append((path, field_name, rust_type))
            if literal_handle is not None:
                handle_entries.append((path, literal_handle, field_name))

    # 3b: a Skip entry per ClassNetCache function, so analyze_coverage counts
    # the group and the table names the function; its parameters are typed by
    # their own groups in 3a.
    for class_name, funcs in sorted(cnc_functions.items()):
        path = class_paths.get(class_name)
        if not path:
            continue
        groups_seen.add(path)
        for func_name in funcs:
            entries.append((path, func_name, "FieldType::Skip"))

    def has_effective_agent_category(cls: str) -> bool:
        return bool(
            nearest(class_category_overrides, cls, frozenset()) & {"Agent", "All"}
        )

    # 3c: runtime-created caches, one per Agent-category descriptor at
    # `descriptor.Path + "_ClassNetCache"` with a shared RpcDescriptor; suffix
    # and RPC name come from source, no path or alias is baked in.
    for suffix, function_name in sorted(set(runtime_cnc_specs)):
        for class_name, path in sorted(class_paths.items()):
            if not has_effective_agent_category(class_name):
                continue
            cache_path = path + suffix
            groups_seen.add(cache_path)
            entries.append((cache_path, function_name, "FieldType::Skip"))

    # Concrete calls to simple static cache factories carry a path constant
    # and one RPC name. They have no descriptor subclass for phase 3b to see.
    for cache_path, function_name in sorted(set(static_cache_specs)):
        groups_seen.add(cache_path)
        entries.append((cache_path, function_name, "FieldType::Skip"))

    # Deduplicate entries (same path + field_name can appear if parent+child both declare)
    first: dict[tuple[str, str], tuple[str, str, str]] = {}
    for entry in entries:
        first.setdefault(entry[:2], entry)
    entries = list(first.values())

    handle_by_key: dict[tuple[str, int], str] = {}
    for group_path, handle, field_name in handle_entries:
        previous = handle_by_key.setdefault((group_path, handle), field_name)
        if previous != field_name:
            raise SystemExit(
                "conflicting explicit handles for "
                f"{group_path} handle {handle}: {previous!r} vs {field_name!r}"
            )
    handle_entries = [
        (group_path, handle, field_name)
        for (group_path, handle), field_name in handle_by_key.items()
    ]

    # Sort by (group_path, field_name) for binary search
    entries.sort(key=lambda e: (e[0], e[1]))
    handle_entries.sort(key=lambda e: (e[0], e[1]))

    raw_count = sum(1 for _, _, t in entries if t == "FieldType::Raw")
    skip_count = sum(1 for _, _, t in entries if t == "FieldType::Skip")
    type_counts: Counter[str] = Counter()
    for _, _, t in entries:
        if t != "FieldType::Raw" and t != "FieldType::Skip":
            base_type = t.split("{")[0].strip().replace("FieldType::", "")
            type_counts[base_type] += 1

    print(f"Groups: {len(groups_seen)}")
    print(f"Fields: {len(entries)}")
    print(f"  Raw (custom decoder): {raw_count}")
    print(f"  Skip (ignored): {skip_count}")
    print(f"  Typed: {len(entries) - raw_count - skip_count}")
    print(f"Handle aliases: {len(handle_entries)}")
    # Printed even at zero (CLAUDE.md), so a policy that let everything through
    # cannot pass for a run with nothing to drop; 8 on the vendored tree.
    print(f"Declared properties dropped by ExportGroupKind: "
          f"{sum(kind_dropped.values())}")
    for kind, count in sorted(kind_dropped.items()):
        print(f"  {kind}: {count}")
    # Same rule: 3 on the vendored tree, one per DECODERLESS_PROPERTIES entry;
    # fewer means the scan stopped seeing them.
    print(f"Declared without a type method (no entry): {len(decoderless_seen)}")
    for cls, prop in sorted(decoderless_seen):
        print(f"  {cls}.{prop}")
    print("Type distribution:")
    for t, c in type_counts.most_common():
        print(f"  {t}: {c}")

    lines = [
        "// Overlay table mapping (group_path, field_name) -> FieldType.",
        "//",
        "// GENERATED by tools/extract_descriptors.py -- do not edit by hand.",
        f"// {len(entries)} entries from {len(groups_seen)} groups.",
        f"// Raw/Custom: {raw_count}, Skip: {skip_count}, Typed: {len(entries) - raw_count - skip_count}.",
        "",
        "use crate::decode::FieldType;",
        "use crate::overlay::{OverlayEntry, OverlayHandleEntry};",
        "use crate::types::{RotatorQuantization, VectorQuantization};",
        "",
        f"pub static OVERLAY_TABLE: [OverlayEntry; {len(entries)}] = [",
    ]
    for group_path, field_name, rust_type in entries:
        gp = group_path.replace("\\", "\\\\").replace('"', '\\"')
        fn = field_name.replace("\\", "\\\\").replace('"', '\\"')
        lines.append(
            f'    OverlayEntry {{ group_path: "{gp}", '
            f'field_name: "{fn}", '
            f'field_type: {rust_type} }},'
        )
    lines.append("];")
    lines.append("")
    lines.append(
        f"pub static OVERLAY_HANDLE_TABLE: [OverlayHandleEntry; "
        f"{len(handle_entries)}] = ["
    )
    for group_path, handle, field_name in handle_entries:
        gp = group_path.replace("\\", "\\\\").replace('"', '\\"')
        fn = field_name.replace("\\", "\\\\").replace('"', '\\"')
        lines.append(
            f'    OverlayHandleEntry {{ group_path: "{gp}", '
            f'handle: {handle}, field_name: "{fn}" }},'
        )
    lines.append("];")
    lines.append("")

    atomic_write_text(out_path, "\n".join(lines))
    print(f"\nwrote {out_path} ({len(entries)} entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
