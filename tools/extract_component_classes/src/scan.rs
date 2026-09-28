//! Walk every package in every container and pick out component templates.
//!
//! Two export shapes carry a component's instance name:
//!
//! - `gen_variable`: a Blueprint-added component. Its template is an export
//!   named `<Name>_GEN_VARIABLE`, and the component spawned from it is named
//!   `<Name>` -- the bare string the replay sends. Inherited-component
//!   overrides in child Blueprints reuse the same name and class.
//! - `cdo_subobject`: a component the class creates in C++. It has no
//!   `_GEN_VARIABLE` template; it appears as a subobject of the Blueprint's
//!   class default object (`Default__<Class>`), under its instance name.
//!
//! Each row's class comes from the export's `ClassIndex`:
//!
//! - a script import is looked up in `global.ucas` -- a `/Script/...` class;
//! - a package import names a public export of another package, which the
//!   second pass resolves to that package's class and then walks up its super
//!   chain to the first native class;
//! - anything that cannot be resolved is `?`, never a guess.

use std::collections::HashMap;

use crate::cityhash::{INDEX_MASK, hash_package_name};
use crate::container::Container;
use crate::reader::{Result, fail};
use crate::script::{
    KIND_EXPORT, KIND_NULL, KIND_PACKAGE_IMPORT, KIND_SCRIPT_IMPORT, ScriptObjects, index_kind,
};
use crate::toc::CHUNK_EXPORT_BUNDLE_DATA;
use crate::zen::{PackageHeader, declared_header_size, parse_package_header};

pub const GEN_VARIABLE_SUFFIX: &str = "_GEN_VARIABLE";
pub const CDO_PREFIX: &str = "Default__";

/// One package to read: a container, a TOC entry, and the file name the
/// directory index gives it (if any).
#[derive(Debug, Clone)]
pub struct Job {
    pub container: usize,
    pub entry: usize,
    pub file: Option<String>,
}

/// Where an export's class index points, before the cross-package pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassRef {
    Script(u64),
    /// `(package id, package name as imported, public export hash)`.
    Package(u64, String, u64),
    /// Another export of the same package: `(package id, its public export
    /// hash, its name)`. A hash of 0 means the export is not public and cannot
    /// be looked up.
    Local(u64, u64, String),
    Null,
    /// The index could not be decoded against this package's tables.
    Bad(String),
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub kind: &'static str,
    pub instance: String,
    pub export: String,
    pub outer: String,
    pub class: ClassRef,
    /// A `_GEN_VARIABLE` export whose FName carries an instance number. The
    /// component name that would spawn from it is not established, so these
    /// are counted and listed with the number kept on the export name.
    pub numbered: bool,
}

/// A class defined by some package, keyed elsewhere by `(package id, public
/// export hash)`.
#[derive(Debug, Clone)]
pub struct ClassExport {
    pub path: String,
    pub super_ref: ClassRef,
}

#[derive(Debug, Clone)]
pub struct PackageScan {
    pub name: String,
    pub package_id_matches: bool,
    /// `None` when the directory index does not name this chunk.
    pub file_stem_matches: Option<bool>,
    pub exports: usize,
    pub candidates: Vec<Candidate>,
    pub classes: Vec<(u64, ClassExport)>,
}

/// Decode `index` as it appears in `pkg`'s export map.
pub fn class_ref(pkg: &PackageHeader, package_id: u64, index: u64) -> ClassRef {
    match index_kind(index) {
        KIND_NULL => ClassRef::Null,
        KIND_SCRIPT_IMPORT => ClassRef::Script(index),
        KIND_PACKAGE_IMPORT => {
            let raw = index & INDEX_MASK;
            let pkg_index = (raw >> 32) as usize;
            let hash_index = (raw & 0xffff_ffff) as usize;
            let (Some(name), Some(&hash)) = (
                pkg.imported_package_names.get(pkg_index),
                pkg.imported_public_export_hashes.get(hash_index),
            ) else {
                return ClassRef::Bad(format!(
                    "package import {pkg_index}/{hash_index} outside {} packages / {} hashes",
                    pkg.imported_package_names.len(),
                    pkg.imported_public_export_hashes.len()
                ));
            };
            ClassRef::Package(hash_package_name(name), name.clone(), hash)
        }
        KIND_EXPORT => match pkg.exports.get(index as usize) {
            Some(e) => ClassRef::Local(package_id, e.public_export_hash, pkg.export_name(e)),
            None => ClassRef::Bad(format!(
                "local export {index} outside {} exports",
                pkg.exports.len()
            )),
        },
        _ => unreachable!("two bits have four values"),
    }
}

