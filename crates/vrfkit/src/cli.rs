//! Argument parsing -- hand-rolled, no external dependencies.
//!
//! Four subcommands:
//!   inspect `<file>`
//!   validate `<file>`
//!   diag `<file>` [--json `<path>`] [--include-payloads]
//!   export `<file>` --out `<dir>`      (feature `export`)

use crate::error::CliError;
use crate::inspect;
use crate::oracle;

const USAGE: &str = "\
vrfkit -- VALORANT replay (.vrf) toolkit

USAGE:
    vrfkit inspect  <file.vrf> [--redact-identifiers]
    vrfkit validate <file.vrf> [--diagnostics]
    vrfkit diag     <file.vrf> [--json <path>] [--include-payloads]
    vrfkit export   <file.vrf> --out <dir> [--checkpoints]

SUBCOMMANDS:
    inspect   Print replay info, header, branch, and chunk summary
              --redact-identifiers  Suppress the replay's friendly name
    validate  Run the RepLayout grammar oracle on every ReplayData content
              block. Exits 0 when all of them framed, 1 when any did not,
              and 2 when the file carried no content blocks to check.
              --diagnostics  Print full context for every malformed/skipped event
    diag      Walk ReplayData and every Checkpoint chunk and aggregate every
              stream failure (kind, cause, group, function count, handle)
              into one bounded JSON document. Writes no table.
              --json  Write the aggregate to a file instead of stdout
              --include-payloads  Include bounded raw payload samples
    export    Write six Parquet tables (fields, movement, actors,
              net_guids, events, partials) + manifest.json
              --checkpoints  Also parse Checkpoint chunks into
                             checkpoint_fields, checkpoint_actors,
                             checkpoint_net_guids, checkpoint_blocks,
                             checkpoint_guid_entries, checkpoint_export_groups and
                             checkpoint_export_fields
                             Parquet tables. Off by default: the
                             snapshots are ~10% of the file and a separate
                             read. fields, movement, actors, net_guids and
                             events are unaffected either way; checkpoint
                             partial rejections, if any, are added to
                             partials.
";

/// Dispatch one command line and report the process exit code it earns.
///
/// `Ok(0)` for every subcommand that has nothing to conclude. `validate` is the
/// exception: it is an oracle, so it returns its own code and `Ok` no longer
/// means "clean". See [`oracle::Verdict`].
pub fn run(args: &[String]) -> Result<u8, CliError> {
    // args[0] = binary name
    if args.len() < 2 {
        return Err(CliError::Usage(USAGE.to_string()));
    }

    match args[1].as_str() {
        "inspect" => {
            let (file, [redact_identifiers], []) = parse(
                args,
                ["--redact-identifiers"],
                [],
                "unknown inspect option or surplus argument: ",
            )?;
            inspect::run(file, redact_identifiers).map(|()| 0)
        }
        "validate" => {
            let (file, [diagnostics], []) = parse(
                args,
                ["--diagnostics"],
                [],
                "unknown validate option or surplus argument: ",
            )?;
            oracle::run(file, diagnostics).map(oracle::Verdict::exit_code)
        }
        "diag" => {
            let (file, [include_payloads], [json]) = parse(
                args,
                ["--include-payloads"],
                [("--json", "--json requires a file path")],
                "unknown diag option or surplus argument: ",
            )?;
            crate::diagnose::run(file, json, include_payloads).map(|()| 0)
        }
        "export" => export(args).map(|()| 0),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(0)
        }
        other => Err(CliError::Usage(format!(
            "unknown subcommand: {other}\n{USAGE}"
        ))),
    }
}

