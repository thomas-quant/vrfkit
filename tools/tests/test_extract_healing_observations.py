import copy, json, os, struct, sys, tempfile, unittest
from pathlib import Path
import pyarrow as pa, pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import extract_healing_observations as tool

SCHEMA = pa.schema(
    [
        (n, t)
        for n, t in [
            ("time_ms", pa.uint32()),
            ("packet_id", pa.uint32()),
            ("channel_index", pa.uint32()),
            ("actor_net_guid", pa.uint32()),
            ("object_net_guid", pa.uint32()),
            ("group_path", pa.string()),
            ("handle", pa.uint32()),
            ("field_name", pa.string()),
            ("compatible_checksum", pa.uint32()),
            ("bit_count", pa.uint32()),
            ("raw_bits", pa.binary()),
            ("value_i64", pa.int64()),
            ("value_f64", pa.float64()),
            ("value_bool", pa.bool_()),
            ("value_str", pa.string()),
        ]
    ]
)
CHECKPOINT_SCHEMA = pa.schema(
    [("checkpoint_index", pa.uint32()), ("checkpoint_id", pa.string()), *SCHEMA]
)


def ip(v):
    out = []
    while True:
        q = v & 127
        v >>= 7
        out.append((q << 1) | (1 if v else 0))
        if not v:
            return out


def ref(v):
    return bytes(ip(v)), 8 * len(ip(v))