/// True for a class object: its own class is a native class whose name ends
/// in `Class` (`/Script/Engine.BlueprintGeneratedClass`,
/// `/Script/CoreUObject.Class`, `/Script/UMG.WidgetBlueprintGeneratedClass`).
fn is_class_object(script: &ScriptObjects, class_index: u64) -> bool {
    if index_kind(class_index) != KIND_SCRIPT_IMPORT {
        return false;
    }
    script.path_of(class_index).is_some_and(|p| {
        p.rsplit(['.', ':'])
            .next()
            .is_some_and(|leaf| leaf.ends_with("Class"))
    })
}

/// Pick the component templates and class objects out of one parsed package.
pub fn scan_header(
    pkg: &PackageHeader,
    package_id: u64,
    script: &ScriptObjects,
) -> (Vec<Candidate>, Vec<(u64, ClassExport)>) {
    let mut candidates = Vec::new();
    let mut classes = Vec::new();
    for export in &pkg.exports {
        let base = export
            .object_name
            .base(&pkg.names)
            .unwrap_or("?")
            .to_owned();
        let rendered = pkg.export_name(export);
        let outer = if index_kind(export.outer_index) == KIND_EXPORT {
            pkg.exports.get(export.outer_index as usize)
        } else {
            None
        };
        let outer_name = outer.map_or_else(String::new, |o| pkg.export_name(o));

        if is_class_object(script, export.class_index) && export.public_export_hash != 0 {
            let path = match outer {
                None => format!("{}.{rendered}", pkg.name),
                Some(_) => format!("{}.{outer_name}:{rendered}", pkg.name),
            };
            classes.push((
                export.public_export_hash,
                ClassExport {
                    path,
                    super_ref: class_ref(pkg, package_id, export.super_index),
                },
            ));
        }

        if let Some(stem) = base.strip_suffix(GEN_VARIABLE_SUFFIX) {
            let numbered = export.object_name.number != 0;
            let instance = if numbered {
                format!("{stem}_{}", export.object_name.number - 1)
            } else {
                stem.to_owned()
            };
            candidates.push(Candidate {
                kind: "gen_variable",
                instance,
                export: rendered,
                outer: outer_name,
                class: class_ref(pkg, package_id, export.class_index),
                numbered,
            });
            continue;
        }
        let is_cdo_child = outer.is_some_and(|o| {
            o.object_name
                .base(&pkg.names)
                .unwrap_or("")
                .starts_with(CDO_PREFIX)
        });
        if is_cdo_child {
            candidates.push(Candidate {
                kind: "cdo_subobject",
                instance: rendered.clone(),
                export: rendered,
                outer: outer_name,
                class: class_ref(pkg, package_id, export.class_index),
                numbered: false,
            });
        }
    }
    (candidates, classes)
}

