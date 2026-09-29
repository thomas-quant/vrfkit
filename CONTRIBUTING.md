# Contributing to vrfkit

Thanks for your interest. vrfkit is a reverse-engineered parser for a format
that changes every game build, so it has a few unusual rules. Please read these
before opening a PR.

## Maintenance and response times

I am a student maintaining vrfkit in my spare time. Contributions and bug
reports are welcome, but replies and PR reviews may take some time while I
balance the project with my studies. There is no guaranteed response time.
Thank you for your patience and for helping improve the project.

## Build

```bash
cargo +1.86.0 build --release -p vrfkit --locked                       # inspect / validate / export
cargo +1.86.0 build --release -p vrfkit --no-default-features --locked # inspect / validate only
```

Edition 2024. `#![forbid(unsafe_code)]` is in every crate — do not add `unsafe`.

Python tooling under `tools/` needs `pip install -r requirements.txt`
(pyarrow, numpy) -- without it several checks below fail to import instead of
running.

**MSRV is 1.86, and the main Rust CI job pins exactly that.** A newer local
toolchain accepts syntax 1.86 rejects — `let` chains are the one that has already broken a build —
so a green `cargo test` on your machine is not evidence CI will pass. Install
the pinned toolchain once and run the sweep through it:

```bash
rustup toolchain install 1.86.0 --component clippy,rustfmt
cargo +1.86.0 clippy --workspace --all-targets --all-features --locked -- -D warnings
```

## Before you open a PR

Run the full sweep. Every one of these must be green:

```bash
cargo +1.86.0 fmt --check
cargo +1.86.0 clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +1.86.0 test --workspace --locked
cargo +1.86.0 test -p vrfkit --no-default-features --locked
cargo +1.86.0 test -p vrf-container --no-default-features --locked
cargo +1.86.0 check --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo +1.86.0 doc --workspace --all-features --no-deps --locked
cargo +1.86.0 fmt --manifest-path tools/extract_component_classes/Cargo.toml --check
cargo +1.86.0 clippy --manifest-path tools/extract_component_classes/Cargo.toml --all-targets --locked -- -D warnings
cargo +1.86.0 test --manifest-path tools/extract_component_classes/Cargo.toml --locked
VRFKIT_INTEROP_DIR="<private-root>" cargo +1.86.0 test -p vrf-export --test roundtrip write_interop_files --locked -- --exact
python -W error crates/vrf-export/tests/python_interop.py "<private-root>/interop"
python -W error tools/check_ascii.py --check
python -W error tools/apply_type_corrections.py --check
python -W error tools/check_effect_decoder.py --check
python -W error tools/extract_checksum_types.py --export tools/fixtures/checksum_export --check
python -W error tools/generate_scoped_types.py --check
python -W error tools/extract_equippables.py --check
python -W error tools/extract_descriptors.py third_party/vrp/Replay.Valorant crates/vrf-decode/src/table.rs
python -W error tools/apply_type_corrections.py
cargo +1.86.0 fmt -p vrf-decode
git diff --exit-code -- crates/vrf-decode/src/table.rs   # regenerated table == committed table
python -W error tools/check_baseline_schemas.py
python -W error tools/check_docs.py   # not --fast: runs both suites again to check the counts
python -W error -m unittest discover -s tools/tests -p "test_*.py"
```

For the interop lines, point `VRFKIT_INTEROP_DIR` at a private root; the Rust
test writes its files to that root's `interop` child, which is passed exactly
to Python. The script refuses to search the system temp directory, because
“newest” can be a stale fixture from another checkout.

Run every advertised core-only and singleton feature, not just the default
workspace. These are the commands CI executes; each singleton intentionally
starts from `--no-default-features`:

```bash
cargo +1.86.0 check -p vrfkit --no-default-features --locked
cargo +1.86.0 check -p vrfkit --no-default-features --features export --locked
cargo +1.86.0 check -p vrf-bitio --no-default-features --locked
cargo +1.86.0 check -p vrf-bitio --no-default-features --features alloc --locked
cargo +1.86.0 check -p vrf-container --no-default-features --locked
cargo +1.86.0 check -p vrf-container --no-default-features --features oodle --locked
cargo +1.86.0 check -p vrf-container --no-default-features --features event --locked
cargo +1.86.0 check -p vrf-container --no-default-features --features checkpoint --locked
cargo +1.86.0 check -p vrf-decode --no-default-features --locked
cargo +1.86.0 check -p vrf-decode --no-default-features --features array --locked
cargo +1.86.0 check -p vrf-decode --no-default-features --features effect --locked
cargo +1.86.0 check -p vrf-decode --no-default-features --features overlay --locked
cargo +1.86.0 check -p vrf-decode --no-default-features --features structs --locked
cargo +1.86.0 check -p vrf-export --no-default-features --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features parquet --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features fields --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features movement --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features actors --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features net-guids --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features events --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features partials --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features checkpoint-context --locked
cargo +1.86.0 check -p vrf-export --no-default-features --features snappy --locked
cargo +1.86.0 check -p vrf-net --no-default-features --locked
cargo +1.86.0 check -p vrf-net --no-default-features --features diagnostics --locked
cargo +1.86.0 check -p vrf-schema --no-default-features --locked
cargo +1.86.0 check -p vrf-schema --no-default-features --features checkpoint --locked
```