def arr(fields):
    bits = []

    def byte(x):
        bits.extend((x >> i) & 1 for i in range(8))

    def raw(x, w):
        bits.extend((x[i // 8] >> (i % 8)) & 1 for i in range(w))

    byte(2)
    byte(2)
    for h, w, x in fields:
        for z in ip(h + 1):
            byte(z)
        for z in ip(w):
            byte(z)
        raw(x, w)
    byte(0)
    byte(0)
    b = bytearray((len(bits) + 7) // 8)
    for i, x in enumerate(bits):
        b[i // 8] |= x << (i % 8)
    return bytes(b), len(bits)


def row(name, crc=None, raw=b"", bits=0, **kw):
    x = {
        "time_ms": 1000,
        "packet_id": 2,
        "channel_index": 3,
        "actor_net_guid": 40,
        "object_net_guid": 41,
        "group_path": tool.OUTER_GROUP,
        "handle": 6,
        "field_name": name,
        "compatible_checksum": crc,
        "bit_count": bits,
        "raw_bits": raw,
        "value_i64": None,
        "value_f64": None,
        "value_bool": None,
        "value_str": None,
    }
    x.update(kw)
    return x


def fixture(value=-0.0, causer=True):
    f = struct.pack("<f", value)
    cr, cw = ref(60)
    fields = [(2, cw, cr), (3, 32, f), (4, 32, f), (5, 1, b"\1")]
    parent, pw = arr(fields)
    rows = [
        row("MulticastNotifyHeal.HealTaken", 1894010429, f, 32, value_f64=value),
        row(
            "MulticastNotifyHeal.LifeChangeBySection[0].ChangedComponent",
            None,
            cr,
            cw,
            value_i64=60,
        ),
        row(
            "MulticastNotifyHeal.LifeChangeBySection[0].LifeResult",
            None,
            f,
            32,
            value_f64=value,
        ),
        row(
            "MulticastNotifyHeal.LifeChangeBySection[0].DeltaLife",
            None,
            f,
            32,
            value_f64=value,
        ),
        row(
            "MulticastNotifyHeal.LifeChangeBySection[0].bAliveAfterChange",
            None,
            b"\1",
            1,
            value_bool=True,
        ),
        row("MulticastNotifyHeal.LifeChangeBySection", 163390906, parent, pw),
    ]
    # Typed like the parser types them: the scoped ObjectNetGuid entries in
    # tools/fixtures/scoped_type_evidence.json put the packed value in value_i64.
    for n, h, c, v in [
        ("EventInstigator", 7, 3087885251, 12),
        ("EventInstigatorPawn", 8, 3901949544, 50),
    ]:
        z, w = ref(v)
        rows.append(row("MulticastNotifyHeal." + n, c, z, w, value_i64=v))
    if causer:
        z, w = ref(70)
        rows.append(
            row("MulticastNotifyHeal.HealCauser", 546618027, z, w, value_i64=70)
        )
    return rows


def manifest():
    return {
        "net_field_export_groups": [
            {
                "path": tool.OUTER_GROUP,
                "fields": [
                    {
                        "handle": 6,
                        "name": "MulticastNotifyHeal",
                        "compatible_checksum": 791426194,
                    }
                ],
            },
            {
                "path": tool.PARAM_GROUP,
                "fields": [
                    {"handle": h, "name": n, "compatible_checksum": c}
                    for h, (n, c) in tool.DECL.items()
                ],
            },
        ],
        "players": [
            {"character_net_guid": 40, "subject": "recipient"},
            {"character_net_guid": 50, "subject": "source"},
        ],
    }


class Tests(unittest.TestCase):
    def make(self, rows=None, actors=None, checkpoint=None):
        td = tempfile.TemporaryDirectory()
        p = Path(td.name)
        (p / "manifest.json").write_text(json.dumps(manifest()))
        pq.write_table(
            pa.Table.from_pylist(rows or fixture(), schema=SCHEMA), p / "fields.parquet"
        )
        checkpoint_rows = [
            {"checkpoint_index": 0, "checkpoint_id": "cp-0", **item}
            for item in (checkpoint or [])
        ]
        pq.write_table(
            pa.Table.from_pylist(checkpoint_rows, schema=CHECKPOINT_SCHEMA),
            p / "checkpoint_fields.parquet",
        )
        actor_schema = pa.schema(
            [
                ("time_ms", pa.uint32()),
                ("packet_id", pa.uint32()),
                ("channel_index", pa.uint32()),
                ("actor_net_guid", pa.uint32()),
                ("event", pa.string()),
                ("class_path", pa.string()),
            ]
        )
        pq.write_table(
            pa.Table.from_pylist(actors or [], schema=actor_schema),
            p / "actors.parquet",
        )
        guid_schema = pa.schema([("net_guid", pa.uint32()), ("path", pa.string())])
        pq.write_table(
            pa.Table.from_pylist(
                [{"net_guid": 60, "path": "HealthDamageSection"}], schema=guid_schema
            ),
            p / "net_guids.parquet",
        )
        return td, p

    def test_signed_zero_missing_causer_keeps_valid_amount_and_edges(self):
        td, p = self.make(fixture(causer=False))
        self.addCleanup(td.cleanup)
        d = tool.extract(p)
        o = d["observations"][0]
        self.assertEqual(o["amount"]["status"], "validated")
        self.assertEqual(struct.pack("<f", o["amount"]["heal_taken"]), b"\0\0\0\x80")
        self.assertEqual(o["source_corroboration"]["status"], "absent")
        self.assertEqual(
            o["source_corroboration"]["event_instigator_pawn"]["value"], 50
        )
        self.assertTrue(o["recipient_corroboration"]["static_manifest_character"])
        self.assertIn(
            "serialized_heal_amount_sum",
            next(k for k in d["summaries"] if "recipient" in k),
        )

    def test_parent_child_mismatch_is_retained_invalid(self):
        rows = fixture()
        rows[2]["raw_bits"] = struct.pack("<f", 2.0)
        rows[2]["value_f64"] = 2.0
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        o = tool.extract(p)["observations"][0]
        self.assertEqual(o["amount"]["status"], "invalid")
        self.assertEqual(o["amount"]["error"], "parent/child raw mismatch")
        self.assertEqual(len(o["source_rows"]), len(rows))

    def test_duplicate_and_disjoint_groups_are_ambiguous(self):
        rows = fixture()
        duplicate = copy.deepcopy(rows[0])
        other = copy.deepcopy(rows[0])
        other["actor_net_guid"] = 99
        other["object_net_guid"] = 100
        td, p = self.make(rows[:1] + [other] + rows[1:] + [duplicate])
        self.addCleanup(td.cleanup)
        obs = tool.extract(p)["observations"]
        target = next(x for x in obs if x["identity"]["actor_net_guid"] == 40)
        self.assertIn("duplicate_same_coordinate_member", target["ambiguity_reasons"])
        self.assertIn("disjoint_same_coordinate_group", target["ambiguity_reasons"])

    def test_same_time_lifecycle_is_unresolved_and_direct_edges_remain(self):
        actors = [
            {
                "time_ms": 1000,
                "packet_id": 1,
                "channel_index": 9,
                "actor_net_guid": 70,
                "event": "open",
                "class_path": "/Game/Characters/X/Abilities/A.A_C",
            }
        ]
        td, p = self.make(actors=actors)
        self.addCleanup(td.cleanup)
        s = tool.extract(p)["observations"][0]["source_corroboration"]
        self.assertEqual(s["status"], "lifecycle_boundary_same_time")
        self.assertEqual(s["event_instigator"]["value"], 12)

    def test_raw_typed_mismatch_fails_atomically(self):
        rows = fixture(1.0)
        rows[0]["value_f64"] = 2.0
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        out = p / "out.json"
        out.write_text("old")
        self.assertEqual(tool.main(["--export", str(p), "--out", str(out)]), 1)
        self.assertEqual(out.read_text(), "old")

    def test_checkpoint_rows_are_preserved_separately(self):
        first = {
            "checkpoint_index": 2,
            "checkpoint_id": "cp-a",
            **copy.deepcopy(fixture()[0]),
        }
        second = {
            "checkpoint_index": 7,
            "checkpoint_id": "cp-b",
            **copy.deepcopy(fixture()[0]),
        }
        cp = [first, second]
        td, p = self.make(checkpoint=cp)
        self.addCleanup(td.cleanup)
        d = tool.extract(p)
        self.assertEqual(d["counts"]["checkpoint_selected_rows"], 2)
        self.assertEqual(
            [
                (x["checkpoint_index"], x["checkpoint_id"])
                for x in d["checkpoint_observations"]
            ],
            [(2, "cp-a"), (7, "cp-b")],
        )

    def test_optional_declarations_may_be_jointly_absent_with_no_source_rows(self):
        td, p = self.make(fixture(causer=False)[:6])
        self.addCleanup(td.cleanup)
        data = manifest()
        data["net_field_export_groups"][1]["fields"] = [
            x for x in data["net_field_export_groups"][1]["fields"] if x["handle"] <= 5
        ]
        (p / "manifest.json").write_text(json.dumps(data))
        result = tool.extract(p)
        self.assertEqual(result["counts"]["amount_validated"], 1)
        self.assertEqual(
            result["observations"][0]["source_corroboration"]["status"], "absent"
        )

    def test_observed_undeclared_optional_edge_and_changed_declaration_fail(self):
        td, p = self.make()
        self.addCleanup(td.cleanup)
        data = manifest()
        data["net_field_export_groups"][1]["fields"] = [
            x for x in data["net_field_export_groups"][1]["fields"] if x["handle"] != 9
        ]
        (p / "manifest.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(tool.IntegrityError, "lacks its declaration"):
            tool.extract(p)
        data = manifest()
        next(
            x for x in data["net_field_export_groups"][1]["fields"] if x["handle"] == 7
        )["compatible_checksum"] = 1
        (p / "manifest.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(tool.InputError, "optional heal parameter"):
            tool.extract(p)

    def test_foreign_schema_is_preserved_but_not_validated(self):
        rows = fixture()
        rows[0]["group_path"] = "/Script/Foreign"
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        o = tool.extract(p)["observations"][0]
        self.assertEqual(o["amount"]["status"], "invalid")
        self.assertIn("foreign_or_invalid_schema", o["ambiguity_reasons"])
        self.assertEqual(o["source_rows"][0]["group_path"], "/Script/Foreign")

    def test_owner_evidence_is_raw_validated_and_reopen_is_rejected(self):
        cls = "/Game/Characters/X/Abilities/A.A_C"
        actors = [
            {
                "time_ms": 100,
                "packet_id": 1,
                "channel_index": 9,
                "actor_net_guid": 70,
                "event": "open",
                "class_path": cls,
            },
            {
                "time_ms": 200,
                "packet_id": 2,
                "channel_index": 9,
                "actor_net_guid": 70,
                "event": "open",
                "class_path": cls,
            },
        ]
        z, w = ref(50)
        owner = row(
            "Owner",
            None,
            z,
            w,
            time_ms=300,
            packet_id=3,
            channel_index=9,
            actor_net_guid=70,
            object_net_guid=None,
            group_path=cls,
            handle=0,
            value_i64=50,
        )
        td, p = self.make(fixture() + [owner], actors)
        self.addCleanup(td.cleanup)
        s = tool.extract(p)["observations"][0]["source_corroboration"]
        self.assertEqual(s["status"], "actor_reopened_without_unique_active_instance")
        bad = copy.deepcopy(owner)
        bad["value_i64"] = 51
        td2, p2 = self.make(fixture() + [bad], actors[:1])
        self.addCleanup(td2.cleanup)
        with self.assertRaises(tool.IntegrityError):
            tool.extract(p2)

    def test_near_f32_value_is_rejected_even_when_repacking_rounds_equal(self):
        rows = fixture(1.0)
        rows[0]["value_f64"] = 1.0 + 1e-9
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        with self.assertRaises(tool.IntegrityError):
            tool.extract(p)

    def test_output_may_not_alias_an_input(self):
        td, p = self.make()
        self.addCleanup(td.cleanup)
        source = p / "fields.parquet"
        before = source.read_bytes()
        self.assertEqual(tool.main(["--export", str(p), "--out", str(source)]), 1)
        self.assertEqual(source.read_bytes(), before)

    def test_heal_causer_typed_corruption_fails_atomically(self):
        rows = fixture()
        rows[-1]["value_i64"] = 71
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        out = p / "out.json"
        out.write_text("old")
        self.assertEqual(tool.main(["--export", str(p), "--out", str(out)]), 1)
        self.assertEqual(out.read_text(), "old")

    def test_typed_instigator_edges_are_present_and_raw_checked(self):
        td, p = self.make()
        self.addCleanup(td.cleanup)
        d = tool.extract(p)
        s = d["observations"][0]["source_corroboration"]
        for key, value in (("event_instigator", 12), ("event_instigator_pawn", 50)):
            with self.subTest(edge=key):
                self.assertEqual(s[key]["status"], "present")
                self.assertEqual(s[key]["value"], value)
                self.assertEqual(
                    d["counts"]["source_edge_status"][key],
                    {"present": 1, "null": 0, "absent": 0, "duplicate": 0, "invalid": 0},
                )
        # The established target is stated, including that it never joins to
        # an opened actor -- an unresolved join is not a decode fault.
        semantics = s["event_instigator"]["semantics"]
        self.assertIn("PlayerController", semantics)
        self.assertIn("not a decode fault", semantics)

    def test_instigator_typed_corruption_fails_atomically(self):
        for name in ("EventInstigator", "EventInstigatorPawn"):
            with self.subTest(field=name):
                rows = fixture()
                target = next(
                    r for r in rows if r["field_name"] == "MulticastNotifyHeal." + name
                )
                target["value_i64"] += 1
                td, p = self.make(rows)
                self.addCleanup(td.cleanup)
                out = p / "out.json"
                out.write_text("old")
                self.assertEqual(tool.main(["--export", str(p), "--out", str(out)]), 1)
                self.assertEqual(out.read_text(), "old")

    def test_an_invalid_edge_is_counted_not_just_labelled(self):
        # A malformed reference window keeps the amount and marks the edge
        # invalid. That used to move no counter at all; it must reach counts.
        rows = fixture()
        target = next(
            r for r in rows if r["field_name"] == "MulticastNotifyHeal.EventInstigator"
        )
        target["bit_count"] += 8
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        d = tool.extract(p)
        o = d["observations"][0]
        self.assertEqual(o["amount"]["status"], "validated")
        self.assertEqual(o["source_corroboration"]["event_instigator"]["status"], "invalid")
        self.assertEqual(d["counts"]["source_edge_status"]["event_instigator"]["invalid"], 1)
        self.assertEqual(d["counts"]["source_edge_status"]["causer"]["present"], 1)

    def test_untyped_instigator_from_an_older_export_fails_loudly(self):
        # An export from a parser that predates the scoped typing carries the
        # raw window only. That must stop the run, not turn the edge "invalid"
        # behind a successful exit.
        for name in ("EventInstigator", "EventInstigatorPawn"):
            with self.subTest(field=name):
                rows = fixture()
                target = next(
                    r for r in rows if r["field_name"] == "MulticastNotifyHeal." + name
                )
                target["value_i64"] = None
                td, p = self.make(rows)
                self.addCleanup(td.cleanup)
                with self.assertRaisesRegex(tool.IntegrityError, "untyped"):
                    tool.extract(p)

    def test_swapped_children_and_unrelated_gap_are_rejected(self):
        rows = fixture()
        rows[1], rows[2] = rows[2], rows[1]
        td, p = self.make(rows)
        self.addCleanup(td.cleanup)
        self.assertEqual(
            tool.extract(p)["observations"][0]["amount"]["status"], "invalid"
        )
        rows = fixture()
        rows.insert(2, row("Unrelated", raw=b"\0", bits=1))
        td2, p2 = self.make(rows)
        self.addCleanup(td2.cleanup)
        observation = tool.extract(p2)["observations"][0]
        self.assertIn(
            "disjoint_same_coordinate_group", observation["ambiguity_reasons"]
        )

    def test_selected_batch_ordinals_include_filtered_rows_and_boundary(self):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        path = Path(td.name) / "fields.parquet"
        filler = row("Unrelated", raw=b"\0", bits=1)
        rows = [filler] * 65534 + [fixture()[0], filler, fixture()[1]]
        pq.write_table(
            pa.Table.from_pylist(rows, schema=SCHEMA), path, row_group_size=65536
        )
        got = list(tool.iter_selected_fields(path, False))
        self.assertEqual([ordinal for ordinal, _ in got], [65534, 65536])

    def test_hardlink_output_alias_is_rejected(self):
        td, p = self.make()
        self.addCleanup(td.cleanup)
        source = p / "fields.parquet"
        alias = p / "alias.json"
        os.link(source, alias)
        before = source.read_bytes()
        self.assertEqual(tool.main(["--export", str(p), "--out", str(alias)]), 1)
        self.assertEqual(source.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