/// Read and scan one package chunk.
pub fn scan_package(
    container: &Container,
    ucas: &mut std::fs::File,
    job: &Job,
    script: &ScriptObjects,
) -> Result<PackageScan> {
    let toc = &container.toc;
    let Some(id) = toc.chunk_ids.get(job.entry) else {
        return fail(format!("chunk {} out of range", job.entry));
    };
    if id.chunk_type != CHUNK_EXPORT_BUNDLE_DATA {
        return fail(format!(
            "chunk {} is type {}, not a package",
            job.entry, id.chunk_type
        ));
    }
    let chunk = toc.chunks[job.entry];
    let block_size = u64::from(toc.block_size);
    let first_block = block_size - chunk.offset % block_size;
    let mut data = container.read_chunk(ucas, job.entry, first_block)?;
    let header_size = u64::from(declared_header_size(&data)?);
    if header_size > data.len() as u64 {
        if header_size > chunk.length {
            return fail(format!(
                "header size {header_size} exceeds the {}-byte chunk",
                chunk.length
            ));
        }
        data = container.read_chunk(ucas, job.entry, header_size)?;
    }
    let pkg = parse_package_header(&data)?;
    let package_id_matches = hash_package_name(&pkg.name) == id.id;
    let file_stem_matches = job.file.as_ref().map(|f| {
        let file = f.rsplit('/').next().unwrap_or(f);
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        let leaf = pkg.name.rsplit('/').next().unwrap_or(&pkg.name);
        stem.eq_ignore_ascii_case(leaf)
    });
    let (candidates, classes) = scan_header(&pkg, id.id, script);
    Ok(PackageScan {
        name: pkg.name.clone(),
        package_id_matches,
        file_stem_matches,
        exports: pkg.exports.len(),
        candidates,
        classes,
    })
}

/// Every class object seen, keyed by `(package id, public export hash)`.
/// `scan_header` records public classes only, so no key has hash 0 and a
/// non-public `ClassRef::Local` finds nothing here.
pub type ClassTable = HashMap<(u64, u64), ClassExport>;

/// The resolved class of a candidate: `(class path, how it resolved, first
/// native class, what the index pointed at)`. Unresolvable parts are `?`.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub class: String,
    pub class_kind: &'static str,
    pub native_class: String,
    pub class_ref: String,
}

impl Resolved {
    fn unresolved(class_kind: &'static str, class_ref: String) -> Self {
        Resolved {
            class: "?".to_owned(),
            class_kind,
            native_class: "?".to_owned(),
            class_ref,
        }
    }
}

pub fn resolve(class: &ClassRef, script: &ScriptObjects, classes: &ClassTable) -> Resolved {
    // A class some package defines, under `kind` when found and
    // `unresolved_kind` when no package read defines it.
    let defined = |key, kind, unresolved_kind, class_ref| match classes.get(&key) {
        Some(found) => Resolved {
            class: found.path.clone(),
            class_kind: kind,
            native_class: native_ancestor(&found.super_ref, script, classes),
            class_ref,
        },
        None => Resolved::unresolved(unresolved_kind, class_ref),
    };
    match class {
        ClassRef::Script(index) => {
            let class_ref = format!("script:{index:#018x}");
            match script.path_of(*index) {
                Some(path) => Resolved {
                    class: path.clone(),
                    class_kind: "script_import",
                    native_class: path,
                    class_ref,
                },
                None => Resolved::unresolved("script_import_unresolved", class_ref),
            }
        }
        ClassRef::Package(id, name, hash) => defined(
            (*id, *hash),
            "package_import",
            "package_import_unresolved",
            format!("package:{name}#{hash:#018x}"),
        ),
        ClassRef::Local(id, hash, name) => defined(
            (*id, *hash),
            "export",
            "export_unresolved",
            format!("export:{name}"),
        ),
        ClassRef::Null => Resolved::unresolved("null", "null".to_owned()),
        ClassRef::Bad(why) => Resolved::unresolved("bad_index", why.clone()),
    }
}