/// The input path, whether each flag was given, and each valued option's value.
type Parsed<'a, const F: usize, const V: usize> = (&'a str, [bool; F], [Option<&'a str>; V]);

/// Split `<subcommand> <file.vrf> [options]`. Each of `flags` may appear
/// once. Each of `valued` may appear once and takes the next argument as its
/// value, whatever it looks like; the pair's second element is the message
/// when there is none. Anything else is refused as `{unknown}{argument}`.
fn parse<'a, const F: usize, const V: usize>(
    args: &'a [String],
    flags: [&str; F],
    valued: [(&str, &str); V],
    unknown: &str,
) -> Result<Parsed<'a, F, V>, CliError> {
    let file = args
        .get(2)
        .ok_or_else(|| CliError::Usage(format!("{} requires <file.vrf>", args[1])))?;
    let (mut set, mut values) = ([false; F], [None; V]);
    let mut rest = args[3..].iter();
    while let Some(arg) = rest.next() {
        let duplicate = || CliError::Usage(format!("duplicate option: {arg}"));
        if let Some(i) = flags.iter().position(|flag| arg == flag) {
            if set[i] {
                return Err(duplicate());
            }
            set[i] = true;
        } else if let Some(i) = valued.iter().position(|(option, _)| arg == option) {
            if values[i].is_some() {
                return Err(duplicate());
            }
            let value = rest
                .next()
                .ok_or_else(|| CliError::Usage(valued[i].1.to_string()))?;
            values[i] = Some(value.as_str());
        } else {
            return Err(CliError::Usage(format!("{unknown}{arg}")));
        }
    }
    Ok((file, set, values))
}

#[cfg(feature = "export")]
fn export(args: &[String]) -> Result<(), CliError> {
    let (file, [with_checkpoints], [out_dir]) = parse(
        args,
        ["--checkpoints"],
        [("--out", "--out requires a directory path")],
        "unknown option: ",
    )?;
    let out_dir =
        out_dir.ok_or_else(|| CliError::Usage("export requires --out <dir>".to_string()))?;
    crate::driver::run(file, out_dir, with_checkpoints)
}

/// Refusal, not silence. A build without the `export` feature has no Parquet
/// writers at all, and a subcommand that printed nothing and exited 0 would be
/// indistinguishable from one that wrote the files.
#[cfg(not(feature = "export"))]
fn export(_args: &[String]) -> Result<(), CliError> {
    Err(CliError::Usage(
        "export is not available: this binary was built without the `export` feature".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn inspect_rejects_surplus_arguments_before_opening_the_file() {
        let err = run(&owned(&["vrfkit", "inspect", "missing.vrf", "extra"]))
            .expect_err("inspect must not ignore a surplus positional argument");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn inspect_accepts_identifier_redaction_before_opening_the_file() {
        let err = run(&owned(&[
            "vrfkit",
            "inspect",
            "missing.vrf",
            "--redact-identifiers",
        ]))
        .expect_err("the missing input should still be opened after parsing");
        assert!(matches!(err, CliError::Io(_)), "got {err:?}");
    }

    #[test]
    fn inspect_rejects_duplicate_identifier_redaction() {
        let err = run(&owned(&[
            "vrfkit",
            "inspect",
            "missing.vrf",
            "--redact-identifiers",
            "--redact-identifiers",
        ]))
        .expect_err("duplicate privacy options must not be ignored");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn validate_rejects_unknown_options_before_opening_the_file() {
        let err = run(&owned(&["vrfkit", "validate", "missing.vrf", "--unknown"]))
            .expect_err("validate must not ignore an unknown option");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn validate_rejects_surplus_positional_arguments() {
        let err = run(&owned(&["vrfkit", "validate", "missing.vrf", "other.vrf"]))
            .expect_err("validate must not ignore another input path");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }

    #[cfg(feature = "export")]
    #[test]
    fn export_rejects_duplicate_out_before_opening_the_file() {
        let err = run(&owned(&[
            "vrfkit",
            "export",
            "missing.vrf",
            "--out",
            "first",
            "--out",
            "second",
        ]))
        .expect_err("export must reject a duplicate --out");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }

    #[cfg(feature = "export")]
    #[test]
    fn export_rejects_duplicate_checkpoints_before_opening_the_file() {
        let err = run(&owned(&[
            "vrfkit",
            "export",
            "missing.vrf",
            "--out",
            "out",
            "--checkpoints",
            "--checkpoints",
        ]))
        .expect_err("export must reject a duplicate --checkpoints");
        assert!(matches!(err, CliError::Usage(_)), "got {err:?}");
    }
}
