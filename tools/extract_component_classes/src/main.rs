//! Read component classes out of an installed game's IoStore containers.
//!
//! A replay names a Blueprint component only by its instance name
//! (`ZoomStateMachine`), and the class it replicates under -- the group the
//! replay declares -- is not derivable from that name. The cooked game says
//! what it is. For every component template in every package this prints the
//! instance name, the package that owns it, and its class, resolved through
//! the script object map in `global.ucas` to a `/Script/...` path.
//!
//! This is what `KNOWN_SUBOBJECT_CLASS_PATHS` in
//! `crates/vrfkit/src/sink/paths.rs` is derived from. docs/DATA.md ("Reading
//! component classes out of the game") has the procedure and what the output
//! does and does not establish.
//!
//! Read-only: files are opened for reading, shared with every other handle,
//! and nothing is written anywhere except `--out`.
//!
//! Usage:
//!   extract-component-classes <PAKS_DIR> [--format tsv|json] [--kind all|gen_variable|cdo_subobject]
//!                             [--name NAME]... [--jobs N] [--out FILE]
//!
//! Exit status: 0 when every package was read and every self-check held; 1
//! when anything could not be read or a check failed (the rows that could be
//! read are still written, and the summary says what is missing); 2 for a
//! usage or setup error.

#![forbid(unsafe_code)]

mod cityhash;
mod container;
mod dirindex;
mod names;
mod reader;
mod scan;
mod script;
mod toc;
mod zen;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use container::Container;
use scan::{ClassTable, Job, PackageScan, Resolved, resolve, scan_package};
use script::{ScriptCheck, ScriptObjects, parse_script_objects};
use toc::{CHUNK_EXPORT_BUNDLE_DATA, CHUNK_SCRIPT_OBJECTS};

const USAGE: &str = "usage: extract-component-classes <PAKS_DIR> [--format tsv|json] \
[--kind all|gen_variable|cdo_subobject] [--name NAME]... [--jobs N] [--out FILE]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Tsv,
    Json,
}

#[derive(Debug)]
struct Args {
    paks: PathBuf,
    format: Format,
    kind: Option<String>,
    names: BTreeSet<String>,
    jobs: usize,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut paks = None;
    let mut format = Format::Tsv;
    let mut kind = None;
    let mut names = BTreeSet::new();
    let mut jobs = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let mut out = None;
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--format" => {
                format = match value("--format")?.as_str() {
                    "tsv" => Format::Tsv,
                    "json" => Format::Json,
                    other => return Err(format!("unknown --format {other}")),
                }
            }
            "--kind" => {
                let k = value("--kind")?;
                match k.as_str() {
                    "all" => kind = None,
                    "gen_variable" | "cdo_subobject" => kind = Some(k),
                    other => return Err(format!("unknown --kind {other}")),
                }
            }
            "--name" => {
                names.insert(value("--name")?);
            }
            "--jobs" => {
                jobs = value("--jobs")?
                    .parse()
                    .map_err(|_| "--jobs needs a positive number".to_owned())?;
                if jobs == 0 {
                    return Err("--jobs needs a positive number".to_owned());
                }
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "-h" | "--help" => return Err(String::new()),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => {
                if paks.replace(PathBuf::from(path)).is_some() {
                    return Err("more than one PAKS_DIR given".to_owned());
                }
            }
        }
    }
    let paks = paks.ok_or_else(|| "PAKS_DIR is required".to_owned())?;
    Ok(Args {
        paks,
        format,
        kind,
        names,
        jobs,
        out,
    })
}

/// One output row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    instance: String,
    kind: &'static str,
    asset: String,
    export: String,
    class: String,
    class_kind: &'static str,
    native_class: String,
    outer: String,
    class_ref: String,
    container: String,
}

/// Every counter the run keeps. All of them are printed, zeros included: a
/// line that appears only when nonzero cannot tell "nothing went wrong" from
/// "this code never ran".
#[derive(Debug, Default)]
struct Counts {
    containers: usize,
    legacy_paks_not_read: usize,
    toc_entries: usize,
    indexed_files: usize,
    /// Directory-index files naming a TOC entry past the end of the TOC, or
    /// one a later file also names: neither can be attached to a chunk.
    indexed_files_dropped: usize,
    package_chunks: usize,
    package_chunks_unindexed: usize,
    package_files_not_package_chunks: usize,
    packages_read: usize,
    packages_failed: usize,
    package_id_matches: usize,
    package_id_mismatches: usize,
    file_stem_matches: usize,
    file_stem_mismatches: usize,
    exports: usize,
    class_objects: usize,
    duplicate_class_keys: usize,
    gen_variable: usize,
    gen_variable_numbered: usize,
    cdo_subobject: usize,
    class_kinds: BTreeMap<&'static str, usize>,
    script: ScriptCheck,
}

