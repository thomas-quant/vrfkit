"""Tests for the read-only descriptor-source audit."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "tools" / "compare_descriptor_sources.py"
sys.path.insert(0, str(REPO / "tools"))
import compare_descriptor_sources as audit  # noqa: E402


def descriptor(path: Path, fields: str) -> None:
    target = path / "src" / "Replay.Valorant" / "Thing.cs"
    target.parent.mkdir(parents=True)
    target.write_text("""
public sealed class ThingDescriptor : ExportGroupDescriptor<ThingDescriptor>
{
    public override string Path => \"/Script/ShooterGame.Thing\";
    protected override void Configure()
    {
%s
    }
}
""" % fields, encoding="utf-8")


class CompareDescriptorSourcesTests(unittest.TestCase):
    def test_versioned_custom_decoder_is_visible_in_audit(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            descriptor(root, '''
        // new VersionedDefinition<IFieldDecoderDescriptor>(ignored)
        AddProperty(x => x.Results).Decode(
            new VersionedDefinition<IFieldDecoderDescriptor>(new LegacyDecoder())
                .From(new ReplayReleaseVersion(13, 5), new CurrentDecoder()));
''')
            source_dir = root / "src" / "Replay.Valorant"
            records = audit.record_versioned_decoders(source_dir)
            self.assertEqual(len(records), 1)
            self.assertEqual(records[0]["path"], "Thing.cs")
            self.assertIn("FieldType::Raw", records[0]["reason"])

    def test_reports_type_addition_removal_and_downstream_loss(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            old, new = root / "old", root / "new"
            descriptor(old, '        AddPropertyHandle(4, x => x.Kept).Float();\n        AddProperty(x => x.Removed).Int32();')
            descriptor(new, '        AddPropertyHandle(4, x => x.Renamed).Float();\n        AddProperty(x => x.Kept).Bool();\n        AddProperty(x => x.Added).UInt32();')
            table = root / "table.rs"
            table.write_text('''pub static OVERLAY_TABLE: [OverlayEntry; 2] = [
 OverlayEntry { group_path: "/Script/ShooterGame.Thing", field_name: "Kept", field_type: FieldType::Float },
 OverlayEntry { group_path: "/Script/ShooterGame.Thing", field_name: "LocalOnly", field_type: FieldType::Raw },
];
pub static OVERLAY_HANDLE_TABLE: [OverlayHandleEntry; 2] = [
 OverlayHandleEntry { group_path: "/Script/ShooterGame.Thing", handle: 4, field_name: "Kept" },
 OverlayHandleEntry { group_path: "/Script/ShooterGame.Thing", handle: 99, field_name: "LocalHandle" },
];
''', encoding="utf-8")
            output = root / "audit.json"
            result = subprocess.run([sys.executable, str(TOOL), "--baseline", str(old), "--candidate", str(new),
                "--downstream-table", str(table), "--output", str(output)], capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertTrue(report["read_only"])
            self.assertEqual({row["kind"] for row in report["field_changes"]}, {"added", "removed", "type_changed"})
            self.assertEqual(report["handle_changes"][0]["kind"], "renamed")
            self.assertEqual(report["csharp_source_changes"][0]["kind"], "modified")
            self.assertEqual({row["field_name"] for row in report["downstream_regeneration_risks"]
                              if row["table"] == "entries" and row["reason"] == "would_be_lost_by_wholesale_regeneration"}, {"LocalOnly"})
            self.assertTrue(any(row["reason"] == "would_be_overwritten_by_wholesale_regeneration"
                                and row["field_name"] == "Kept"
                                for row in report["downstream_regeneration_risks"]))
            self.assertTrue(any(row["reason"] == "would_be_remapped_by_wholesale_regeneration"
                                and row["handle"] == 4
                                for row in report["downstream_regeneration_risks"]))
            self.assertEqual(report["inputs"]["downstream_table"]["sha256"], audit.sha256_file(table))
            self.assertNotEqual(report["sources"]["baseline"]["csharp_manifest_sha256"],
                                report["sources"]["candidate"]["csharp_manifest_sha256"])
            self.assertTrue(all(row["disposition"] == "review_candidate" for row in report["field_changes"]))

    def test_parse_table_normalizes_nested_types_and_rejects_duplicates(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            generated = root / "generated.rs"
            formatted = root / "formatted.rs"
            generated.write_text('''OverlayEntry { group_path: "g", field_name: "nested", field_type: FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents } },
OverlayHandleEntry { group_path: "g", handle: 1, field_name: "nested" },
''', encoding="utf-8")
            formatted.write_text('''OverlayEntry {
 group_path: "g",
 field_name: "nested",
 field_type: FieldType::RepMovement {
   rotation: RotatorQuantization::ByteComponents,
 },
},
OverlayHandleEntry {
 group_path: "g",
 handle: 1,
 field_name: "nested",
},
''', encoding="utf-8")
            self.assertEqual(audit.parse_table(generated), audit.parse_table(formatted))
            duplicate = root / "duplicate.rs"
            duplicate.write_text(generated.read_text(encoding="utf-8") + generated.read_text(encoding="utf-8"), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate overlay entry"):
                audit.parse_table(duplicate)
            duplicate_handle = root / "duplicate_handle.rs"
            duplicate_handle.write_text('''OverlayHandleEntry { group_path: "g", handle: 1, field_name: "one" },
OverlayHandleEntry { group_path: "g", handle: 1, field_name: "two" },
''', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate overlay handle"):
                audit.parse_table(duplicate_handle)

    def test_git_revision_input_leaves_checkout_unchanged(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            descriptor(repo, '        AddProperty(x => x.Value).Float();')
            for args in (("init",), ("config", "user.email", "test@example.com"),
                         ("config", "user.name", "Test"), ("add", "."), ("commit", "-m", "fixture")):
                subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True)
            before_head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], check=True,
                capture_output=True, text=True).stdout
            before_status = subprocess.run(["git", "-C", str(repo), "status", "--porcelain=v1"], check=True,
                capture_output=True, text=True).stdout
            output = root / "audit.json"
            result = subprocess.run([sys.executable, str(TOOL), "--baseline", f"{repo}::HEAD",
                "--candidate", f"{repo}::HEAD", "--output", str(output)], capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(result.returncode, 0, result.stderr)
            after_head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], check=True,
                capture_output=True, text=True).stdout
            after_status = subprocess.run(["git", "-C", str(repo), "status", "--porcelain=v1"], check=True,
                capture_output=True, text=True).stdout
            self.assertEqual((after_head, after_status), (before_head, before_status))

    def test_git_revision_input_does_not_depend_on_the_tar_on_path(self):
        """A revision is unpacked in-process, so a tar that always fails,
        placed first on PATH, must not matter (Git Bash's GNU tar refuses a
        `C:\\` destination)."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            descriptor(repo, '        AddProperty(x => x.Value).Float();')
            for args in (("init",), ("config", "user.email", "test@example.com"),
                         ("config", "user.name", "Test"), ("add", "."), ("commit", "-m", "fixture")):
                subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True)
            broken = root / "broken-bin"
            broken.mkdir()
            if os.name == "nt":
                (broken / "tar.bat").write_text("@exit /b 1\r\n", encoding="ascii")
            else:
                script = broken / "tar"
                script.write_text("#!/bin/sh\nexit 1\n", encoding="ascii")
                script.chmod(0o755)
            env = dict(os.environ, PATH=str(broken) + os.pathsep + os.environ.get("PATH", ""))
            output = root / "audit.json"
            result = subprocess.run([sys.executable, str(TOOL), "--baseline", f"{repo}::HEAD",
                "--candidate", f"{repo}::HEAD", "--output", str(output)], capture_output=True,
                text=True, encoding="utf-8", env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(output.exists())

    def test_a_localized_git_failure_keeps_its_message(self):
        """git on a Korean-locale Windows writes cp949; a failing call must
        surface git's message, not a UnicodeDecodeError naming a byte."""
        localized = "fatal: \uc798\ubabb\ub41c \uac1c\uccb4 \uc774\ub984".encode("cp949")
        real_run = subprocess.run

        def fake_git(failing):
            def run(cmd, *args, **kwargs):
                # A real process, so the tool's own text/encoding/errors
                # arguments do the decoding.
                if failing in cmd:
                    stub = f"import sys; sys.stderr.buffer.write({localized!r}); sys.exit(128)"
                else:
                    stub = "print('0' * 40)"
                return real_run([sys.executable, "-c", stub], *args, **kwargs)
            return run

        with tempfile.TemporaryDirectory() as temp:
            for failing, message in (("rev-parse", "git rev-parse"),
                                      ("archive", "git archive failed")):
                with self.subTest(failing=failing), \
                        mock.patch.object(audit.subprocess, "run", fake_git(failing)):
                    with self.assertRaises(ValueError) as caught:
                        audit.source_from_spec(f"{temp}::HEAD", Path(temp))
                    self.assertNotIsInstance(caught.exception, UnicodeDecodeError)
                    self.assertIn(message, str(caught.exception))
                    self.assertIn("fatal:", str(caught.exception))

    def test_invalid_source_fails_without_writing_output(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            output = root / "audit.json"
            result = subprocess.run([sys.executable, str(TOOL), "--baseline", str(root / "missing"),
                "--candidate", str(root / "missing"), "--output", str(output)], capture_output=True, text=True, encoding="utf-8")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
