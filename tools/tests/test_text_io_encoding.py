"""Every text-mode file read or write in tools/*.py names its encoding.

Without `encoding=`, Python decodes with the locale's code page. On a cp949
Windows host that made extract_healing_observations.py exit 1 on a
manifest.json whose source_file held Hangul, while every sibling tool read the
same file. Running the tools cannot catch this in CI: every job sets
PYTHONUTF8=1, which hides the locale. So this reads the source instead.

Checked: Path.read_text/write_text, builtin/io open, os.fdopen and Path.open,
unless the mode is binary. `<module>.open` (tarfile.open, ...) has another
signature and is skipped. Text-mode subprocess calls are checked apart: they
decode a child's output with the locale's code page too (a cp949
UnicodeDecodeError was reproduced), so each must name `encoding=` and an
explicit `errors=` policy.
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


#: The subprocess functions that decode a child's output in text mode.
SUBPROCESS_CALLS = {"run", "check_output", "Popen", "call", "check_call"}


def undecided_subprocess_text(source: str, filename: str = "<source>") -> tuple[list[int], int]:
    """Lines of text-mode subprocess calls missing `encoding=` or `errors=`,
    and the text-mode call count. Text mode is `text=` or `universal_newlines=`
    set to anything but a literal false, or `encoding=` / `errors=` alone."""
    tree = ast.parse(source, filename)
    modules = {alias.asname or alias.name
               for node in ast.walk(tree) if isinstance(node, ast.Import)
               for alias in node.names if alias.name == "subprocess"}
    functions = {alias.asname or alias.name
                 for node in ast.walk(tree)
                 if isinstance(node, ast.ImportFrom) and node.module == "subprocess"
                 for alias in node.names if alias.name in SUBPROCESS_CALLS}
    flagged, checked = [], 0
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        is_module_call = (isinstance(func, ast.Attribute) and func.attr in SUBPROCESS_CALLS
                          and isinstance(func.value, ast.Name) and func.value.id in modules)
        if not (is_module_call or (isinstance(func, ast.Name) and func.id in functions)):
            continue
        keywords = {keyword.arg: keyword.value for keyword in node.keywords if keyword.arg}
        text = any(
            name in keywords
            and not (isinstance(keywords[name], ast.Constant) and not keywords[name].value)
            for name in ("text", "universal_newlines"))
        if not (text or "encoding" in keywords or "errors" in keywords):
            continue
        checked += 1
        # `**kwargs` could carry them, but nothing here can see it.
        if "encoding" not in keywords or "errors" not in keywords:
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

    def test_every_text_mode_subprocess_call_in_tools_names_encoding_and_errors(self):
        scripts = sorted(TOOLS.glob("*.py"))
        self.assertIn(TOOLS / "verify_build_corpus.py", scripts)
        flagged, checked = [], 0
        for script in scripts:
            lines, count = undecided_subprocess_text(
                script.read_text(encoding="utf-8"), str(script))
            flagged += [f"{script.name}:{line}" for line in lines]
            checked += count
        # Non-vacuous: tools/*.py held 18 text-mode calls when this was added.
        self.assertGreater(checked, 10)
        self.assertEqual(flagged, [],
                         "text-mode subprocess output without encoding= and errors= "
                         "decodes with the locale")

    def test_every_text_mode_subprocess_call_in_the_tests_names_encoding_and_errors(self):
        # The tests run the tools as children and read what they print, so a
        # locale decode fails them on a cp949 host just as it fails the tools.
        scripts = sorted((TOOLS / "tests").glob("*.py"))
        self.assertIn(TOOLS / "tests" / "test_check_ascii.py", scripts)
        flagged, checked = [], 0
        for script in scripts:
            lines, count = undecided_subprocess_text(
                script.read_text(encoding="utf-8"), str(script))
            flagged += [f"{script.name}:{line}" for line in lines]
            checked += count
        # Non-vacuous: tools/tests held 25 text-mode calls when this was added.
        self.assertGreater(checked, 20)
        self.assertEqual(flagged, [],
                         "text-mode subprocess output without encoding= and errors= "
                         "decodes with the locale")

    def test_the_subprocess_scanner_flags_each_undecided_shape(self):
        cases = {
            "import subprocess\nsubprocess.run(c, text=True)": 1,
            "import subprocess\nsubprocess.run(c, universal_newlines=True)": 1,
            "import subprocess\nsubprocess.check_output(c, text=flag)": 1,
            "import subprocess\nsubprocess.run(c, text=True, encoding='utf-8')": 1,
            "import subprocess\nsubprocess.run(c, errors='replace')": 1,
            "import subprocess as sp\nsp.Popen(c, text=True)": 1,
            "from subprocess import check_output\ncheck_output(c, text=True)": 1,
            "import subprocess\nsubprocess.run(c, text=True, encoding='utf-8', errors='strict')": 0,
            "import subprocess\nsubprocess.run(c, capture_output=True)": 0,
            "import subprocess\nsubprocess.run(c, text=False)": 0,
            "runner.run(suite, text=True)": 0,
        }
        for source, expected in cases.items():
            with self.subTest(source=source):
                flagged, _ = undecided_subprocess_text(source)
                self.assertEqual(len(flagged), expected)

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