struct Provenance {
    name: String,
    container_id: u64,
    toc_entries: usize,
    package_chunks: usize,
    /// `None` when the file's metadata cannot be read.
    utoc_bytes: Option<u64>,
    ucas_bytes: Option<u64>,
    ucas_modified: String,
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
    }
}

fn run(args: &Args) -> Result<i32, String> {
    let mut counts = Counts::default();
    let (utocs, legacy) = discover(&args.paks)?;
    counts.legacy_paks_not_read = legacy.len();

    let global_path = args.paks.join("global.utoc");
    if !global_path.is_file() {
        return Err(format!("{}: not found", global_path.display()));
    }
    let (script, global) = load_script_objects(&global_path)?;
    counts.script = script.verify();

    let mut containers = Vec::new();
    // Every `/Script` path in the output comes from global, so it is listed
    // with the containers the packages came from. It holds no package chunk
    // this tool reads.
    let mut provenance = vec![provenance_of(&global, &global_path, 0)];
    let mut jobs = Vec::new();
    for utoc in &utocs {
        if utoc
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        {
            continue;
        }
        let container = Container::open(utoc).map_err(|e| e.to_string())?;
        let ci = containers.len();
        counts.containers += 1;
        counts.toc_entries += container.toc.chunk_ids.len();

        let mut by_entry: HashMap<u32, String> = HashMap::new();
        if !container.toc.directory_index.is_empty() {
            let index = dirindex::parse_directory_index(&container.toc.directory_index)
                .and_then(|d| d.files())
                .map_err(|e| format!("{}: {e}", container.name))?;
            counts.indexed_files += index.len();
            for (path, entry) in index {
                if entry as usize >= container.toc.chunk_ids.len()
                    || by_entry.insert(entry, path).is_some()
                {
                    counts.indexed_files_dropped += 1;
                }
            }
        }
        let mut packages_here = 0usize;
        for (entry, id) in container.toc.chunk_ids.iter().enumerate() {
            let file = by_entry.remove(&(entry as u32));
            if id.chunk_type != CHUNK_EXPORT_BUNDLE_DATA {
                if file.as_deref().is_some_and(is_package_file) {
                    counts.package_files_not_package_chunks += 1;
                }
                continue;
            }
            packages_here += 1;
            if file.is_none() {
                counts.package_chunks_unindexed += 1;
            }
            jobs.push(Job {
                container: ci,
                entry,
                file,
            });
        }
        counts.package_chunks += packages_here;
        provenance.push(provenance_of(&container, utoc, packages_here));
        containers.push(container);
    }
    if containers.is_empty() {
        return Err(format!(
            "{}: no .utoc containers besides global",
            args.paks.display()
        ));
    }

    let (scans, failures) = scan_all(&containers, &jobs, &script, args.jobs);
    counts.packages_failed = failures.len();

    // Second pass: every class object from every package, so a component
    // whose class is a Blueprint in another package can be followed there.
    let mut classes = ClassTable::new();
    for (job, scan) in &scans {
        counts.packages_read += 1;
        counts.exports += scan.exports;
        if scan.package_id_matches {
            counts.package_id_matches += 1;
        } else {
            counts.package_id_mismatches += 1;
        }
        match scan.file_stem_matches {
            Some(true) => counts.file_stem_matches += 1,
            Some(false) => counts.file_stem_mismatches += 1,
            None => {}
        }
        let id = containers[job.container].toc.chunk_ids[job.entry].id;
        for (hash, class) in &scan.classes {
            counts.class_objects += 1;
            if classes.insert((id, *hash), class.clone()).is_some() {
                counts.duplicate_class_keys += 1;
            }
        }
    }

    let mut rows = Vec::new();
    for (job, scan) in &scans {
        for cand in &scan.candidates {
            match cand.kind {
                "gen_variable" => counts.gen_variable += 1,
                _ => counts.cdo_subobject += 1,
            }
            if cand.numbered {
                counts.gen_variable_numbered += 1;
            }
            let Resolved {
                class,
                class_kind,
                native_class,
                class_ref,
            } = resolve(&cand.class, &script, &classes);
            *counts.class_kinds.entry(class_kind).or_insert(0) += 1;
            if args.kind.as_deref().is_some_and(|k| k != cand.kind) {
                continue;
            }
            if !args.names.is_empty() && !args.names.contains(&cand.instance) {
                continue;
            }
            rows.push(Row {
                instance: cand.instance.clone(),
                kind: cand.kind,
                asset: scan.name.clone(),
                export: cand.export.clone(),
                class,
                class_kind,
                native_class,
                outer: cand.outer.clone(),
                class_ref,
                container: containers[job.container].name.clone(),
            });
        }
    }
    rows.sort();

    let text = match args.format {
        Format::Tsv => render_tsv(&rows),
        Format::Json => render_json(&rows, &counts, &provenance, &args.paks),
    };
    match &args.out {
        Some(path) => {
            std::fs::write(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(text.as_bytes())
                .and_then(|_| lock.flush())
                .map_err(|e| format!("stdout: {e}"))?;
        }
    }

    let failure_lines: Vec<String> = failures
        .iter()
        .map(|(job, e)| {
            format!(
                "{} entry {} ({}): {e}",
                containers[job.container].name,
                job.entry,
                job.file.as_deref().unwrap_or("unindexed")
            )
        })
        .collect();
    report(&counts, &provenance, &legacy, &failure_lines, &rows, args);
    let failed = counts.packages_failed > 0
        || counts.package_id_mismatches > 0
        || counts.script.hash_mismatches > 0
        || counts.script.paths_resolved != counts.script.objects
        || counts.packages_read == 0;
    if failed {
        eprintln!(
            "FAILED: the listing above is incomplete or a self-check did not hold; \
             see the counters. Rows that could be read were still written."
        );
        return Ok(1);
    }
    eprintln!("OK: every package read, every self-check held");
    Ok(0)
}

fn is_package_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".uasset") || lower.ends_with(".umap")
}