/// Walk a Blueprint class's super chain to the first native class. `?` when a
/// link cannot be followed; the walk is bounded so a cycle ends as `?` too.
fn native_ancestor(start: &ClassRef, script: &ScriptObjects, classes: &ClassTable) -> String {
    let mut at = start.clone();
    for _ in 0..64 {
        match at {
            ClassRef::Script(index) => {
                return script.path_of(index).unwrap_or_else(|| "?".to_owned());
            }
            ClassRef::Package(id, _, hash) | ClassRef::Local(id, hash, _) => {
                match classes.get(&(id, hash)) {
                    Some(found) => at = found.super_ref.clone(),
                    None => return "?".to_owned(),
                }
            }
            ClassRef::Null | ClassRef::Bad(_) => return "?".to_owned(),
        }
    }
    "?".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::parse_script_objects;
    use crate::script::tests::{build_script_objects, script_index};
    use crate::zen::tests::{PackageSpec, build_package};

    fn script() -> ScriptObjects {
        parse_script_objects(&build_script_objects(
            &[
                "/Script/Engine",
                "BlueprintGeneratedClass",
                "/Script/ShooterGame",
                "EquippableStateMachineComponent",
                "AresCharacter",
            ],
            &[
                (0, 0, "/Script/Engine", None),
                (
                    1,
                    0,
                    "/Script/Engine.BlueprintGeneratedClass",
                    Some("/Script/Engine"),
                ),
                (2, 0, "/Script/ShooterGame", None),
                (
                    3,
                    0,
                    "/Script/ShooterGame.EquippableStateMachineComponent",
                    Some("/Script/ShooterGame"),
                ),
                (
                    4,
                    0,
                    "/Script/ShooterGame.AresCharacter",
                    Some("/Script/ShooterGame"),
                ),
            ],
        ))
        .unwrap()
    }

    /// A character Blueprint: its class (export 0), its CDO (export 1), one
    /// SCS component whose class is native (export 2), one whose class is a
    /// Blueprint in another package (export 3), a C++ default subobject on the
    /// CDO (export 4), and an unrelated export (5).
    fn character() -> Vec<u8> {
        let bpgc = script_index("/Script/Engine.BlueprintGeneratedClass");
        let esm = script_index("/Script/ShooterGame.EquippableStateMachineComponent");
        let character = script_index("/Script/ShooterGame.AresCharacter");
        // Imported package 0, imported hash 0.
        let pkg_import = KIND_PACKAGE_IMPORT << 62;
        build_package(&PackageSpec {
            names: vec![
                "/Game/Characters/BP_Agent",
                "BP_Agent_C",
                "Default__BP_Agent_C",
                "Resume_StateMachine_GEN_VARIABLE",
                "Cooldown_GEN_VARIABLE",
                "InventoryComponent",
                "SomeMaterial",
            ],
            package_name: 0,
            exports: vec![
                (1, 0, u64::MAX, bpgc, character, 0x1111),
                (2, 0, u64::MAX, 0, u64::MAX, 0x2222),
                (3, 0, 0, esm, u64::MAX, 0),
                (4, 0, 0, pkg_import, u64::MAX, 0),
                (5, 0, 1, esm, u64::MAX, 0),
                (6, 0, u64::MAX, esm, u64::MAX, 0),
            ],
            imported_hashes: vec![0x3333],
            imported_packages: vec![("/Game/Abilities/Comp_Cooldown", 0)],
        })
    }

    /// The Blueprint class the second component points at, defined in its own
    /// package with a native parent.
    fn cooldown() -> Vec<u8> {
        let bpgc = script_index("/Script/Engine.BlueprintGeneratedClass");
        let esm = script_index("/Script/ShooterGame.EquippableStateMachineComponent");
        build_package(&PackageSpec {
            names: vec!["/Game/Abilities/Comp_Cooldown", "Comp_Cooldown_C"],
            package_name: 0,
            exports: vec![(1, 0, u64::MAX, bpgc, esm, 0x3333)],
            imported_hashes: vec![],
            imported_packages: vec![],
        })
    }

    #[test]
    fn components_are_found_by_both_shapes_and_resolved() {
        let script = script();
        let agent = parse_package_header(&character()).unwrap();
        let agent_id = hash_package_name(&agent.name);
        let (cands, agent_classes) = scan_header(&agent, agent_id, &script);
        let cd = parse_package_header(&cooldown()).unwrap();
        let cd_id = hash_package_name(&cd.name);
        let (none, cd_classes) = scan_header(&cd, cd_id, &script);
        assert!(none.is_empty());

        let mut table = ClassTable::new();
        for (id, list) in [(agent_id, agent_classes), (cd_id, cd_classes)] {
            for (hash, class) in list {
                table.insert((id, hash), class);
            }
        }
        let got: Vec<(&str, String, Resolved)> = cands
            .iter()
            .map(|c| {
                (
                    c.kind,
                    c.instance.clone(),
                    resolve(&c.class, &script, &table),
                )
            })
            .collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].0, "gen_variable");
        assert_eq!(got[0].1, "Resume_StateMachine");
        assert_eq!(
            got[0].2.class,
            "/Script/ShooterGame.EquippableStateMachineComponent"
        );
        assert_eq!(got[0].2.class_kind, "script_import");

        assert_eq!(got[1].1, "Cooldown");
        assert_eq!(
            got[1].2.class,
            "/Game/Abilities/Comp_Cooldown.Comp_Cooldown_C"
        );
        assert_eq!(got[1].2.class_kind, "package_import");
        assert_eq!(
            got[1].2.native_class,
            "/Script/ShooterGame.EquippableStateMachineComponent"
        );

        assert_eq!(got[2].0, "cdo_subobject");
        assert_eq!(got[2].1, "InventoryComponent");
        assert_eq!(cands[2].outer, "Default__BP_Agent_C");
    }

    /// A package import whose target package was never read stays `?` -- the
    /// row is kept, with the reference it could not follow.
    #[test]
    fn an_unreadable_import_is_a_visible_absence() {
        let script = script();
        let agent = parse_package_header(&character()).unwrap();
        let id = hash_package_name(&agent.name);
        let (cands, _) = scan_header(&agent, id, &script);
        let r = resolve(&cands[1].class, &script, &ClassTable::new());
        assert_eq!(r.class, "?");
        assert_eq!(r.class_kind, "package_import_unresolved");
        assert!(
            r.class_ref.contains("/Game/Abilities/Comp_Cooldown"),
            "{r:?}"
        );
    }

    #[test]
    fn a_numbered_gen_variable_keeps_its_number_and_is_flagged() {
        let script = script();
        let esm = script_index("/Script/ShooterGame.EquippableStateMachineComponent");
        let bytes = build_package(&PackageSpec {
            names: vec!["/Game/P", "X_GEN_VARIABLE"],
            package_name: 0,
            exports: vec![(1, 3, u64::MAX, esm, u64::MAX, 0)],
            imported_hashes: vec![],
            imported_packages: vec![],
        });
        let pkg = parse_package_header(&bytes).unwrap();
        let (cands, _) = scan_header(&pkg, 1, &script);
        assert_eq!(cands.len(), 1);
        assert!(cands[0].numbered);
        assert_eq!(cands[0].instance, "X_2");
        assert_eq!(cands[0].export, "X_GEN_VARIABLE_2");
    }

    /// A package import packs two indices: the imported package in the high
    /// 32 bits of the 62-bit value and the public export hash in the low 32.
    #[test]
    fn a_package_import_splits_into_package_and_hash_indices() {
        let bytes = build_package(&PackageSpec {
            names: vec!["/Game/P"],
            package_name: 0,
            exports: vec![],
            imported_hashes: vec![0xAAAA, 0xBBBB],
            imported_packages: vec![("/Game/First", 0), ("/Game/Second", 0)],
        });
        let pkg = parse_package_header(&bytes).unwrap();
        let index = (KIND_PACKAGE_IMPORT << 62) | (1u64 << 32) | 1;
        assert_eq!(
            class_ref(&pkg, 1, index),
            ClassRef::Package(
                hash_package_name("/Game/Second"),
                "/Game/Second".to_owned(),
                0xBBBB
            )
        );
    }

    #[test]
    fn an_index_outside_the_package_tables_is_bad_not_guessed() {
        let bytes = build_package(&PackageSpec {
            names: vec!["/Game/P"],
            package_name: 0,
            exports: vec![],
            imported_hashes: vec![],
            imported_packages: vec![],
        });
        let pkg = parse_package_header(&bytes).unwrap();
        let r = class_ref(&pkg, 1, (KIND_PACKAGE_IMPORT << 62) | (5u64 << 32));
        assert!(matches!(r, ClassRef::Bad(_)));
        assert!(matches!(class_ref(&pkg, 1, 4), ClassRef::Bad(_)));
        assert_eq!(class_ref(&pkg, 1, u64::MAX), ClassRef::Null);
    }
}
