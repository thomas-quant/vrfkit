"""Every text-mode file read or write in tools/*.py names its encoding.

Without `encoding=`, Python decodes with the locale's code page. On a cp949
Windows host that made extract_healing_observations.py exit 1 on a
manifest.json whose source_file held Hangul, while every sibling tool read the
same file. Running the tools cannot catch this in CI: every job sets
PYTHONUTF8=1, which hides the locale. So this reads the source instead.

Checked: Path.read_text/write_text, builtin/io open, os.fdopen and Path.open,
unless the mode is binary. `<module>.open` (tarfile.open, ...) has another
signature and is skipped; subprocess text=True is not file I/O.
"""
import ast
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]


def _mode(node: ast.Call, position: int) -> ast.expr | None:
    for keyword in node.keywords:
        if keyword.arg == "mode":
            return keyword.value
    return node.args[position] if len(node.args) > position else None


def unencoded_text_io(source: str, filename: str = "<source>") -> tuple[list[int], int]:
    """Lines of text-mode file I/O calls without `encoding=`, and the call count."""
    tree = ast.parse(source, filename)
    modules = {alias.asname or alias.name.split(".")[0]
               for node in ast.walk(tree) if isinstance(node, ast.Import)
               for alias in node.names}
    flagged, checked = [], 0
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Attribute) and func.attr in ("read_text", "write_text"):
            mode = None
        elif isinstance(func, ast.Name) and func.id == "open":
            mode = _mode(node, 1)
        elif isinstance(func, ast.Attribute) and func.attr in ("open", "fdopen"):
            owner = func.value.id if isinstance(func.value, ast.Name) else None
            if func.attr == "fdopen" or owner == "io":
                mode = _mode(node, 1)
            elif owner in modules:
                continue
            else:
                mode = _mode(node, 0)
        else:
            continue
        if isinstance(mode, ast.Constant) and isinstance(mode.value, str) and "b" in mode.value:
            continue
        checked += 1
        # `**kwargs` could carry an encoding, but nothing here can see it.
        if not any(keyword.arg == "encoding" for keyword in node.keywords):
            flagged.append(node.lineno)
    return flagged, checked


class TextIoEncodingTests(unittest.TestCase):
    def test_every_text_file_call_in_tools_names_its_encoding(self):
        scripts = sorted(TOOLS.glob("*.py"))
        # A glob that found nothing would pass vacuously.
        self.assertIn(TOOLS / "extract_healing_observations.py", scripts)
        flagged, checked = [], 0
        for script in scripts:
            lines, count = unencoded_text_io(script.read_text(encoding="utf-8"), str(script))
            flagged += [f"{script.name}:{line}" for line in lines]
            checked += count
        self.assertGreater(checked, 50)
        self.assertEqual(flagged, [], "text I/O without encoding= decodes with the locale")

    def test_the_scanner_flags_each_unencoded_shape(self):
        cases = {
            "p.read_text()": 1,
            "p.write_text(s)": 1,
            "open(name)": 1,
            "open(name, 'w')": 1,
            "p.open()": 1,
            "p.open(mode='a')": 1,
            "os.fdopen(fd, 'w')": 1,
            "io.open(name, 'r')": 1,
            "p.read_text(encoding='utf-8')": 0,
            "p.write_text(s, encoding='utf-8', newline='\\n')": 0,
            "open(name, 'rb')": 0,
            "p.open('xb')": 0,
            "os.fdopen(fd, 'w', encoding=e)": 0,
            "import tarfile\ntarfile.open(fileobj=f)": 0,
        }
        for source, expected in cases.items():
            with self.subTest(source=source):
                flagged, _ = unencoded_text_io(source)
                self.assertEqual(len(flagged), expected)


if __name__ == "__main__":
    unittest.main()