/// The `.utoc` files and the legacy `.pak` files in `dir`, each sorted by name.
/// Legacy paks are listed, not read, so the summary can say what was skipped.
fn discover(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut utocs = Vec::new();
    let mut paks = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "utoc" => utocs.push(path),
            "pak" => paks.push(path),
            _ => {}
        }
    }
    utocs.sort();
    paks.sort();
    if utocs.is_empty() {
        return Err(format!("{}: no .utoc files", dir.display()));
    }
    Ok((utocs, paks))
}

/// Which files a run read, so its output can be matched to them.
fn provenance_of(container: &Container, utoc: &Path, package_chunks: usize) -> Provenance {
    Provenance {
        name: container.name.clone(),
        container_id: container.toc.header.container_id,
        toc_entries: container.toc.chunk_ids.len(),
        package_chunks,
        utoc_bytes: file_len(utoc),
        ucas_bytes: file_len(&container.ucas_path),
        ucas_modified: modified(&container.ucas_path),
    }
}

fn load_script_objects(global: &Path) -> Result<(ScriptObjects, Container), String> {
    let container = Container::open(global).map_err(|e| e.to_string())?;
    let entries: Vec<usize> = container
        .toc
        .chunk_ids
        .iter()
        .enumerate()
        .filter(|(_, id)| id.chunk_type == CHUNK_SCRIPT_OBJECTS)
        .map(|(i, _)| i)
        .collect();
    if entries.len() != 1 {
        return Err(format!(
            "{}: {} script object chunks, expected exactly 1",
            global.display(),
            entries.len()
        ));
    }
    let mut ucas = container.open_ucas().map_err(|e| e.to_string())?;
    let bytes = container
        .read_chunk(&mut ucas, entries[0], u64::MAX)
        .map_err(|e| e.to_string())?;
    let script = parse_script_objects(&bytes).map_err(|e| format!("{}: {e}", global.display()))?;
    Ok((script, container))
}

type Scanned = Vec<(Job, PackageScan)>;
type Failed = Vec<(Job, String)>;

