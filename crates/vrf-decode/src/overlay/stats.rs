//! Counters and the per-field breakdown an overlay pass accumulates.
//!
//! These are the export summary's only view of what the overlay did, and the
//! error report is what tells an operator *which* field to look at rather than
//! just how many failed. The reference replay currently records zero decode
//! errors, so everything here except the plain counters is a cold path; it is
//! written for clarity, not for speed.

use std::collections::HashMap;

use crate::decode::FieldType;

/// Categorisation of a decode failure -- distinguishes root cause so the
/// operator knows whether to fix the overlay type, the bit-count expectation,
/// or something structural.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeErrorKind {
    /// BitReader reached EOF before the decoder finished consuming.
    Eof,
    /// Decoder finished but bits remained unconsumed.
    Residual,
    /// Zero-bit payload with a non-zero-expecting type.
    ZeroBits,
}

impl std::fmt::Display for DecodeErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => f.write_str("EOF"),
            Self::Residual => f.write_str("Residual"),
            Self::ZeroBits => f.write_str("ZeroBits"),
        }
    }
}

impl DecodeErrorKind {
    const fn sort_key(self) -> u8 {
        match self {
            Self::Eof => 0,
            Self::Residual => 1,
            Self::ZeroBits => 2,
        }
    }
}

/// Accumulates per-(group, field, type, bit_count, error_kind) counts so the
/// operator can identify the dominant decode-error sources after an export run.
///
/// Designed for long-lived use: call [`Self::record`] on every failure, then
/// [`Self::top_n`] to get the sorted report.
#[derive(Debug, Clone, Default)]
pub struct OverlayErrorReport {
    /// Key: (group_path, field_name, field_type_tag, bit_count, error_kind).
    /// Value: occurrence count.
    counts: HashMap<(String, String, String, u32, DecodeErrorKind), u64>,
}

/// One row from the error report, sorted by descending count.
#[derive(Debug, Clone)]
pub struct OverlayErrorRow {
    pub count: u64,
    pub group_path: String,
    pub field_name: String,
    pub declared_type: String,
    pub bit_count: u32,
    pub error_kind: DecodeErrorKind,
}

impl OverlayErrorReport {
    /// Record a decode failure.
    pub fn record(
        &mut self,
        group_path: &str,
        field_name: &str,
        field_type: FieldType,
        bit_count: u32,
        kind: DecodeErrorKind,
    ) {
        let key = (
            group_path.to_owned(),
            field_name.to_owned(),
            format!("{field_type:?}"),
            bit_count,
            kind,
        );
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// Merge another report into this one (additive counts).
    pub fn merge_from(&mut self, other: &Self) {
        for (key, &count) in &other.counts {
            *self.counts.entry(key.clone()).or_insert(0) += count;
        }
    }

    /// Return the top `n` error sources sorted by descending count.
    pub fn top_n(&self, n: usize) -> Vec<OverlayErrorRow> {
        let mut rows: Vec<OverlayErrorRow> = self
            .counts
            .iter()
            .map(|((gp, fn_, dt, bc, ek), &cnt)| OverlayErrorRow {
                count: cnt,
                group_path: gp.clone(),
                field_name: fn_.clone(),
                declared_type: dt.clone(),
                bit_count: *bc,
                error_kind: *ek,
            })
            .collect();
        // Deterministic ordering: descending count, then by the row's identity
        // fields so equal-count buckets have a stable order run-to-run. A plain
        // `Reverse(count)` left ties in HashMap iteration order, which varied
        // per run and made the printed error report non-reproducible.
        rows.sort_by(|a, b| {
            b.count.cmp(&a.count).then_with(|| {
                a.group_path
                    .cmp(&b.group_path)
                    .then_with(|| a.field_name.cmp(&b.field_name))
                    .then_with(|| a.declared_type.cmp(&b.declared_type))
                    .then_with(|| a.bit_count.cmp(&b.bit_count))
                    .then_with(|| a.error_kind.sort_key().cmp(&b.error_kind.sort_key()))
            })
        });
        rows.truncate(n);
        rows
    }

    /// Total number of distinct error buckets.
    pub fn bucket_count(&self) -> usize {
        self.counts.len()
    }

    /// Total errors across all buckets.
    pub fn total_errors(&self) -> u64 {
        self.counts.values().sum()
    }
}

/// Statistics from an overlay pass.
#[derive(Debug, Clone, Default)]
pub struct OverlayStats {
    /// Fields where the type was known and decoding succeeded.
    pub decoded_ok: u64,
    /// Fields where the type was known but decoding failed.
    pub decoded_err: u64,
    /// Fields where the type is Raw/Skip (intentionally not decoded).
    pub raw_or_skip: u64,
    /// Fields where (group_path, field_name) had no entry in the table.
    pub not_in_table: u64,
    /// Fields where field_name was None (unmapped handle).
    pub no_field_name: u64,
    /// Handle fallbacks refused because the replay declared a DIFFERENT,
    /// unresolved field name at that handle -- see
    /// docs/OVERLAY_RESOLUTION.md "Fail-closed on a handle conflict" for the
    /// fail-closed rationale and the 1065353216 misdecode example.
    ///
    /// It is deliberately NOT routed through `decoded_err`: nothing failed to
    /// decode, the overlay declined to claim a type.
    pub handle_conflicts_refused: u64,
    /// Detailed per-field error breakdown (populated only when reporting is on).
    pub error_report: OverlayErrorReport,
}

impl OverlayStats {
    /// Add the six counters of `other` into `self`.
    ///
    /// `error_report` is deliberately not merged: callers fold it into one
    /// report shared by every pass (see `OverlayErrorReport::merge_from`), so
    /// a checkpoint-only failure is still in the breakdown the summary prints.
    /// The destructure has no `..`, so a counter added to this struct does not
    /// compile until it is summed here -- the export and `diag` totals both go
    /// through this method, and a hand-written copy of the sum is how a new
    /// counter used to reach one of them and not the other.
    pub fn merge_counts_from(&mut self, other: &Self) {
        let Self {
            decoded_ok,
            decoded_err,
            raw_or_skip,
            not_in_table,
            no_field_name,
            handle_conflicts_refused,
            error_report: _,
        } = other;
        self.decoded_ok += decoded_ok;
        self.decoded_err += decoded_err;
        self.raw_or_skip += raw_or_skip;
        self.not_in_table += not_in_table;
        self.no_field_name += no_field_name;
        self.handle_conflicts_refused += handle_conflicts_refused;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eight buckets that all carry the same count, so every pair is a tie and
    /// only the tiebreak decides the order. Named out of alphabetical order so
    /// a report that echoes insertion order cannot pass by accident.
    const TIED: [(&str, &str); 8] = [
        ("GroupD", "delta"),
        ("GroupA", "beta"),
        ("GroupC", "gamma"),
        ("GroupA", "alpha"),
        ("GroupB", "epsilon"),
        ("GroupD", "alpha"),
        ("GroupB", "beta"),
        ("GroupC", "alpha"),
    ];

    fn tied_report() -> OverlayErrorReport {
        let mut report = OverlayErrorReport::default();
        for (group, field) in TIED {
            report.record(group, field, FieldType::Int32, 32, DecodeErrorKind::Eof);
        }
        report
    }

    #[test]
    fn top_n_breaks_count_ties_by_group_then_field() {
        let rows = tied_report().top_n(TIED.len());
        let order: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.group_path.as_str(), r.field_name.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("GroupA", "alpha"),
                ("GroupA", "beta"),
                ("GroupB", "beta"),
                ("GroupB", "epsilon"),
                ("GroupC", "alpha"),
                ("GroupC", "gamma"),
                ("GroupD", "alpha"),
                ("GroupD", "delta"),
            ]
        );
    }

