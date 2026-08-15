//! The dependency table in `docs/architecture.md`, enforced.
//!
//! The rule that earns its keep is **`engine` must never depend on `llrp`**. It is what
//! makes a second reader protocol a new adapter rather than a rewrite, and it is exactly
//! the kind of rule that a well-meaning pull request erodes at 11 p.m. — one convenient
//! import at a time, each individually defensible.
//!
//! Written as a test rather than a `cargo deny` rule so the failure message can name the
//! crate, the offending dependency, and the document that says why. A build failure that
//! says `bans.deny` teaches nobody anything.
//!
//! ## Scope
//!
//! Only workspace-internal dependencies. External crates are `cargo deny`'s job.
//! Dev-dependencies are checked separately and more loosely: a test may reach for a helper
//! that the shipped crate may not, and `splitforge-testkit` exists precisely to be reached
//! for. What must never differ is the *shipped* graph, because that is what runs at a race.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The table from `docs/architecture.md` § 2, as data.
///
/// Each entry lists the workspace crates a package **may** depend on. Anything outside the
/// list is a violation; anything inside it is permission, not obligation.
const ALLOWED: &[(&str, &[&str])] = &[
    ("splitforge-domain", &[]),
    ("splitforge-reader", &["splitforge-domain"]),
    (
        "splitforge-llrp",
        &["splitforge-domain", "splitforge-reader"],
    ),
    ("splitforge-storage", &["splitforge-domain"]),
    ("splitforge-engine", &["splitforge-domain"]),
    ("splitforge-results", &["splitforge-domain"]),
    (
        "splitforge-export",
        &["splitforge-domain", "splitforge-results"],
    ),
    (
        "splitforge-api",
        &[
            "splitforge-domain",
            "splitforge-engine",
            "splitforge-export",
            "splitforge-results",
        ],
    ),
    (
        "splitforge-sync",
        &["splitforge-domain", "splitforge-export"],
    ),
    (
        "splitforge-testkit",
        &[
            "splitforge-domain",
            "splitforge-reader",
            "splitforge-storage",
        ],
    ),
    (
        "splitforge-simulator",
        &["splitforge-domain", "splitforge-reader"],
    ),
    // "Everything except llrp internals." Listed exhaustively rather than as a wildcard,
    // so that adding a crate to the workspace forces a deliberate answer here.
    (
        "splitforge-cli",
        &[
            "splitforge-api",
            "splitforge-domain",
            "splitforge-engine",
            "splitforge-export",
            "splitforge-reader",
            "splitforge-results",
            "splitforge-simulator",
            "splitforge-storage",
            "splitforge-sync",
            "splitforge-testkit",
        ],
    ),
    // The composition root: the only crate that is allowed to know every concrete
    // implementation, including the protocol adapters.
    (
        "splitforge-edge",
        &[
            "splitforge-api",
            "splitforge-cli",
            "splitforge-domain",
            "splitforge-engine",
            "splitforge-export",
            "splitforge-llrp",
            "splitforge-reader",
            "splitforge-results",
            "splitforge-simulator",
            "splitforge-storage",
            "splitforge-sync",
            "splitforge-testkit",
        ],
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the testkit sits two directories below the workspace root")
        .to_path_buf()
}

/// Every workspace member's manifest, keyed by package name.
fn manifests() -> BTreeMap<String, toml::Table> {
    let root = workspace_root();
    let mut found = BTreeMap::new();

    for group in ["crates", "apps"] {
        let directory = root.join(group);
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()));

        for entry in entries {
            let path = entry.expect("directory entry").path().join("Cargo.toml");
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            let manifest: toml::Table = text
                .parse()
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("every package has a name")
                .to_owned();
            found.insert(name, manifest);
        }
    }

    assert!(
        found.len() >= ALLOWED.len(),
        "found {} workspace members but the rule table covers {}",
        found.len(),
        ALLOWED.len()
    );
    found
}

/// The workspace crates named in one dependency section of a manifest.
fn workspace_dependencies(manifest: &toml::Table, section: &str) -> BTreeSet<String> {
    manifest
        .get(section)
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .keys()
                .filter(|name| name.starts_with("splitforge-"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_rule_table_covers_every_workspace_member() {
    // Otherwise a new crate could be added and silently exempted from every rule below,
    // which is the failure mode this whole file exists to prevent.
    let manifests = manifests();
    let covered: BTreeSet<&str> = ALLOWED.iter().map(|(name, _)| *name).collect();

    for name in manifests.keys() {
        assert!(
            covered.contains(name.as_str()),
            "{name} is a workspace member with no entry in the dependency rule table; add \
             one here and in docs/architecture.md § 2"
        );
    }
}

#[test]
fn no_crate_depends_on_something_the_architecture_forbids() {
    let manifests = manifests();

    for (name, allowed) in ALLOWED {
        let Some(manifest) = manifests.get(*name) else {
            continue;
        };
        let allowed: BTreeSet<&str> = allowed.iter().copied().collect();

        for dependency in workspace_dependencies(manifest, "dependencies") {
            assert!(
                allowed.contains(dependency.as_str()),
                "{name} depends on {dependency}, which docs/architecture.md § 2 forbids. \
                 If the architecture should change, change the table there first — and the \
                 ADR that justifies it."
            );
        }
    }
}

#[test]
fn the_engine_cannot_reach_a_reader_protocol() {
    // Stated separately from the table because it is the rule the architecture is built
    // around, and a failure here should say so rather than reading as one row of many.
    let manifests = manifests();

    for crate_name in ["splitforge-engine", "splitforge-results", "splitforge-api"] {
        let manifest = &manifests[crate_name];
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            assert!(
                !workspace_dependencies(manifest, section).contains("splitforge-llrp"),
                "{crate_name} reached for splitforge-llrp in [{section}]. The engine must \
                 not know that any particular reader protocol exists — that is what makes \
                 a second protocol an adapter instead of a rewrite. See ADR-0004."
            );
        }
    }
}

#[test]
fn the_domain_stays_free_of_io() {
    // The domain's purity is what lets it be reasoned about and tested without a database,
    // a socket, or a clock. Checked by name because the point is to catch the *first*
    // convenient import, before it grows a justification.
    const FORBIDDEN: &[&str] = &[
        "axum", "hyper", "reqwest", "rusqlite", "sqlx", "tokio", "tonic",
    ];

    let manifests = manifests();
    let manifest = &manifests["splitforge-domain"];

    for section in ["dependencies", "build-dependencies"] {
        let Some(table) = manifest.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for dependency in table.keys() {
            assert!(
                !FORBIDDEN.contains(&dependency.as_str()),
                "splitforge-domain depends on {dependency}. The domain holds port traits, \
                 not I/O — see ADR-0001 and docs/architecture.md § 2."
            );
        }
    }
}
