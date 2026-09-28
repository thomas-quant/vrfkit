//! Same-volume staging and publication of one complete export directory.
//!
//! # What an interrupted export leaves behind
//!
//! A run writes into `.{out}.vrfkit-staging-{pid}-{nonce}` beside the
//! destination and publishes by renaming that directory over `--out`. `Drop`
//! removes the staging directory of a run that fails in-process, but a killed
//! process runs no destructor. Measured at 259ed10 with `Stop-Process -Force`
//! 1.5 s into an export: `.pub2.vrfkit-staging-55396-0` kept a
//! 1,561,999-byte `fields.parquet` with no footer and no manifest, and
//! nothing ever removed it. Power loss is the same case, and so -- reasoned,
//! not measured -- is a Windows console Ctrl+C: nothing here installs a
//! handler, and the default one ends the process through `ExitProcess`. A
//! kill between `publish`'s two renames strands the prior output in a
//! `.{out}.vrfkit-previous-{pid}-{nonce}` sibling instead -- a complete
//! export, manifest included.
//!
//! The next export to the same destination names every such sibling on
//! stderr and leaves it alone ([`report_leftovers`]). The corpus tools never
//! read one as an export: `tools/export_scan.py` recognises the same names,
//! and `tools/tests/test_export_scan.py` reads [`GENERATED_INFIX`],
//! [`STAGING`] and [`PREVIOUS`] out of this file, so renaming one here
//! without the other fails a test instead of quietly letting a leftover back
//! into a corpus aggregate.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{CHECKPOINT_TABLES, MAIN_TABLES, MANIFEST};

static NEXT_OUTPUT_PATH: AtomicU64 = AtomicU64::new(0);

/// What sits between the destination name and the kind in every sibling this
/// module names: `.{destination}.vrfkit-{kind}-{pid}-{nonce}`.
const GENERATED_INFIX: &str = ".vrfkit-";
/// The kind of the directory a run writes into before it publishes.
const STAGING: &str = "staging";
/// The kind of the prior destination, moved aside for one publication.
const PREVIOUS: &str = "previous";

/// Owns a unique sibling staging directory until it is either published or
/// abandoned.
///
/// The destination is never opened for writing. A failed decode therefore
/// drops this guard and removes only the generated staging directory, leaving
/// the last complete destination untouched. Publication uses same-parent
/// renames so the operation never crosses volumes.
pub(super) struct OutputTransaction {
    destination: PathBuf,
    staging: PathBuf,
    published: bool,
}

impl OutputTransaction {
    /// Create an empty, uniquely named staging directory beside `destination`,
    /// first warning on stderr about every sibling an earlier export to the
    /// same destination left behind (see [`report_leftovers`]). A destination
    /// holding anything an export does not write is refused before that (see
    /// [`foreign_entries`]).
    pub(super) fn begin(destination: &Path) -> io::Result<Self> {
        Self::begin_reporting_to(destination, &mut io::stderr())
    }

