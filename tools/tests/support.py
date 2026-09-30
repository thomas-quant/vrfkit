"""Shared test scaffolding. Imports nothing from tools/; importing it puts tools/ on sys.path."""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parents[1]
REPO = TOOLS.parent
sys.path.insert(0, str(TOOLS))


class TempDirTestCase(unittest.TestCase):
    def tmp(self) -> Path:
        """A fresh directory removed when the test ends."""
        return Path(self.enterContext(tempfile.TemporaryDirectory()))


def run_cli(main, *argv, prog=None, exits=False, merged=False):
    """Run a tool's main and return (code, stdout, stderr).

    With `prog`, argv is patched into sys.argv and main() takes no arguments; otherwise
    main(list(argv)). exits=True turns SystemExit into its code; merged=True writes both
    streams, in order, to stdout.
    """
    args, out, err = [str(a) for a in argv], io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out if merged else err):
        try:
            if prog is None:
                code = main(args)
            else:
                with mock.patch.object(sys, "argv", [prog, *args]):
                    code = main()
        except SystemExit as exit:
            if not exits:
                raise
            code = exit.code
    return code, out.getvalue(), err.getvalue()
