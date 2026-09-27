//! Cross-language pins for the constants `tools/to_valplay_bundle.py` shares
//! with this workspace.
//!
//! # Why this file exists
//!
//! The Python adapter is the only consumer of the export that has to agree
//! with the Rust side on *values*, not on symbols. Two of them decide how
//! every exported row is classified:
//!
//! * `UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME` -- the reserved
//!   `field_name` that marks a whole preserved ClassNetCache block. The
//!   adapter drops those rows before grouping; a value drift makes it keep
//!   them, and a preservation blob is then published as if it were a decoded
//!   field.
//! * `CLASS_NET_CACHE_SUFFIX` -- the group-path suffix that separates an RPC
//!   from a replicated property. A value drift reclassifies every RPC in the
//!   bundle as a replicated property, which produces a complete-looking
//!   document with no kills, no damage and no abilities in it.
//!
//! Neither failure raises anything. `crates/vrf-export/tests/roundtrip.rs`
//! uses the Rust constant by *symbol*, so changing what it points at leaves
//! the whole Rust suite green. This file reads the Python source and compares
//! the literals, so the drift fails here instead of downstream.
//!
//! # Why it parses rather than greps
//!
//! Both values also appear in the adapter's module docstring. A substring
//! search would be satisfied by the prose alone and would keep passing after
//! the code below it changed -- a check that cannot fail. So the assignment
//! statement is located by name and only the string literals inside that
//! statement are compared.
//!
//! # Why the Event allowlists enumerate the parser's table
//!
//! The Rust side of each Event contract is built from
//! `vrf_container::KNOWN_EVENT_GROUPS`, the parser's own list, not from a
//! list kept here. This file used to carry its own seven names, so a group
//! the parser learned passed every test while the adapter, which does not
//! know it, published that group without its words, tag or name. Each
//! dictionary is compared whole, so a group missing on either side fails.

use std::fs;
use std::path::PathBuf;

/// The adapter source, read from the workspace this test was compiled in.
///
/// A missing file is a failure, never a skip: the adapter is the published
/// path from an export to valplay, and "the contract could not be checked"
/// must not read the same as "the contract holds".
fn adapter_source() -> String {
    let path = adapter_path();
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read the valplay adapter at {}: {err}",
            path.display()
        )
    })
}

fn adapter_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/vrfkit; the workspace root is two up. It is
    // fixed at compile time and differs per worktree, so this resolves inside
    // the checkout under test rather than against a working directory that
    // `cargo test` does not promise.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tools")
        .join("to_valplay_bundle.py")
}

/// Every string literal in the top-level `name = ...` assignment, joined.
///
/// Joined rather than taken singly because Python concatenates adjacent
/// literals, so a value split across lines inside parentheses is one value.
/// Returns `None` when no such assignment exists -- which is itself a failure
/// at the call site, not a pass.
fn python_constant(source: &str, name: &str) -> Option<String> {
    let start = source.lines().position(|line| {
        line.starts_with(name) && line[name.len()..].trim_start().starts_with('=')
    })?;

    // Take the statement: the first line, plus continuation lines while the
    // parentheses opened so far have not been closed.
    let mut statement = String::new();
    let mut depth: i32 = 0;
    for line in source.lines().skip(start) {
        statement.push_str(line);
        statement.push('\n');
        for c in line.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 {
            break;
        }
    }

    // Concatenate the contents of every quoted run in the statement. The
    // adapter's constants carry no escapes, so a literal scanner that does not
    // model backslashes is sufficient -- and a value that grew one would stop
    // matching here, which is the safe direction to fail in.
    let mut value = String::new();
    let mut quote: Option<char> = None;
    for c in statement.chars() {
        match quote {
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
            }
            Some(open) => {
                if c == open {
                    quote = None;
                } else {
                    value.push(c);
                }
            }
        }
    }
    Some(value)
}

/// Parse one top-level numeric assignment used by both languages.
fn python_f64_constant(source: &str, name: &str) -> Option<f64> {
    source.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?.trim_start();
        let value = rest.strip_prefix('=')?.trim();
        value.parse().ok()
    })
}