    /// [`begin`](Self::begin), with the leftover warnings written to
    /// `warnings`, so a test reads exactly the lines an operator would.
    fn begin_reporting_to(destination: &Path, warnings: &mut dyn Write) -> io::Result<Self> {
        let file_name = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination must be a named directory, not a filesystem root",
            )
        })?;
        if destination.exists() && !destination.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "export destination exists but is not a directory: {}",
                    destination.display()
                ),
            ));
        }
        // `publish` replaces the whole directory, so whatever else it holds
        // would be deleted with it. Refused here, before anything is decoded
        // or created.
        let foreign = foreign_entries(destination).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "could not list export destination {} to check that it holds only \
                     export output: {error}",
                    destination.display()
                ),
            )
        })?;
        if !foreign.is_empty() {
            let them = if foreign.len() == 1 { "it" } else { "them" };
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "export destination {} holds {} an export does not write ({}); publishing \
                     replaces the whole directory and would delete {them} -- choose a new or \
                     empty directory, or remove {them} from this one",
                    destination.display(),
                    entry_count(foreign.len()),
                    name_list(&foreign)
                ),
            ));
        }

        let parent = usable_parent(destination);
        fs::create_dir_all(parent)?;
        // Before this run's own staging directory exists, so a run never
        // reports itself.
        report_leftovers(destination, parent, file_name, warnings);
        let staging = create_unique_directory(parent, file_name, STAGING)?;
        Ok(Self {
            destination: destination.to_path_buf(),
            staging,
            published: false,
        })
    }

    /// Directory into which every output file must be written and finalised.
    pub(super) fn path(&self) -> &Path {
        &self.staging
    }

    /// Publish the completed staging directory as one directory rename.
    ///
    /// When a prior destination exists it is first moved to a unique sibling.
    /// If the staging rename then fails, that prior complete directory is moved
    /// straight back before the error is returned. Only after the new directory
    /// is in place is the backup removed.
    ///
    /// Every error names the step that failed, its paths, and what became of
    /// the run's output, and keeps the OS error's kind. The bare OS error did
    /// none of that: at 259ed10 a re-export while another process had the
    /// destination as its working directory printed only `I/O error: ...
    /// (os error 32)` -- no path, and no sign that a fully decoded export had
    /// just been thrown away.
    pub(super) fn publish(self) -> io::Result<()> {
        self.publish_reporting_to(&mut io::stderr())
    }

    /// [`publish`](Self::publish), with its warnings written to `warnings`,
    /// so a test reads exactly the lines an operator would.
    fn publish_reporting_to(mut self, warnings: &mut dyn Write) -> io::Result<()> {
        let prior = if self.destination.exists() {
            let backup = unique_sibling(&self.destination, PREVIOUS)?;
            if let Err(error) = fs::rename(&self.destination, &backup) {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "publish: could not move the prior output {} aside to {}: {error}; \
                         the new export was discarded and {} is unchanged",
                        self.destination.display(),
                        backup.display(),
                        self.destination.display()
                    ),
                ));
            }
            Some(backup)
        } else {
            None
        };

        if let Err(publish_error) = fs::rename(&self.staging, &self.destination) {
            let outcome = match prior {
                None => String::from("the new export was discarded"),
                Some(backup) => match fs::rename(&backup, &self.destination) {
                    Ok(()) => format!(
                        "the new export was discarded and the prior output was restored to {}",
                        self.destination.display()
                    ),
                    Err(restore_error) => format!(
                        "the new export was discarded, and the prior output could not be moved \
                         back ({restore_error}) and remains at {}",
                        backup.display()
                    ),
                },
            };
            return Err(io::Error::new(
                publish_error.kind(),
                format!(
                    "publish: could not move the staged export {} to {}: {publish_error}; {outcome}",
                    self.staging.display(),
                    self.destination.display()
                ),
            ));
        }

        self.published = true;
        if let Some(backup) = prior {
            // Publication is already committed. A cleanup failure must not be
            // reported as a failed export (which would falsely imply the old
            // destination was still active), but it must not be silent either.
            if let Some(warning) = discard_prior_output(&backup) {
                let _ = writeln!(warnings, "warning: export published, but {warning}");
            }
        }
        Ok(())
    }
}

/// Delete the prior output `publish` moved aside, or say why it was kept.
///
/// `begin` refused a destination holding anything an export does not write,
/// but the whole decode runs between the two, and a file saved into `--out`
/// meanwhile would be deleted here without anyone having been asked. So the
/// backup is checked again -- as exactly what is about to be deleted -- and
/// anything foreign in it, or a listing that fails, keeps all of it.
fn discard_prior_output(backup: &Path) -> Option<String> {
    let foreign = match foreign_entries(backup) {
        Ok(foreign) => foreign,
        Err(error) => {
            return Some(format!(
                "prior-output backup {} could not be listed to check that it holds only export \
                 output ({error}); it was kept -- delete it once you have checked it",
                backup.display()
            ));
        }
    };
    if !foreign.is_empty() {
        return Some(format!(
            "the prior output it replaced gained {} an export does not write ({}) while this \
             export ran; it was kept, not deleted, at {} -- move what you need out of it, then \
             delete it",
            entry_count(foreign.len()),
            name_list(&foreign),
            backup.display()
        ));
    }
    remove_generated(backup).err().map(|error| {
        format!(
            "prior-output backup {} could not be removed: {error}",
            backup.display()
        )
    })
}

impl Drop for OutputTransaction {
    /// Remove the staging directory of a run that never published.
    ///
    /// A removal that fails is reported: `publish`'s errors say the new
    /// export was discarded, and a staging directory that silently survived
    /// would contradict them. Written with `writeln!`, not `eprintln!`, which
    /// panics when the write to stderr fails -- inside an unwind, an abort.
    /// A killed process never gets here; the next run's `report_leftovers`
    /// covers that case.
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if let Err(error) = remove_generated(&self.staging) {
            let _ = writeln!(
                io::stderr(),
                "warning: unpublished staging directory {} could not be removed: {error}; \
                 it is not an export -- delete it once no export to {} is running",
                self.staging.display(),
                self.destination.display()
            );
        }
    }
}