If your change affects exported output, also run the regression guards in
[`docs/USAGE.md`](docs/USAGE.md) §6 (`check_export_baseline.py`,
`check_decode_errors_corpus.py`, `validate_corpus.py`) against a replay, and
update baselines with `--update` only after explaining each changed line. Those
need a corpus — see [Environment](#environment) below.

### What CI runs

- **`rust`** (Windows, Rust 1.86): fmt, clippy, the all-features check, the
  feature matrix, the standalone component tool, strict rustdoc, the core-only
  `vrfkit` and `vrf-container` tests, the interop test, the table
  regeneration, and `check_docs.py` in full -- both suites with Python
  warnings as errors; a failed process, a missing or zero count, or a skipped
  Python test fails it. It then runs `check_corpus_baseline.py`,
  `verify_build_corpus.py` (validation, checkpoint export, reconciled
  counters, independent raw/typed comparisons, positive checkpoint decoding
  per build) and `validate_type_evidence.py --compare-typed` on the 12.10,
  12.11 and 13.00 fixtures. Those are byte-identical to the upstream parser's
  public test replays and are fetched from a pinned commit, SHA-256 checked.
  Every identity in `tools/fixtures/public_fixture_type_evidence.json` must
  be observed and every independently decoded value must match; the fixtures
  cover only the fields they contain. The report and logs are kept 14 days as
  an artifact, failures included; replays and Parquet are not uploaded.
- **`python-checks`** (Python 3.12 and 3.13, Windows and Ubuntu): the ASCII,
  generator and baseline-schema checks, the tools suite, the effect decoder
  and `check_docs.py --fast`.
- **`rust-stable`** (Windows): all-feature workspace tests and core-only CLI
  tests.
- **`workflow-lint`**: checksum-pinned actionlint. Actions are pinned to
  commit IDs, permissions are read-only, superseded runs are cancelled and
  every job has a time limit.
- **`windows-package`**: see [Tagged Windows releases](#tagged-windows-releases).

`CI complete` succeeds only when every job above succeeds; a failure,
cancellation or skip cannot pass it. The 13.02, 13.04, 13.05 and 13.06
baselines have no public fixture and are still yours to run.

### Replay evidence for parser changes

For changes to parsing, payload transforms, decoding, or exported output,
including support for a new game build, include actual replay validation
results in the PR. Passing CI alone does not establish that real replays
decode correctly. Before merging, the contributor or maintainer should run
the affected validation and export paths on real replays and record:

- The tested game build/branch and replay count for each build.
- The code commit and exact commands used, including whether checkpoint
  decoding was exercised when the change affects it.
- Successful and failed replay counts, relevant transform/framing/decode
  failure counts, and oracle pass rates where available.
- A before/after comparison on the same replays, including a previously
  supported build when shared parser code changes, with any output differences
  or remaining failures explained.

State the scope of the check: a few samples or a full available corpus. If no
replay is available, you can still open a PR; say that replay validation was
not run and what input is needed so a maintainer can complete it before
merging. A skipped corpus check is not replay validation. You do not need to
upload replay files publicly to contribute; recorded commands and results
can document a local run.

## Tagged Windows releases

Pushing a tag such as `v0.1.0` runs the same `ci.yml` as the branch/PR checks
through `workflow_call`. The tag must be SemVer with a leading `v`; optional
prerelease and build suffixes are allowed. The release job runs only after all
checks succeed for that exact tagged commit. Only that publishing job receives
`contents: write`; tests and builds retain read-only repository permissions.

Version numbers follow the CLI, which is what a release ships. A release bumps
the patch version unless it breaks how `vrfkit` is invoked or what it writes;
such a break bumps the minor version. New manifest keys and new summary lines
do not count as breaks. The library crates are not published, so their Rust
API is internal and may change in any release. Every crate carries the
workspace version.

The Windows package job is also required on every PR and manual CI run. It
builds and tests the optimized CLI for `x86_64-pc-windows-msvc`, creates a ZIP
with `tools/package_release.py`, runs the extracted executable, and audits the
pinned public 12.10 replay with the packaged binary, including checkpoints and
independent typed/raw comparisons. The ZIP includes `vrfkit.exe`, `LICENSE`,
`NOTICE.md` and `build-info.json` (tag, source commit, target and binary hash).
It and its SHA-256 sidecar are retained for 14 days as `windows-release`.
The corresponding replay report and diagnostic logs use a separate artifact.

On tag pushes, the publishing job downloads those verified assets, checks the
ZIP checksum and embedded provenance again, and publishes them without a
second build. Prerelease tags produce GitHub prereleases. Release notes are
generated automatically. Runs for the same tag are serialized; an existing
release is not overwritten. Packaging rejects existing output directories.

To validate a candidate before tagging or merging, run the **CI** workflow
manually on its branch and inspect the `windows-release` and
`release-replay-verification` artifacts. Branch, PR and manual CI runs do not
publish releases. No release is made merely by merging the workflow.

## Environment

The corpus guards read their inputs from environment variables rather than
hardcoded paths, so nothing in the tree points at one person's disk. None of
them are needed for the sweep above; all of them are needed for §6.

| Variable | What it points at | Read by |
|---|---|---|
| `VRFKIT_CORPUS_DIR` | Directory of `.vrf` replays; a bare filename in a baseline resolves against it | `check_export_baseline.py`, `check_corpus_baseline.py`, `check_metrics_baseline.py` |
| `VRFKIT_VALPLAY_DIR` | valplay checkout root | `check_metrics_baseline.py`, `validate_metrics_corpus.py` |
| `VRFKIT_JOBS` | Worker count for the corpus sweeps; default is cores - 2, capped at 16 | `validate_corpus.py` |
| `VRFKIT_REQUIRE_CORPUS` | Set to anything to turn "corpus absent, skipping" into a failure | `crates/vrf-container/tests/corpus.rs`, `check_export_baseline.py`, `check_corpus_baseline.py` |

`analyze_coverage.py` and `extract_equippables.py` read the C# descriptors
vendored under [`third_party/vrp/`](third_party/vrp/README.md) (`--csharp-dir`
/ `--csharp-root` for another checkout). `compare_combat_report.py` and
`compare_rpc_params.py` take `--reference` and `--ours`, defaulting to a
machine-local C# export produced as described in
[docs/USAGE.md](docs/USAGE.md#regression-guards----after-non-trivial-changes);
`compare_with_csharp.py` takes both directories as positional arguments. None
of the three reads an environment variable. Nothing checks this table, so
verify a row by grepping for the variable rather than by reading the name:

```bash
grep -rn "VRFKIT_" tools/*.py | grep environ
```

**`tests/corpus.rs` is a container-level smoke test, not a decode sweep.** It
parses each replay's header and decompresses its Oodle chunks; it never reaches
a field. A green `cargo test` with the corpus present therefore says nothing
about decoding. The sweeps that do are `validate_corpus.py` (RepLayout framing
on every content block), `check_decode_errors_corpus.py` (the overlay), and
`verify_build_corpus.py` (the common main/checkpoint audit). These are separate
from `cargo test`. CI runs the common audit and the corpus baselines on the
three public fixtures only ([What CI runs](#what-ci-runs)); the full private
corpus audit remains a local check.

```bash
export VRFKIT_CORPUS_DIR=/path/to/replays
python tools/check_decode_errors_corpus.py ./target/release/vrfkit "$VRFKIT_CORPUS_DIR"
```

**Without `VRFKIT_CORPUS_DIR` the guards skip and exit 0**, printing `SKIP:
replay not present`. That is deliberate — the corpus lives outside the repo, so
a contributor without one is not blocked — but it means an unset variable reads
as a pass at a glance. If you meant to run them, read the output and check it
says how many replays it walked.

## The load-bearing invariants (do not break)

These corrupt downstream consumers silently — no test fails when they break.

- **No skip path.** Every walkable field emits `raw_bits` even when its type is
  unknown or decoding fails. Typed `value_*` columns are an *additive* overlay;
  a decode failure leaves them null with the raw bits intact.
- **No silent success.** A block whose group cannot be resolved fails loudly
  (counted in `validate`'s `RPC payload lost` line), never guessed.
- **Byte-identical output.** Exported Parquet is reproducible run to run. If you
  change row buffering, batch sizes, or iteration order, verify the output is
  byte-identical (the baselines pin this).
- **ASCII only** in Rust code and comments — the Windows cp949 console truncates
  output at the first non-ASCII byte in a format string.
- **No hardcoded names** in the parser. Display names live in the Python adapter
  (`tools/equippable_table.py`), never in a Rust crate.

## Generated files — never hand-edit

| File | Generator |
|---|---|
| `crates/vrf-decode/src/table.rs` | `tools/extract_descriptors.py` on `third_party/vrp/Replay.Valorant`, then `tools/apply_type_corrections.py` |
| `crates/vrf-decode/src/checksum_table.rs` | `tools/extract_checksum_types.py` against one or more fresh exports |
| `crates/vrf-decode/src/scoped_types.rs` | `tools/generate_scoped_types.py` from reviewed exact group/name/checksum evidence |
| `crates/vrf-transform/src/sbox.rs` | `tools/extract_sboxes.py` |
| `crates/vrf-transform/tests/data/golden_vectors.rs` | `tools/extract_golden.py` |
| `crates/vrf-transform/tests/data/native_vectors.rs` | `tools/capture_native_transforms.py` against pinned original executable readers |
| `tools/equippable_table.py` | `tools/extract_equippables.py` from the vendored `third_party/vrp/Replay.Valorant/Combat/ValorantEquippableResolver.cs` |

Run order: `extract_descriptors.py` → `apply_type_corrections.py` →
`cargo fmt` → `extract_checksum_types.py` (against a **fresh** export).
`extract_descriptors.py` rewrites `table.rs` whole, so it runs first. The
corrections work on the one-line and the rustfmt layout alike, but `cargo fmt`
must still run after them to reproduce the committed bytes. The checksum
step's place is load-bearing too (below).

The C# descriptor input is vendored under
[`third_party/vrp/`](third_party/vrp/README.md),
copied verbatim from the commit that README names. A descriptor change is an
edit there, committed together with the regenerated `table.rs`. CI runs the
first three steps against that directory and fails if `table.rs` changes:

```bash
python tools/extract_descriptors.py third_party/vrp/Replay.Valorant \
    crates/vrf-decode/src/table.rs
python tools/apply_type_corrections.py
cargo +1.86.0 fmt -p vrf-decode
git diff --exit-code -- crates/vrf-decode/src/table.rs
```

The checksum step is last because it learns from what the overlay table
declares. Run it before the additions land and the new entries are not donors
yet -- the symptom is a field typed on the group you declared and still raw on
its siblings, which is easy to read as the propagation not working. Re-export
after rebuilding, then regenerate.

The S-box and golden-vector generators need an upstream checkout; nothing
under `third_party/` holds their input:

```bash
python tools/extract_sboxes.py <path>/ValorantSeededTransformHelpers.cs \
    crates/vrf-transform/src/sbox.rs
python tools/extract_golden.py <path>/ValorantSeededTransformTests.cs \
    crates/vrf-transform/tests/data/golden_vectors.rs
```

## Type corrections are conservative

`tools/apply_type_corrections.py` carries two kinds of entry:

- **Corrections** — the C# descriptor declares a type and the wire disagrees.
  Each has cited wire evidence.
- **ADDITIONS** — the C# descriptor is silent. These rest on unusually complete
  wire evidence (e.g. `Money` = 800 at pistol-round start across all actors). Do
  not widen the ADDITIONS list "by eye" — that undoes the reason it is allowed.
  Read the bar stated above `ADDITIONS` in the script first.

## Commit style

Conventional commits (`feat:`, `fix:`, `docs:`, `test:`, `perf:`, `refactor:`,
`chore:`), lowercase, present tense. Keep the subject short; put the "why" in
the body.
