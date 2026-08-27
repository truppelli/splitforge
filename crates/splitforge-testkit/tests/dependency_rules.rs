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
    // The first *physical* adapter (ADR-0024), holding the same boundary as llrp above.
    // LLRP stays the first *networked* protocol and M3b keeps every support criterion; the
    // two crates are peers here because the port is what makes them peers.
    (
        "splitforge-thingmagic",
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
    // "Everything except a protocol adapter's internals." Listed exhaustively rather than
    // as a wildcard, so that adding a crate to the workspace forces a deliberate answer
    // here — which is how splitforge-thingmagic came to be absent from this list rather
    // than quietly included in it.
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
            "splitforge-thingmagic",
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

/// Every crate that speaks a specific reader's wire protocol.
///
/// There are two now (ADR-0024), which is the first real test of the claim below: until a
/// second adapter existed, "the engine must not depend on `llrp`" and "the engine must not
/// depend on *a protocol*" were the same sentence, and only one of them was being checked.
const PROTOCOL_ADAPTERS: &[&str] = &["splitforge-llrp", "splitforge-thingmagic"];

#[test]
fn the_engine_cannot_reach_a_reader_protocol() {
    // Stated separately from the table because it is the rule the architecture is built
    // around, and a failure here should say so rather than reading as one row of many.
    let manifests = manifests();

    for crate_name in ["splitforge-engine", "splitforge-results", "splitforge-api"] {
        let manifest = &manifests[crate_name];
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let dependencies = workspace_dependencies(manifest, section);
            for adapter in PROTOCOL_ADAPTERS {
                assert!(
                    !dependencies.contains(*adapter),
                    "{crate_name} reached for {adapter} in [{section}]. The engine must \
                     not know that any particular reader protocol exists — that is what \
                     makes a second protocol an adapter instead of a rewrite. See \
                     ADR-0004 and ADR-0012."
                );
            }
        }
    }
}

#[test]
fn only_the_composition_root_knows_a_protocol_adapter_exists() {
    // The generalization of the rule above. `splitforge-edge` wires concrete
    // implementations together and is allowed to name them; every other crate — including
    // the CLI, which is otherwise permitted almost everything — is not.
    //
    // Without this, the second adapter could be reached for from anywhere the first one
    // was carefully kept out of, and each import would look individually reasonable.
    let manifests = manifests();

    for (name, manifest) in &manifests {
        if name == "splitforge-edge" || PROTOCOL_ADAPTERS.contains(&name.as_str()) {
            continue;
        }
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let dependencies = workspace_dependencies(manifest, section);
            for adapter in PROTOCOL_ADAPTERS {
                assert!(
                    !dependencies.contains(*adapter),
                    "{name} depends on {adapter} in [{section}]. Only splitforge-edge, the \
                     composition root, may name a concrete reader protocol — see \
                     docs/architecture.md § 2."
                );
            }
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