/// Name, on `warnings`, every sibling an earlier export to `destination` left
/// behind. Report only: nothing is deleted and nothing is restored.
///
/// A staging sibling may belong to an export that is still running -- in
/// another process, or in another thread of this one, so not even this
/// process's own PID in the name proves it dead -- and deleting it would make
/// that export fail at its final rename with all its work done. A `previous`
/// sibling beside a missing destination may be the only copy of that output,
/// and restoring it on a guess would bring back a directory the user may
/// have removed on purpose, or pick the wrong one of several. Naming them
/// costs a line each; guessing wrong costs data.
///
/// A parent that cannot be listed is a warning, not an error: an export must
/// not fail because this check could not run, and must not pretend it ran.
fn report_leftovers(
    destination: &Path,
    parent: &Path,
    destination_name: &OsStr,
    warnings: &mut dyn Write,
) {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            let _ = writeln!(
                warnings,
                "warning: could not list {} to look for leftovers of an interrupted export: {error}",
                parent.display()
            );
            return;
        }
    };
    let mut leftovers = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let _ = writeln!(
                    warnings,
                    "warning: could not read an entry of {} while looking for leftovers of an \
                     interrupted export: {error}",
                    parent.display()
                );
                continue;
            }
        };
        let Some(kind) = generated_kind(&entry.file_name(), destination_name) else {
            continue;
        };
        // Only directories are ever generated; a file with the name is not ours.
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => leftovers.push((entry.path(), kind)),
            Ok(_) => {}
            Err(error) => {
                let _ = writeln!(
                    warnings,
                    "warning: could not tell whether {} is a leftover of an interrupted export: \
                     {error}",
                    entry.path().display()
                );
            }
        }
    }
    leftovers.sort();
    let destination_missing = !destination.exists();
    for (path, kind) in leftovers {
        let _ = writeln!(
            warnings,
            "{}",
            leftover_warning(&path, kind, destination, destination_missing)
        );
    }
}

/// One line per leftover, ASCII only (the cp949 console rule), and shaped so
/// no `check_export_baseline.py` counter pattern (`Label:  123`) can match it.
fn leftover_warning(
    path: &Path,
    kind: &str,
    destination: &Path,
    destination_missing: bool,
) -> String {
    if kind == STAGING {
        format!(
            "warning: {} is the staging directory of an export to {} that was interrupted or \
             is still running; it is not an export and is left in place -- delete it once no \
             export to that destination is running",
            path.display(),
            destination.display()
        )
    } else if destination_missing {
        format!(
            "warning: {} holds output that an earlier export to {} moved aside and did not \
             remove, and {} does not exist, so this may be the only copy of it; it is left in \
             place -- move it elsewhere to keep it",
            path.display(),
            destination.display(),
            destination.display()
        )
    } else {
        // A backup `discard_prior_output` kept can hold what someone saved
        // into the destination while an export ran. Name it, and do not
        // advise deleting it the way a backup of export output alone is.
        let (also, advice) = match foreign_entries(path) {
            Ok(foreign) if foreign.is_empty() => (
                String::new(),
                "delete it once no export to that destination is running",
            ),
            Ok(foreign) => (
                format!(
                    ", and {} an export does not write ({})",
                    entry_count(foreign.len()),
                    name_list(&foreign)
                ),
                "move what you need out of it, then delete it once no export to that \
                 destination is running",
            ),
            Err(error) => (
                format!(", and could not be listed to check for anything else ({error})"),
                "look inside it before deleting it",
            ),
        };
        format!(
            "warning: {} holds output that an earlier export to {} moved aside and did not \
             remove{also}; it is not the current export and is left in place -- {advice}",
            path.display(),
            destination.display()
        )
    }
}

/// Every name an export writes into its directory.
fn output_names() -> impl Iterator<Item = &'static str> {
    MAIN_TABLES
        .into_iter()
        .chain(CHECKPOINT_TABLES)
        .chain([MANIFEST])
}

/// The entries of `directory` that no export writes, sorted by name; none
/// for a directory that does not exist.
///
/// An export writes only files, each named in [`MAIN_TABLES`],
/// [`CHECKPOINT_TABLES`] or [`MANIFEST`]. Anything else is foreign: another
/// file, a subdirectory (its whole tree would go with it), a directory or
/// symlink that merely bears a table's name, and also what Explorer leaves
/// behind (`desktop.ini`, `Thumbs.db`). The comparison is exact, case
/// included, because an export writes exactly these names. The leftovers
/// this module names are never among them: they are siblings of the
/// destination, not entries of it.
fn foreign_entries(directory: &Path) -> io::Result<Vec<OsString>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut foreign = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let is_output = entry.file_type()?.is_file() && output_names().any(|output| name == output);
        if !is_output {
            foreign.push(name);
        }
    }
    foreign.sort();
    Ok(foreign)
}

