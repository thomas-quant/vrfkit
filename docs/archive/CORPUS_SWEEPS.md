# Earlier corpus sweeps [ARCHIVED -- HISTORICAL]

The corpus measurements README and USAGE quoted until 2026-09-29, moved here
when those pages were cut to the current state. Each keeps its date and the
parser behaviour of its time; none is a current result. The current audit is
[`../BUILD_VERIFICATION.md`](../BUILD_VERIFICATION.md).

## 13.01, 215 replays, before unresolved payloads were preserved

`validate_corpus.py` before unresolved whole-payload rows were separated from
genuine loss in the oracle score:

```
succeeded: 215/215        failed: 0
pass rate: min 97.487378%  median 99.323434%  max 99.682485%
totals   : 136,545,822 content blocks / 98,884,839 fields / 75,571,092 RPCs
           malformed framing 0        unattributed 1,972,019,383 bits
```

PROJECT_STATUS.md section 4 records the same pass rates and totals at
`8be0b8d` (2026-08-02), but 1,972,018,965 unattributed bits. The figure above
is a re-measurement recorded in commit `0007449` (2026-08-06); no record says
what moved the 418 bits.

In the `8be0b8d` run, 97.283437% of the unattributed bits were
`AbilitiesAndBuffsComponent`, for which the replay declares no cache group;
`MeleeAttackState1`-`4` and `_Alt` already resolved through the shared
`MeleeAttackStateComponent_ClassNetCache`.
An even older implementation printed 100% for the wrong reason: it silently
dropped blocks whose group it could not find and counted nothing. The exact
`185d452`/`a73ee3a` comparison reproduces that accounting change on two 13.01
inputs with unchanged block populations and row counts; its scope is in
[`../FOLLOWUP.md`](../FOLLOWUP.md) (it does not cover 13.05, which both old
revisions reject). PROJECT_STATUS.md sections 4, 5-D and 7-C carry the
investigation.

## 13.01, 13.02 and 13.04, 527 replays, 2026-08-31

```
succeeded: 527/527 (215 13.01, 204 13.02, 108 13.04)        failed: 0
pass rate: min, median and max 100.000000%
totals   : 344,569,357 content blocks / malformed framing 0
```

The 13.04 subset exported with checkpoints: 108/108 readable, with zero
overlay, struct and checkpoint decode failures.

## 13.05, 51 replays, 2026-09-07

`vrfkit validate` once per file over the corpus's 51 13.05 replays, reading
`ORACLE PASS RATE`, against four-file samples of each older build validated the
same day with the same binary:

```
13.05: 51/51 parsed, min 99.929554% / mean 99.950249% / max 99.962170%
same day: 13.01 99.933-99.947%, 13.02 99.953-99.961%, 13.04 99.941-99.959%
```

## 714 replays, September 2026

The corpus: 215 13.01, 204 13.02, 108 13.04 and 187 13.05, exported with
checkpoints.

- **2026-09-07.** Every `validate` run found field-stream loss: weighted block
  preservation 99.949053% main, 99.589353% checkpoint. Ares accounted for
  239,134 of the 240,679 main field-stream failures, the largest shape stopping
  after 9 bits (142,025 blocks); the 53,582 lost checkpoint blocks stopped at
  185 bits (52,201), 217 (105) and 233 (1,276). These are failure locations,
  not proof that `CachedAttributeSet` itself was broken.
- **Tail preservation.** Recovering or preserving post-RepLayout tails took the
  lost blocks to zero in both streams; every ReplayData validation passed. Main
  physical typed coverage rose from 66.18% to 69.92%, checkpoint from 41.24% to
  41.30%, and the time fields added 37,601,710 typed rows across the two
  streams. The decoded/raw split is in [`../FOLLOWUP.md`](../FOLLOWUP.md).
- **Header-order correction.** All 125,037 main and 835,967 checkpoint partial
  fragments reassemble, into 293,720 complete bunches, with zero partial
  errors; the missing-initial conclusion before it was a header-order error.
  Physical typed coverage then read 70.8088% main and 78.2028% checkpoint
  ([`../PARTIAL_HEADER_CORRECTION.md`](../PARTIAL_HEADER_CORRECTION.md)).

## Raw-property inventory, 527 replays, 2026-08-31

`analyze_raw_properties.py --all` over the 527 replays of 2026-08-31: 269,994,556
non-ClassNetCache replicated-property rows. Of 59,291,880 raw-only rows,
57,318,004 kept a wire name and 1,973,876 did not. Every unnamed row kept
exact-length `raw_bits` (missing, typed, wrong-length, checksum-attributed and
sentinel-handle violations all zero); 90.6005% carried a non-zero payload and
11.6311% were not byte-aligned. The anonymous inventory found 1,699 field
signatures and 1,029 update layouts. 13.04 had a larger genuinely new shape
tail: 73.71% of its unnamed rows used a cross-build signature and 77.78% of its
unnamed updates a cross-build layout, against about 100% for 13.01 and 13.02.