    #[test]
    fn top_n_is_identical_across_independently_built_reports() {
        // Each report owns a HashMap with its own RandomState, so tied buckets
        // iterate in a different order per instance. Sorting on count alone let
        // that leak into the printed report; two runs disagreed.
        let first = tied_report().top_n(TIED.len());
        let second = tied_report().top_n(TIED.len());
        let key = |rows: &[OverlayErrorRow]| -> Vec<(String, String)> {
            rows.iter()
                .map(|r| (r.group_path.clone(), r.field_name.clone()))
                .collect()
        };
        assert_eq!(key(&first), key(&second));
    }

    #[test]
    fn top_n_still_puts_the_biggest_count_first() {
        let mut report = tied_report();
        report.record(
            "GroupD",
            "delta",
            FieldType::Int32,
            32,
            DecodeErrorKind::Eof,
        );
        let rows = report.top_n(1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].group_path, "GroupD");
    }

    #[test]
    fn top_n_breaks_identity_ties_by_error_kind() {
        let mut report = OverlayErrorReport::default();
        for kind in [
            DecodeErrorKind::ZeroBits,
            DecodeErrorKind::Residual,
            DecodeErrorKind::Eof,
        ] {
            report.record("Group", "Field", FieldType::Int32, 32, kind);
        }

        let kinds: Vec<DecodeErrorKind> = report
            .top_n(3)
            .into_iter()
            .map(|row| row.error_kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                DecodeErrorKind::Eof,
                DecodeErrorKind::Residual,
                DecodeErrorKind::ZeroBits,
            ]
        );
    }

    fn distinct_counts(base: u64) -> OverlayStats {
        OverlayStats {
            decoded_ok: base + 1,
            decoded_err: base + 2,
            raw_or_skip: base + 3,
            not_in_table: base + 4,
            no_field_name: base + 5,
            handle_conflicts_refused: base + 6,
            error_report: tied_report(),
        }
    }

    /// Each counter lands in its own total, and the per-field breakdown is
    /// left alone: callers merge it into a report shared across passes.
    #[test]
    fn merge_counts_from_sums_every_counter_and_leaves_the_report() {
        let mut total = OverlayStats::default();
        total.merge_counts_from(&distinct_counts(0));
        total.merge_counts_from(&distinct_counts(100));
        assert_eq!(total.decoded_ok, 1 + 101);
        assert_eq!(total.decoded_err, 2 + 102);
        assert_eq!(total.raw_or_skip, 3 + 103);
        assert_eq!(total.not_in_table, 4 + 104);
        assert_eq!(total.no_field_name, 5 + 105);
        assert_eq!(total.handle_conflicts_refused, 6 + 106);
        assert_eq!(total.error_report.total_errors(), 0);
    }
}