/// `1 entry`, `2 entries`.
fn entry_count(count: usize) -> String {
    format!("{count} entr{}", if count == 1 { "y" } else { "ies" })
}

/// The first few `names`, and how many more there are: a folder of
/// downloads can hold thousands, and the count already says how many.
fn name_list(names: &[OsString]) -> String {
    const SHOWN: usize = 8;
    let mut list = names
        .iter()
        .take(SHOWN)
        .map(|name| name.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        list.push_str(&format!(", and {} more", names.len() - SHOWN));
    }
    list
}

fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn generated_name(destination_name: &OsStr, kind: &str, nonce: u64) -> OsString {
    let mut name = OsString::from(".");
    name.push(destination_name);
    name.push(format!(
        "{GENERATED_INFIX}{kind}-{}-{nonce}",
        std::process::id()
    ));
    name
}

/// The kind of sibling `name` is, when it is exactly what [`generated_name`]
/// writes for `destination_name`: `.{destination_name}.vrfkit-{kind}-` and
/// then `{pid}-{nonce}`, two runs of ASCII digits.
///
/// Compared as encoded bytes, not `str`, so a destination whose name is not
/// valid Unicode is still checked instead of silently never matching. The
/// comparison is exact, case included: on a case-insensitive volume an
/// export to `Out` does not see the leftovers of one to `out`. The tools'
/// filter (`tools/export_scan.py`) accepts any destination name, so they
/// skip such a directory either way.
fn generated_kind(name: &OsStr, destination_name: &OsStr) -> Option<&'static str> {
    let rest = name
        .as_encoded_bytes()
        .strip_prefix(b".")?
        .strip_prefix(destination_name.as_encoded_bytes())?
        .strip_prefix(GENERATED_INFIX.as_bytes())?;
    [STAGING, PREVIOUS].into_iter().find(|kind| {
        rest.strip_prefix(kind.as_bytes())
            .and_then(|tail| tail.strip_prefix(b"-"))
            .is_some_and(is_pid_and_nonce)
    })
}