/// Parse the simple string-keyed dictionary literals used for the Event
/// payload allowlist. The adapter intentionally keeps one entry per line, so
/// accepting any other syntax here fails closed instead of attempting to be a
/// Python parser.
fn python_string_dict(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    let start = source.lines().position(|line| {
        line.starts_with(name) && line[name.len()..].trim_start().starts_with('=')
    })?;
    let mut rows = Vec::new();
    for line in source.lines().skip(start + 1) {
        let line = line.trim();
        if line == "}" {
            rows.sort();
            return Some(rows);
        }
        let entry = line.strip_suffix(',')?;
        let (key, value) = entry.split_once(':')?;
        let key = key.trim().strip_prefix('"')?.strip_suffix('"')?;
        rows.push((key.to_string(), value.trim().to_string()));
    }
    None
}

/// One Event allowlist as the parser sees it: every group in
/// `vrf_container::KNOWN_EVENT_GROUPS`, through the accessor the parser
/// itself calls, rendered as the adapter's dictionary literal spells it.
fn rust_event_rows(render: fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut rows = vrf_container::KNOWN_EVENT_GROUPS
        .iter()
        .map(|known| {
            let value = render(known.group).unwrap_or_else(|| {
                panic!(
                    "{} is in KNOWN_EVENT_GROUPS but its accessor returns None",
                    known.group
                )
            });
            (known.group.to_string(), value)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn rust_event_word_counts() -> Vec<(String, String)> {
    rust_event_rows(|group| vrf_container::known_event_word_count(group).map(|n| n.to_string()))
}

fn rust_event_tags() -> Vec<(String, String)> {
    rust_event_rows(|group| vrf_container::known_event_payload_tag(group).map(|t| t.to_string()))
}

fn rust_event_names() -> Vec<(String, String)> {
    rust_event_rows(|group| {
        vrf_container::known_event_payload_name(group).map(|name| format!("\"{name}\""))
    })
}

/// The whole adapter dictionary against the whole parser table. A group
/// either side lacks, or a value that differs, is drift.
fn assert_event_contract(source: &str, dictionary: &str, rust: &[(String, String)], drift: &str) {
    let python = python_string_dict(source, dictionary)
        .unwrap_or_else(|| panic!("the adapter must assign a simple {dictionary} dictionary"));
    assert_eq!(python, rust, "{drift}");
}

fn assert_event_word_count_contract(source: &str) {
    assert_event_contract(
        source,
        "_SERVER_TIMELINE_WORD_COUNTS",
        &rust_event_word_counts(),
        "the adapter's Event word-count allowlist drifted",
    );
}

/// `_SERVER_TIMELINE_WORD_COUNTS` built from the parser's table, minus
/// `skip` and plus `extra`: a dictionary that differs from the parser by
/// exactly one group and nothing else.
fn word_count_dictionary(skip: Option<&str>, extra: Option<(&str, usize)>) -> String {
    let mut source = String::from("_SERVER_TIMELINE_WORD_COUNTS = {\n");
    for known in vrf_container::KNOWN_EVENT_GROUPS {
        if Some(known.group) != skip {
            source.push_str(&format!("    \"{}\": {},\n", known.group, known.word_count));
        }
    }
    if let Some((group, count)) = extra {
        source.push_str(&format!("    \"{group}\": {count},\n"));
    }
    source.push_str("}\n");
    source
}

#[test]
fn adapter_pins_the_unresolved_class_net_cache_field_name() {
    let source = adapter_source();
    let python = python_constant(&source, "UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME")
        .expect("the adapter must assign UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME");
    assert_eq!(
        python,
        vrf_export::UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME,
        "the adapter's reserved field name no longer matches vrf-export's; \
         preserved ClassNetCache blocks would be published as decoded fields"
    );
}

#[test]
fn adapter_pins_the_class_net_cache_suffix() {
    let source = adapter_source();
    let python = python_constant(&source, "CLASS_NET_CACHE_SUFFIX")
        .expect("the adapter must assign CLASS_NET_CACHE_SUFFIX");
    assert_eq!(
        python,
        vrf_schema::CLASS_NET_CACHE_SUFFIX,
        "the adapter's RPC discriminator no longer matches vrf-schema's; \
         every RPC in the bundle would be classified as a replicated property"
    );
}

#[test]
fn adapter_pins_the_event_payload_time_tolerance() {
    let source = adapter_source();
    let python = python_f64_constant(&source, "EVENT_PAYLOAD_TIME_TOLERANCE_MS")
        .expect("the adapter must assign EVENT_PAYLOAD_TIME_TOLERANCE_MS");
    assert_eq!(
        python,
        vrf_container::EVENT_PAYLOAD_TIME_TOLERANCE_MS,
        "the adapter and parser no longer agree on when payload_seconds is structurally valid"
    );
}

#[test]
fn adapter_pins_the_event_payload_word_counts() {
    assert_event_word_count_contract(&adapter_source());
}

/// A source parser that quietly substitutes Rust's values would make the real
/// contract tautological. Keep one complete but deliberately drifted Python
/// dictionary to prove that a one-value disagreement reaches the assertion.
/// Built from the parser's table with one value changed, so it stays
/// complete when the table grows; a hand-written seven-group literal would
/// then fail on the missing group and stop testing the value at all.
#[test]
#[should_panic(expected = "the adapter's Event word-count allowlist drifted")]
fn event_word_count_contract_rejects_one_drifted_value() {
    let complete = word_count_dictionary(None, None);
    let drifted = complete.replacen("\"characterDeath\": 2,", "\"characterDeath\": 1,", 1);
    assert_ne!(
        drifted, complete,
        "the fixture must change exactly one value"
    );
    assert_event_word_count_contract(&drifted);
}

/// The dictionary built from the parser's table passes, so the two tests
/// below fail for the one group they change and not for how the dictionary
/// is written.
#[test]
fn event_word_count_contract_accepts_the_parsers_own_table() {
    assert_event_word_count_contract(&word_count_dictionary(None, None));
}

/// The direction the hand-kept list could not see: the parser knows a group
/// the adapter does not, so the adapter would publish it without its words.
#[test]
#[should_panic(expected = "the adapter's Event word-count allowlist drifted")]
fn event_word_count_contract_rejects_a_group_the_adapter_lacks() {
    let first = vrf_container::KNOWN_EVENT_GROUPS[0].group;
    assert_event_word_count_contract(&word_count_dictionary(Some(first), None));
}

/// The adapter assigns words to a group the parser never decodes.
#[test]
#[should_panic(expected = "the adapter's Event word-count allowlist drifted")]
fn event_word_count_contract_rejects_a_group_the_parser_lacks() {
    assert_event_word_count_contract(&word_count_dictionary(None, Some(("spikeDropped", 0))));
}

#[test]
fn adapter_pins_the_event_payload_tags() {
    assert_event_contract(
        &adapter_source(),
        "_SERVER_TIMELINE_PAYLOAD_TAGS",
        &rust_event_tags(),
        "the adapter's Event tag allowlist drifted",
    );
}

#[test]
fn adapter_pins_the_event_payload_names() {
    assert_event_contract(
        &adapter_source(),
        "_SERVER_TIMELINE_PAYLOAD_NAMES",
        &rust_event_names(),
        "the adapter's public Event-name allowlist drifted",
    );
}

/// The parser itself has to be able to fail, and to see through prose.
///
/// Without this, a `python_constant` that always returned the Rust value --
/// or that matched the first quoted text anywhere in the file -- would keep
/// both tests above green forever.
#[test]
fn the_constant_scanner_reads_the_assignment_and_not_the_prose() {
    let source = concat!(
        "\"\"\"A docstring mentioning WIDGET = \"decoy\" in prose.\"\"\"\n",
        "\n",
        "OTHER = \"not this one\"\n",
        "WIDGET = (\n",
        "    \"real\"\n",
        "    \"_value\"\n",
        ")\n",
        "TRAILING = \"after\"\n",
    );
    assert_eq!(
        python_constant(source, "WIDGET").as_deref(),
        Some("real_value")
    );
    assert_eq!(
        python_constant(source, "OTHER").as_deref(),
        Some("not this one")
    );
    assert_eq!(
        python_constant(source, "ABSENT"),
        None,
        "a missing assignment must be reported, not silently treated as empty"
    );
    assert_eq!(python_f64_constant("LIMIT = 1.001\n", "LIMIT"), Some(1.001));
    assert_eq!(python_f64_constant("LIMIT = nope\n", "LIMIT"), None);
    assert_eq!(
        python_string_dict(
            "VALUES = {\n    \"two\": \"second\",\n    \"one\": 1,\n}\n",
            "VALUES",
        ),
        Some(vec![
            ("one".to_string(), "1".to_string()),
            ("two".to_string(), "\"second\"".to_string()),
        ])
    );
}