/// Read every package on `workers` threads. Results come back in job order, so
/// the output does not depend on scheduling.
fn scan_all(
    containers: &[Container],
    jobs: &[Job],
    script: &ScriptObjects,
    workers: usize,
) -> (Scanned, Failed) {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<PackageScan, String>)>> =
        Mutex::new(Vec::with_capacity(jobs.len()));
    std::thread::scope(|s| {
        for _ in 0..workers.min(jobs.len().max(1)) {
            s.spawn(|| {
                let mut handles: Vec<Option<std::fs::File>> =
                    (0..containers.len()).map(|_| None).collect();
                let mut local = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    let container = &containers[job.container];
                    let outcome = match &mut handles[job.container] {
                        Some(file) => {
                            scan_package(container, file, job, script).map_err(|e| e.to_string())
                        }
                        slot => match container.open_ucas() {
                            Ok(file) => {
                                let file = slot.insert(file);
                                scan_package(container, file, job, script)
                                    .map_err(|e| e.to_string())
                            }
                            Err(e) => Err(e.to_string()),
                        },
                    };
                    local.push((i, outcome));
                }
                results
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend(local);
            });
        }
    });
    let mut all = results.into_inner().unwrap_or_else(|p| p.into_inner());
    all.sort_by_key(|(i, _)| *i);
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (i, outcome) in all {
        match outcome {
            Ok(scan) => ok.push((jobs[i].clone(), scan)),
            Err(e) => failed.push((jobs[i].clone(), e)),
        }
    }
    (ok, failed)
}

/// A file's size, `None` when its metadata cannot be read: printed as `?` and
/// as JSON `null`, never as a plausible 0.
fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).map(|m| m.len()).ok()
}

fn size_text(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "?".to_owned(), |n| n.to_string())
}

fn json_size(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "null".to_owned(), |n| n.to_string())
}