/// `{pid}-{nonce}`: two non-empty runs of ASCII digits joined by one `-`.
fn is_pid_and_nonce(tail: &[u8]) -> bool {
    let mut parts = tail.split(|&byte| byte == b'-');
    let (Some(pid), Some(nonce), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [pid, nonce]
        .iter()
        .all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
}

/// Draw the next nonce and build the candidate path from it -- the one step
/// `create_unique_directory` and `unique_sibling` both repeat inside their
/// loop. What differs between the two is deliberately not folded in here:
/// one claims the path atomically with `create_dir`, the other only checks
/// `exists()` because it is naming a path it does not create yet.
fn next_candidate(parent: &Path, destination_name: &OsStr, kind: &str) -> PathBuf {
    let nonce = NEXT_OUTPUT_PATH.fetch_add(1, Ordering::Relaxed);
    parent.join(generated_name(destination_name, kind, nonce))
}

fn create_unique_directory(
    parent: &Path,
    destination_name: &OsStr,
    kind: &str,
) -> io::Result<PathBuf> {
    loop {
        let candidate = next_candidate(parent, destination_name, kind);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn unique_sibling(destination: &Path, kind: &str) -> io::Result<PathBuf> {
    let name = destination.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
    })?;
    let parent = usable_parent(destination);
    loop {
        let candidate = next_candidate(parent, name, kind);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
}

/// Remove only paths generated by this module. `remove_dir_all` does not
/// follow directory symlinks, and the exact generated path is never derived
/// from an untrusted glob or environment variable. A prior-output backup has
/// a generated name but the user's contents, so it only comes here once
/// [`discard_prior_output`] has found nothing in it an export does not write.
fn remove_generated(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHECKPOINT_TABLES, MANIFEST, OutputTransaction, PREVIOUS, STAGING, entry_count,
        generated_kind, generated_name, name_list, output_names,
    };
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "vrfkit-publish-test-{}-{}",
                std::process::id(),
                NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create isolated test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn staging_entries(parent: &Path) -> Vec<PathBuf> {
        fs::read_dir(parent)
            .expect("read test directory")
            .map(|entry| entry.expect("read directory entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".vrfkit-staging-"))
            })
            .collect()
    }

    /// `begin`, with the leftover warnings captured instead of printed.
    fn begin_capturing(destination: &Path) -> (OutputTransaction, String) {
        let mut warnings = Vec::new();
        let transaction = OutputTransaction::begin_reporting_to(destination, &mut warnings)
            .expect("begin an export transaction");
        let warnings = String::from_utf8(warnings).expect("warnings are UTF-8");
        (transaction, warnings)
    }

    #[test]
    fn an_aborted_export_preserves_the_prior_output_and_cleans_staging() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("manifest.json"), b"old complete").unwrap();

        {
            let transaction = OutputTransaction::begin(&destination).unwrap();
            fs::write(transaction.path().join("manifest.json"), b"new partial").unwrap();
            // Fault injection: returning before publication drops the guard.
        }

        assert_eq!(
            fs::read(destination.join("manifest.json")).unwrap(),
            b"old complete"
        );
        assert!(staging_entries(root.path()).is_empty());
    }

    #[test]
    fn a_publication_failure_restores_the_prior_complete_directory() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("manifest.json"), b"old complete").unwrap();

        let transaction = OutputTransaction::begin(&destination).unwrap();
        fs::write(transaction.path().join("manifest.json"), b"new complete").unwrap();
        // Fault injection after staging: make the second rename fail, after
        // publication has already moved the old destination aside.
        fs::remove_dir_all(transaction.path()).unwrap();
        let error = transaction
            .publish()
            .expect_err("missing staging must fail");

        assert_eq!(
            fs::read(destination.join("manifest.json")).unwrap(),
            b"old complete"
        );
        assert!(staging_entries(root.path()).is_empty());
        let message = error.to_string();
        assert!(
            message.contains(&format!(
                "the prior output was restored to {}",
                destination.display()
            )),
            "the error must say the prior output is back in place: {message}"
        );
    }

    /// The error a failed publication returns must say which step failed,
    /// on which paths, and what became of the run's work.
    ///
    /// At 259ed10 both renames returned the bare OS error. Measured: a
    /// re-export while another process had the destination as its working
    /// directory printed only `I/O error: ... (os error 32)` -- no path, and
    /// nothing saying the fully decoded staged export had been thrown away.
    #[test]
    fn a_failed_publication_names_the_step_the_paths_and_the_outcome() {
        let root = TestDir::new();
        let destination = root.path().join("export");

        let (transaction, _) = begin_capturing(&destination);
        let staging = transaction.path().to_path_buf();
        // Fault injection: with no prior output, only the staging rename can
        // fail, and nothing is restored.
        fs::remove_dir_all(&staging).unwrap();
        let error = transaction
            .publish()
            .expect_err("missing staging must fail");

        let message = error.to_string();
        for expected in [
            "could not move the staged export".to_owned(),
            staging.display().to_string(),
            destination.display().to_string(),
            "the new export was discarded".to_owned(),
        ] {
            assert!(
                message.contains(&expected),
                "missing {expected:?}: {message}"
            );
        }
        assert_eq!(
            error.kind(),
            io::ErrorKind::NotFound,
            "the OS error kind must survive the added context"
        );
    }

    /// The first rename -- moving the prior output aside -- fails the way the
    /// measured os error 32 did: something else holds the destination open
    /// without `FILE_SHARE_DELETE`. Windows-only because only Windows refuses
    /// to rename a directory another handle has open; every Rust CI job runs
    /// on Windows, so this runs there.
    #[cfg(windows)]
    #[test]
    fn a_destination_held_open_elsewhere_fails_naming_it_and_keeps_the_prior_output() {
        use std::os::windows::fs::OpenOptionsExt;
        // winbase.h; required for CreateFileW to open a directory at all.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("manifest.json"), b"old complete").unwrap();

        let (transaction, _) = begin_capturing(&destination);
        fs::write(transaction.path().join("manifest.json"), b"new complete").unwrap();
        let holder = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&destination)
            .expect("hold the destination directory open without sharing");
        let error = transaction
            .publish()
            .expect_err("a destination held without FILE_SHARE_DELETE cannot be moved aside");
        drop(holder);

        let message = error.to_string();
        for expected in [
            "could not move the prior output".to_owned(),
            destination.display().to_string(),
            "the new export was discarded".to_owned(),
        ] {
            assert!(
                message.contains(&expected),
                "missing {expected:?}: {message}"
            );
        }
        assert_eq!(
            fs::read(destination.join("manifest.json")).unwrap(),
            b"old complete"
        );
        assert!(
            staging_entries(root.path()).is_empty(),
            "the discarded staging directory must still be removed"
        );
    }

    /// What `Stop-Process -Force` 1.5 s into an export left behind at
    /// 259ed10 -- measured, name and all: a footerless `fields.parquet`, no
    /// manifest, in a staging directory nothing ever removed. The next export
    /// to the same destination must name it, and must not delete it: the PID
    /// in the name cannot tell a dead export from one still running.
    #[test]
    fn a_staging_directory_left_by_a_killed_export_is_reported_and_kept() {
        let root = TestDir::new();
        let destination = root.path().join("pub2");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("manifest.json"), b"old complete").unwrap();
        let leftover = root.path().join(".pub2.vrfkit-staging-55396-0");
        fs::create_dir(&leftover).unwrap();
        fs::write(leftover.join("fields.parquet"), b"PAR1 no footer").unwrap();

        let (transaction, warnings) = begin_capturing(&destination);
        assert_eq!(
            warnings.lines().count(),
            1,
            "one line per leftover: {warnings}"
        );
        assert!(
            warnings.contains(&leftover.display().to_string()),
            "the warning must name the leftover: {warnings}"
        );
        assert!(warnings.contains("not an export"), "{warnings}");

        fs::write(transaction.path().join("manifest.json"), b"new complete").unwrap();
        transaction.publish().unwrap();
        assert_eq!(
            fs::read(destination.join("manifest.json")).unwrap(),
            b"new complete"
        );
        assert_eq!(
            fs::read(leftover.join("fields.parquet")).unwrap(),
            b"PAR1 no footer",
            "report only: the leftover must be left exactly as it was"
        );
    }

    /// A kill between `publish`'s two renames leaves no destination and the
    /// complete prior output in a `previous` sibling. That may be the only
    /// copy, so it is reported -- saying the destination is missing -- and
    /// neither restored (the user may have removed the destination on
    /// purpose) nor deleted.
    #[test]
    fn a_prior_output_stranded_between_the_renames_is_reported_not_restored() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        let stranded = root.path().join(".export.vrfkit-previous-4242-7");
        fs::create_dir(&stranded).unwrap();
        fs::write(stranded.join("manifest.json"), b"old complete").unwrap();

        let (transaction, warnings) = begin_capturing(&destination);
        assert_eq!(
            warnings.lines().count(),
            1,
            "one line per leftover: {warnings}"
        );
        assert!(
            warnings.contains(&stranded.display().to_string()),
            "the warning must name the leftover: {warnings}"
        );
        assert!(
            warnings.contains("does not exist"),
            "a missing destination must be called out: {warnings}"
        );
        assert!(!destination.exists(), "begin must not resurrect a backup");

        fs::write(transaction.path().join("manifest.json"), b"new complete").unwrap();
        transaction.publish().unwrap();
        assert_eq!(
            fs::read(stranded.join("manifest.json")).unwrap(),
            b"old complete"
        );
    }

    /// Report-only is what keeps two exports to one destination safe: the
    /// second names the first's live staging directory and leaves it alone,
    /// and both still publish (last writer wins, as before).
    #[test]
    fn a_concurrent_exports_staging_is_reported_and_left_to_publish() {
        let root = TestDir::new();
        let destination = root.path().join("export");

        let (first, quiet) = begin_capturing(&destination);
        assert_eq!(quiet, "", "a clean parent has nothing to report");
        let (second, warnings) = begin_capturing(&destination);
        assert!(
            warnings.contains(&first.path().display().to_string()),
            "the second export must see the first's staging: {warnings}"
        );

        fs::write(first.path().join("manifest.json"), b"first").unwrap();
        fs::write(second.path().join("manifest.json"), b"second").unwrap();
        first.publish().unwrap();
        second.publish().unwrap();
        assert_eq!(
            fs::read(destination.join("manifest.json")).unwrap(),
            b"second"
        );
        assert!(staging_entries(root.path()).is_empty());
    }

    #[test]
    fn only_this_destinations_generated_shapes_are_reported() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        for near_miss in [
            ".exports.vrfkit-staging-1-0",    // a destination this name prefixes
            ".other.vrfkit-previous-1-0",     // another destination
            ".import.vrfkit-staging-1-0",     // another one of the same length
            "export.vrfkit-staging-1-0",      // no leading dot
            ".export.vrfkit-staging-1",       // no nonce
            ".export.vrfkit-staging-1-",      // empty nonce
            ".export.vrfkit-staging--1-0",    // empty pid
            ".export.vrfkit-staging-x-0",     // pid is not a number
            ".export.vrfkit-staging-1-0-2",   // one part too many
            ".export.vrfkit-staging-1-0.bak", // a suffix
            ".export.vrfkit-draft-1-0",       // a kind this module never makes
        ] {
            fs::create_dir(root.path().join(near_miss)).unwrap();
        }
        // The right name on a file: this module only ever makes directories.
        fs::write(root.path().join(".export.vrfkit-staging-2-0"), b"file").unwrap();

        let (_transaction, warnings) = begin_capturing(&destination);
        assert_eq!(warnings, "", "near misses must not be reported");
    }

    #[test]
    fn every_generated_name_is_recognised_as_its_own_kind() {
        for kind in [STAGING, PREVIOUS] {
            let name = generated_name(OsStr::new("export"), kind, u64::MAX);
            assert_eq!(generated_kind(&name, OsStr::new("export")), Some(kind));
            assert_eq!(generated_kind(&name, OsStr::new("expor")), None);
            assert_eq!(generated_kind(&name, OsStr::new("exports")), None);
            assert_eq!(generated_kind(&name, OsStr::new("import")), None);
        }
    }

    /// A complete `--checkpoints` export -- every name an export writes -- is
    /// what a re-export replaces, and a table this run does not rewrite is
    /// gone afterwards rather than left mixed in with the new set.
    #[test]
    fn a_successful_publication_replaces_the_directory_as_one_complete_set() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        for name in output_names() {
            fs::write(destination.join(name), b"old").unwrap();
        }

        let (transaction, warnings) = begin_capturing(&destination);
        assert_eq!(warnings, "", "a prior export is not a leftover");
        fs::write(transaction.path().join(MANIFEST), b"new complete").unwrap();
        let mut warnings = Vec::new();
        transaction.publish_reporting_to(&mut warnings).unwrap();

        assert_eq!(
            fs::read(destination.join(MANIFEST)).unwrap(),
            b"new complete"
        );
        assert!(!destination.join(CHECKPOINT_TABLES[0]).exists());
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            1,
            "only the destination remains: no staging and no prior-output backup"
        );
        assert!(
            warnings.is_empty(),
            "{}",
            String::from_utf8_lossy(&warnings)
        );
    }

    /// A destination that does not exist yet, or exists and is empty, has
    /// nothing to lose and is published into as before.
    #[test]
    fn a_missing_or_empty_destination_is_accepted() {
        let root = TestDir::new();
        let empty = root.path().join("empty");
        fs::create_dir(&empty).unwrap();
        for destination in [root.path().join("missing"), empty] {
            let (transaction, warnings) = begin_capturing(&destination);
            assert_eq!(warnings, "");
            fs::write(transaction.path().join(MANIFEST), b"new complete").unwrap();
            transaction.publish().unwrap();
            assert_eq!(
                fs::read(destination.join(MANIFEST)).unwrap(),
                b"new complete"
            );
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    /// An export writes files. A directory by a table's name is not one, and
    /// neither is the tree under it.
    #[test]
    fn a_directory_named_like_a_table_is_a_foreign_entry() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        let impostor = destination.join(CHECKPOINT_TABLES[0]);
        fs::create_dir_all(&impostor).unwrap();
        fs::write(impostor.join("part-0.parquet"), b"someone else's dataset").unwrap();
        fs::write(destination.join(MANIFEST), b"old complete").unwrap();

        let error = OutputTransaction::begin_reporting_to(&destination, &mut Vec::new())
            .err()
            .expect("a directory is never export output");
        assert!(
            error.to_string().contains(&format!(
                "1 entry an export does not write ({})",
                CHECKPOINT_TABLES[0]
            )),
            "{error}"
        );
        assert_eq!(
            fs::read(impostor.join("part-0.parquet")).unwrap(),
            b"someone else's dataset"
        );
    }

    /// Something saved into `--out` after `begin` checked it -- while the
    /// decode ran -- is found again at publication, in the backup that was
    /// about to be deleted, and the backup is kept and named instead.
    #[test]
    fn an_entry_that_appears_during_the_run_keeps_the_prior_output() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join(MANIFEST), b"old complete").unwrap();

        let (transaction, _) = begin_capturing(&destination);
        fs::write(transaction.path().join(MANIFEST), b"new complete").unwrap();
        fs::write(destination.join("notes.txt"), b"saved mid-run").unwrap();
        let mut warnings = Vec::new();
        transaction.publish_reporting_to(&mut warnings).unwrap();

        assert_eq!(
            fs::read(destination.join(MANIFEST)).unwrap(),
            b"new complete"
        );
        let kept: Vec<PathBuf> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path != &destination)
            .collect();
        assert_eq!(kept.len(), 1, "the prior output must survive: {kept:?}");
        assert_eq!(
            generated_kind(kept[0].file_name().unwrap(), OsStr::new("export")),
            Some(PREVIOUS)
        );
        assert_eq!(
            fs::read(kept[0].join("notes.txt")).unwrap(),
            b"saved mid-run"
        );
        let warnings = String::from_utf8(warnings).unwrap();
        assert_eq!(warnings.lines().count(), 1, "{warnings}");
        for expected in [
            "1 entry an export does not write (notes.txt)".to_owned(),
            kept[0].display().to_string(),
        ] {
            assert!(
                warnings.contains(&expected),
                "missing {expected:?}: {warnings}"
            );
        }
    }

    /// A prior output kept because something was saved into it mid-run (see
    /// the test above) holds the user's files. The next export names them,
    /// and does not advise deleting it the way it does a backup that holds
    /// export output only.
    #[test]
    fn a_kept_prior_output_is_reported_with_what_it_holds_besides_export_output() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join(MANIFEST), b"current").unwrap();
        let kept = root.path().join(".export.vrfkit-previous-4242-7");
        let plain = root.path().join(".export.vrfkit-previous-4242-8");
        for backup in [&kept, &plain] {
            fs::create_dir(backup).unwrap();
            fs::write(backup.join(MANIFEST), b"old complete").unwrap();
        }
        fs::write(kept.join("notes.txt"), b"saved mid-run").unwrap();

        let (_transaction, warnings) = begin_capturing(&destination);
        let lines: Vec<&str> = warnings.lines().collect();
        assert_eq!(lines.len(), 2, "one line per leftover: {warnings}");
        assert!(lines[0].contains(&kept.display().to_string()), "{warnings}");
        assert!(
            lines[0].contains("1 entry an export does not write (notes.txt)"),
            "{warnings}"
        );
        assert!(
            lines[0].contains("move what you need out of it"),
            "{warnings}"
        );
        assert!(
            lines[1].contains(&plain.display().to_string()),
            "{warnings}"
        );
        assert!(
            lines[1].ends_with("delete it once no export to that destination is running"),
            "{warnings}"
        );
        assert_eq!(fs::read(kept.join("notes.txt")).unwrap(), b"saved mid-run");
    }

    /// The list stops after a few names; the count never does.
    #[test]
    fn a_long_list_of_foreign_entries_names_the_first_and_counts_the_rest() {
        let names: Vec<OsString> = (0..10).map(|i| OsString::from(format!("f{i}"))).collect();
        assert_eq!(
            name_list(&names),
            "f0, f1, f2, f3, f4, f5, f6, f7, and 2 more"
        );
        assert_eq!(name_list(&names[..2]), "f0, f1");
        assert_eq!(entry_count(1), "1 entry");
        assert_eq!(entry_count(10), "10 entries");
    }

    /// `export dir/match.vrf --out dir` exited 0 with no warning line and left
    /// `dir` holding only the export: publication moved the whole prior
    /// directory aside and deleted it, the replay, the user's files and a
    /// subdirectory with it. Measured with the pinned 12.10 public fixture.
    /// Such a destination is refused before anything is decoded, and left
    /// exactly as it was.
    #[test]
    fn a_destination_holding_foreign_entries_is_refused_and_left_intact() {
        let root = TestDir::new();
        let destination = root.path().join("export");
        fs::create_dir_all(destination.join("sub")).unwrap();
        let kept: [(PathBuf, &[u8]); 4] = [
            (destination.join("match.vrf"), b"the replay itself"),
            (destination.join("precious.txt"), b"a file of the user's"),
            (destination.join("sub").join("notes.txt"), b"nested"),
            (destination.join("manifest.json"), b"old complete"),
        ];
        for (path, bytes) in &kept {
            fs::write(path, bytes).unwrap();
        }

        let outcome = OutputTransaction::begin_reporting_to(&destination, &mut Vec::new())
            .and_then(|transaction| {
                fs::write(transaction.path().join("manifest.json"), b"new complete")?;
                transaction.publish()
            });

        for (path, bytes) in &kept {
            assert_eq!(
                fs::read(path).ok().as_deref(),
                Some(*bytes),
                "{} must survive an export to {}",
                path.display(),
                destination.display()
            );
        }
        let error = outcome.expect_err("a destination holding foreign entries must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(
            message.contains("3 entries an export does not write (match.vrf, precious.txt, sub)"),
            "the refusal must count and name every foreign entry, and only those: {message}"
        );
        assert!(
            message.contains("choose a new or empty directory"),
            "the refusal must say how to proceed: {message}"
        );
        assert!(staging_entries(root.path()).is_empty());
    }
}
