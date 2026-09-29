"""Guards for the metrics cross-validation run: one dead replay fails the run
even when another completes, and a previous run's output is never read as
this run's (see `fresh_dir`).
"""
import contextlib
import io
import json
import sys
import tempfile
import unittest
from concurrent.futures import Future
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import validate_metrics_corpus as guard  # noqa: E402

OK = {"id": "a", "stage": "ok", "elapsed_s": 1.0, "sections": {"combat": "EXACT"}}


class FreshDirTests(unittest.TestCase):
    def test_a_stale_file_does_not_survive_into_the_next_run(self):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / "xval" / "some-id"
            target.mkdir(parents=True)
            stale = target / "metrics.json"
            stale.write_text('{"combat": "from the previous run"}',
                             encoding="utf-8")

            guard.fresh_dir(target)

            self.assertTrue(target.is_dir())
            self.assertFalse(stale.exists())

    def test_a_directory_that_does_not_exist_yet_is_created(self):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / "never" / "existed"
            guard.fresh_dir(target)
            self.assertTrue(target.is_dir())


class UnsafeReplayIdTests(unittest.TestCase):
    """An untrusted --only value must never become an rmtree target."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.saved = {name: getattr(guard, name)
                      for name in ("REPO", "EXPORTS", "VRF_DIR", "VRFKIT")}
        guard.REPO = self.root / "repo"
        guard.EXPORTS = self.root / "references"
        guard.VRF_DIR = self.root / "replays"
        guard.VRFKIT = Path(sys.executable)
        for directory in (guard.REPO, guard.EXPORTS, guard.VRF_DIR):
            directory.mkdir(parents=True)

    def tearDown(self):
        for name, value in self.saved.items():
            setattr(guard, name, value)
        self.temp.cleanup()

    def _assert_rejected_without_deleting(self, replay_id: str, victim: Path):
        victim.mkdir(parents=True)
        sentinel = victim / "keep.txt"
        sentinel.write_text("owned by someone else", encoding="utf-8")

        result = guard.process(replay_id)

        self.assertEqual(result["stage"], "input", result)
        self.assertTrue(sentinel.is_file(), f"unsafe id deleted {sentinel}")

    def test_parent_traversal_is_rejected_before_output_cleanup(self):
        victim = guard.REPO / "out" / "outside"
        self._assert_rejected_without_deleting("../outside", victim)

    def test_absolute_replay_id_is_rejected_before_output_cleanup(self):
        victim = self.root / "absolute-victim"
        self._assert_rejected_without_deleting(str(victim.resolve()), victim)

    def test_missing_replay_and_reference_preserve_previous_outputs(self):
        export = guard.REPO / "out" / "xval" / "missing"
        bundle = guard.REPO / "out" / "xval_bundle" / "missing"
        for directory in (export, bundle):
            directory.mkdir(parents=True)
            (directory / "keep.txt").write_text("old complete run", encoding="utf-8")

        result = guard.process("missing")

        self.assertEqual(result["stage"], "input", result)
        self.assertTrue((export / "keep.txt").is_file())
        self.assertTrue((bundle / "keep.txt").is_file())


class FailureTests(unittest.TestCase):
    def test_a_run_where_everything_completed_has_no_failures(self):
        self.assertEqual(guard.failures([OK]), [])

    def test_a_replay_that_died_is_a_failure(self):
        results = [OK, {"id": "b", "stage": "export", "error": "boom"}]
        problems = guard.failures(results)
        self.assertEqual(len(problems), 1)
        self.assertIn("b", problems[0])
        self.assertIn("export", problems[0])

    def test_every_dead_replay_is_named_not_just_the_first(self):
        results = [{"id": "b", "stage": "export", "error": "boom"},
                   {"id": "c", "stage": "metrics", "error": "boom"}]
        self.assertEqual(len(guard.failures(results)), 2)


class _SyncPool:
    """Stands in for `ProcessPoolExecutor` and runs `submit()` in-process: the
    real pool re-imports the module in a fresh interpreter, where this test's
    patched globals (`guard.VRFKIT`, `guard.REPO`, ...) are invisible."""

    def __init__(self, max_workers=None):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def submit(self, fn, *args, **kwargs):
        fut = Future()
        try:
            fut.set_result(fn(*args, **kwargs))
        except BaseException as exc:  # pragma: no cover - defensive
            fut.set_exception(exc)
        return fut


class MainWiringTests(unittest.TestCase):
    """`failures()` is pinned on synthetic results above; these pin that
    `main()` calls it and acts on what it returns."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        root = Path(self._tmp.name)
        self.root = root

        vrf_dir = root / "vrf"
        exports = root / "exports"
        vrf_dir.mkdir()
        exports.mkdir()

        # `sc.run` is patched below, so these are never executed; main()
        # only requires VRFKIT to exist.
        vrfkit, adapter, compute = (root / "vrfkit_stub.py", root / "adapter_stub.py",
                                    root / "compute_stub.py")
        vrfkit.touch()

        # `a` gets a source replay and a reference bundle -- the stub pipeline
        # completes it. `b` gets neither, so `process()` dies at the "input"
        # stage before any subprocess runs at all.
        (vrf_dir / "a.vrf").write_bytes(b"replay")
        (exports / "a").mkdir()
        (exports / "a" / "metrics.json").write_text("{}", encoding="utf-8")

        self.run_calls = []

        def run_stub(cmd, timeout=None):
            """Model successful external stages without executing platform code."""
            cmd = [str(part) for part in cmd]
            self.run_calls.append(cmd)
            if cmd[0] == str(vrfkit):
                Path(cmd[cmd.index("--out") + 1]).mkdir(parents=True, exist_ok=True)
            elif cmd[1] == str(adapter):
                Path(cmd[cmd.index("-o") + 1]).mkdir(parents=True, exist_ok=True)
            elif cmd[1] == str(compute):
                bundle = Path(cmd[2])
                (bundle / "metrics.json").write_text("{}", encoding="utf-8")
            else:  # pragma: no cover - the assertions below name every stage
                self.fail(f"unexpected subprocess command: {cmd}")
            return 0, ""

        self._patches = [
            mock.patch.object(guard, "REPO", root),
            mock.patch.object(guard, "VRF_DIR", vrf_dir),
            mock.patch.object(guard, "EXPORTS", exports),
            mock.patch.object(guard, "VRFKIT", vrfkit),
            mock.patch.object(guard.cmb, "BUNDLE_TOOL", adapter),
            mock.patch.object(guard.cmb, "COMPUTE_METRICS", compute),
            mock.patch.object(guard.cmb.sc, "run", side_effect=run_stub),
            mock.patch.object(guard, "ProcessPoolExecutor", _SyncPool),
        ]
        for p in self._patches:
            p.start()
            self.addCleanup(p.stop)

        self._argv = sys.argv

    def run_main(self, only=("a", "b")):
        sys.argv = ["validate_metrics_corpus.py", "--jobs", "1"]
        for r in only:
            sys.argv += ["--only", r]
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
                code = guard.main()
        finally:
            sys.argv = self._argv
        return code, out.getvalue()

    def test_one_completed_replay_does_not_mask_a_dead_one(self):
        """`a` finishes and `b` never does: the run still fails."""
        code, output = self.run_main(only=("a", "b"))
        self.assertEqual(code, 1, output)
        self.assertIn("FAILED", output)
        self.assertIn("b", output)

    def test_all_replays_completing_exits_zero(self):
        code, output = self.run_main(only=("a",))
        self.assertEqual(code, 0, output)
        export_dir = self.root / "out" / "xval" / "a"
        bundle_dir = self.root / "out" / "xval_bundle" / "a"
        self.assertEqual(
            self.run_calls,
            [
                [str(guard.VRFKIT), "export", str(guard.VRF_DIR / "a.vrf"),
                 "--out", str(export_dir)],
                [sys.executable, str(guard.cmb.BUNDLE_TOOL), str(export_dir), "-o",
                 str(bundle_dir)],
                [sys.executable, str(guard.cmb.COMPUTE_METRICS), str(bundle_dir)],
            ],
        )


if __name__ == "__main__":
    unittest.main()