fn modified(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(utc_timestamp)
        .unwrap_or_else(|_| "?".to_owned())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a file time, without a date crate.
fn utc_timestamp(t: SystemTime) -> String {
    let Ok(since) = t.duration_since(UNIX_EPOCH) else {
        return "?".to_owned();
    };
    let secs = since.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const COLUMNS: [&str; 10] = [
    "instance",
    "kind",
    "class",
    "class_kind",
    "native_class",
    "asset",
    "export",
    "outer",
    "class_ref",
    "container",
];

fn row_fields(r: &Row) -> [&str; 10] {
    [
        &r.instance,
        r.kind,
        &r.class,
        r.class_kind,
        &r.native_class,
        &r.asset,
        &r.export,
        &r.outer,
        &r.class_ref,
        &r.container,
    ]
}

fn render_tsv(rows: &[Row]) -> String {
    let mut out = COLUMNS.join("\t");
    out.push('\n');
    for r in rows {
        let fields: Vec<String> = row_fields(r).iter().map(|f| tsv_escape(f)).collect();
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

/// Tabs, newlines and backslashes cannot appear raw in a TSV cell.
fn tsv_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn render_json(rows: &[Row], counts: &Counts, provenance: &[Provenance], paks: &Path) -> String {
    let mut out = String::from("{\n  \"paks_dir\": ");
    json_string(&mut out, &paks.display().to_string());
    out.push_str(",\n  \"containers\": [");
    for (i, p) in provenance.iter().enumerate() {
        out.push_str(if i == 0 { "\n    {" } else { ",\n    {" });
        out.push_str("\"name\": ");
        json_string(&mut out, &p.name);
        let _ = write!(
            out,
            ", \"container_id\": \"{:#018x}\", \"toc_entries\": {}, \"package_chunks\": {}, \
             \"utoc_bytes\": {}, \"ucas_bytes\": {}, \"ucas_modified\": ",
            p.container_id,
            p.toc_entries,
            p.package_chunks,
            json_size(p.utoc_bytes),
            json_size(p.ucas_bytes)
        );
        json_string(&mut out, &p.ucas_modified);
        out.push('}');
    }
    out.push_str("\n  ],\n  \"counts\": {");
    let pairs = count_pairs(counts);
    for (i, (k, v)) in pairs.iter().enumerate() {
        out.push_str(if i == 0 { "\n    " } else { ",\n    " });
        json_string(&mut out, k);
        let _ = write!(out, ": {v}");
    }
    out.push_str("\n  },\n  \"rows\": [");
    for (i, r) in rows.iter().enumerate() {
        out.push_str(if i == 0 { "\n    {" } else { ",\n    {" });
        for (j, (col, val)) in COLUMNS.iter().zip(row_fields(r)).enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            json_string(&mut out, col);
            out.push_str(": ");
            json_string(&mut out, val);
        }
        out.push('}');
    }
    out.push_str("\n  ]\n}\n");
    out
}

fn count_pairs(c: &Counts) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = vec![
        ("containers".into(), c.containers),
        ("legacy_paks_not_read".into(), c.legacy_paks_not_read),
        ("toc_entries".into(), c.toc_entries),
        ("indexed_files".into(), c.indexed_files),
        ("indexed_files_dropped".into(), c.indexed_files_dropped),
        ("package_chunks".into(), c.package_chunks),
        (
            "package_chunks_unindexed".into(),
            c.package_chunks_unindexed,
        ),
        (
            "package_files_not_package_chunks".into(),
            c.package_files_not_package_chunks,
        ),
        ("packages_read".into(), c.packages_read),
        ("packages_failed".into(), c.packages_failed),
        ("package_id_matches".into(), c.package_id_matches),
        ("package_id_mismatches".into(), c.package_id_mismatches),
        ("file_stem_matches".into(), c.file_stem_matches),
        ("file_stem_mismatches".into(), c.file_stem_mismatches),
        ("exports".into(), c.exports),
        ("class_objects".into(), c.class_objects),
        ("duplicate_class_keys".into(), c.duplicate_class_keys),
        ("script_objects".into(), c.script.objects),
        ("script_paths_resolved".into(), c.script.paths_resolved),
        ("script_hash_matches".into(), c.script.hash_matches),
        ("script_hash_mismatches".into(), c.script.hash_mismatches),
        ("gen_variable".into(), c.gen_variable),
        ("gen_variable_numbered".into(), c.gen_variable_numbered),
        ("cdo_subobject".into(), c.cdo_subobject),
    ];
    for kind in [
        "script_import",
        "script_import_unresolved",
        "package_import",
        "package_import_unresolved",
        "export",
        "export_unresolved",
        "null",
        "bad_index",
    ] {
        v.push((
            format!("class_kind.{kind}"),
            c.class_kinds.get(kind).copied().unwrap_or(0),
        ));
    }
    v
}

fn report(
    counts: &Counts,
    provenance: &[Provenance],
    legacy: &[PathBuf],
    failures: &[String],
    rows: &[Row],
    args: &Args,
) {
    let mut err = String::new();
    let _ = writeln!(err, "paks: {}", args.paks.display());
    for p in provenance {
        let _ = writeln!(
            err,
            "  {:<32} id {:#018x}  {:>7} chunks  {:>6} packages  ucas {:>12} bytes  modified {}",
            p.name,
            p.container_id,
            p.toc_entries,
            p.package_chunks,
            size_text(p.ucas_bytes),
            p.ucas_modified
        );
    }
    for pak in legacy {
        let _ = writeln!(
            err,
            "  not read (legacy .pak): {}",
            pak.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        );
    }
    for (k, v) in count_pairs(counts) {
        let _ = writeln!(err, "  {k:<36} {v}");
    }
    let _ = writeln!(err, "  rows written                         {}", rows.len());
    for line in failures.iter().take(20) {
        let _ = writeln!(err, "  failed: {line}");
    }
    if failures.len() > 20 {
        let _ = writeln!(err, "  ... and {} more failures", failures.len() - 20);
    }
    if !args.names.is_empty() {
        let found: BTreeSet<&str> = rows.iter().map(|r| r.instance.as_str()).collect();
        for name in &args.names {
            if !found.contains(name.as_str()) {
                let _ = writeln!(
                    err,
                    "  not found: {name} (no {} export has this instance name)",
                    args.kind
                        .as_deref()
                        .unwrap_or("gen_variable or cdo_subobject")
                );
            }
        }
    }
    eprint!("{err}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_render_in_utc() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_354), (2025, 9, 23));
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_758_591_394);
        assert_eq!(utc_timestamp(t), "2025-09-23T01:36:34Z");
    }

    /// A one-chunk TOC over stored (uncompressed) bytes.
    fn stored_toc(len: usize, chunk_type: u8, directory_index: Vec<u8>) -> Vec<u8> {
        use crate::toc::tests::{TocSpec, build_toc};
        use crate::toc::{ChunkId, CompressedBlock, FLAG_INDEXED, OffsetLength};
        build_toc(&TocSpec {
            flags: FLAG_INDEXED,
            block_size: 0x10000,
            methods: vec![],
            chunks: vec![(
                ChunkId {
                    id: 1,
                    index: 0,
                    chunk_type,
                },
                OffsetLength {
                    offset: 0,
                    length: len as u64,
                },
            )],
            blocks: vec![CompressedBlock {
                offset: 0,
                compressed_size: len as u32,
                uncompressed_size: len as u32,
                method: 0,
            }],
            directory_index,
            ..crate::toc::tests::TocSpec::default()
        })
    }

    /// Run the tool with `--format json` over a synthetic Paks directory: a
    /// global container and one container `other` whose single chunk is not
    /// a package, so there is nothing to scan and the run fails, but still
    /// reports. Returns the exit code and the JSON.
    fn run_synthetic(test: &str, other_index: Vec<u8>) -> (Result<i32, String>, String) {
        use crate::script::tests::build_script_objects;
        let dir = std::env::temp_dir().join(format!("ecc-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = build_script_objects(&["/Script/A"], &[(0, 0, "/Script/A", None)]);
        let global = stored_toc(script.len(), CHUNK_SCRIPT_OBJECTS, Vec::new());
        std::fs::write(dir.join("global.utoc"), global).unwrap();
        std::fs::write(dir.join("global.ucas"), &script).unwrap();
        std::fs::write(dir.join("other.utoc"), stored_toc(4, 2, other_index)).unwrap();
        std::fs::write(dir.join("other.ucas"), [0u8; 4]).unwrap();
        let out = dir.join("out.json");
        let args = Args {
            paks: dir.clone(),
            format: Format::Json,
            kind: None,
            names: BTreeSet::new(),
            jobs: 1,
            out: Some(out.clone()),
        };
        let code = run(&args);
        let json = std::fs::read_to_string(&out);
        std::fs::remove_dir_all(&dir).unwrap();
        (code, json.unwrap())
    }

    /// The run lists every container it read, global included: the script
    /// object map, and so every `/Script` path in the output, comes from
    /// there.
    #[test]
    fn provenance_lists_the_global_container() {
        let (code, json) = run_synthetic("provenance", Vec::new());
        assert_eq!(code, Ok(1));
        let names: Vec<&str> = json
            .match_indices("\"name\": \"")
            .map(|(at, key)| {
                let rest = &json[at + key.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(names, ["global", "other"]);
    }

    /// A directory-index file that names an entry past the TOC, or an entry
    /// another file also names, cannot be attached to a chunk. Both used to
    /// vanish from the listing without a count.
    #[test]
    fn directory_index_files_that_name_no_entry_of_their_own_are_counted() {
        use crate::dirindex::tests::build_index;
        const NONE: u32 = u32::MAX;
        // One directory holding three files, for TOC entries 0, 0 and 9; the
        // TOC has one entry.
        let index = build_index(
            "../../../",
            &[(NONE, NONE, NONE, 0)],
            &[(0, 1, 0), (1, 2, 0), (2, NONE, 9)],
            &["a.ubulk", "b.ubulk", "c.ubulk"],
        );
        let (code, json) = run_synthetic("dropped", index);
        assert_eq!(code, Ok(1));
        assert!(json.contains("\"indexed_files\": 3,"), "{json}");
        assert!(json.contains("\"indexed_files_dropped\": 2,"), "{json}");
        let (_, json) = run_synthetic("none-dropped", Vec::new());
        assert!(json.contains("\"indexed_files_dropped\": 0,"), "{json}");
    }

    #[test]
    fn a_size_that_cannot_be_read_is_absent_not_zero() {
        let missing = Path::new("no such dir/no such file.ucas");
        assert_eq!(file_len(missing), None);
        assert_eq!(size_text(file_len(missing)), "?");
        assert_eq!(json_size(file_len(missing)), "null");
        // Tests run from the package root.
        let here = Path::new("Cargo.toml");
        let len = std::fs::metadata(here).unwrap().len();
        assert_eq!(size_text(file_len(here)), len.to_string());
    }

    #[test]
    fn json_and_tsv_escape_what_they_must() {
        let mut s = String::new();
        json_string(&mut s, "a\"b\\c\td\u{e9}");
        assert_eq!(s, "\"a\\\"b\\\\c\\td\\u00e9\"");
        assert_eq!(tsv_escape("a\tb\\c\n"), "a\\tb\\\\c\\n");
    }
}
