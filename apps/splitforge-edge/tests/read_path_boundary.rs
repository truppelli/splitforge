//! `splitforge-sync` stays off the read path (architecture § 5, ADR-0006).
//!
//! The dependency rules keep the crates the read path is built from, `splitforge-reader`,
//! `splitforge-storage` and the protocol adapters, from ever naming `splitforge-sync`
//! (ADR-0012). They cannot say the same of this crate: the composition root is allowed to
//! depend on it, because the shipper that sends to RaceDay Connect will be composed here. What
//! keeps it out of the read loop is this file.
//!
//! Deliberately read from the source, as `splitforge-api` checks that it binds no port
//! (ADR-0021). A call behind a flag or an `if` would pass every runtime test and still be the
//! thing forbidden: a read waiting on an integration.

use std::path::{Path, PathBuf};

/// Where in this crate `splitforge_sync` may be named: the shipper's own module, and nowhere
/// else. `main.rs` starts the shipper through that module rather than naming the crate itself,
/// so a reviewer reading the read loop never has to wonder whether something in it sends.
const SHIPPER: &[&str] = &["src/ship.rs", "src/ship/"];

/// The functions a read, and the evidence about the reader, pass through in `main.rs`.
///
/// If one is renamed or moved, this test fails and says which, rather than passing because it
/// no longer looks at anything.
const READ_PATH: &[&str] = &[
    "fn read_into_journal(",
    "fn store(",
    "fn record_connection(",
    "fn check_for_silence(",
];

/// What the read path may not mention: the crate, the shipper's module, or its table.
const BANNED_ON_THE_READ_PATH: &[&str] = &["splitforge_sync", "ship::", "outbox"];

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read a source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// The source's code, with `//` comments removed: they are where the rule gets explained, so
/// they are not evidence of it being broken.
fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

fn relative(path: &Path) -> String {
    path.strip_prefix(crate_dir())
        .expect("under the crate")
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn only_the_shipper_module_names_splitforge_sync() {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    assert!(!files.is_empty(), "no source found under src/");

    let mut offenders = Vec::new();
    for file in &files {
        let name = relative(file);
        if SHIPPER
            .iter()
            .any(|allowed| name == *allowed || name.starts_with(allowed))
        {
            continue;
        }
        let source = std::fs::read_to_string(file).expect("read the source");
        for (number, line) in source.lines().enumerate() {
            if code(line).contains("splitforge_sync") {
                offenders.push(format!("{name}:{}: {}", number + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "splitforge-sync may be named only in the shipper's module, {SHIPPER:?} (architecture \
         § 5), but found:\n  {}",
        offenders.join("\n  ")
    );
}

/// The body of the function whose signature starts at `signature`: from its opening brace to
/// the brace that closes it, counted on code with comments removed.
fn body<'a>(source: &'a str, signature: &str) -> Option<Vec<(usize, &'a str)>> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines.iter().position(|line| {
        line.starts_with(signature) || line.starts_with(&format!("async {signature}"))
    })?;

    let mut depth = 0_i64;
    let mut opened = false;
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(start) {
        let code = code(line);
        for character in code.chars() {
            match character {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        out.push((index + 1, *line));
        if opened && depth == 0 {
            return Some(out);
        }
    }
    None
}

#[test]
fn nothing_on_the_read_path_reaches_the_shipper() {
    let path = crate_dir().join("src/main.rs");
    let source = std::fs::read_to_string(&path).expect("read main.rs");

    let mut missing = Vec::new();
    let mut offenders = Vec::new();
    for signature in READ_PATH {
        let Some(lines) = body(&source, signature) else {
            missing.push(*signature);
            continue;
        };
        for (number, line) in lines {
            let code = code(line);
            if let Some(found) = BANNED_ON_THE_READ_PATH
                .iter()
                .find(|needle| code.contains(**needle))
            {
                offenders.push(format!("main.rs:{number}: {found} in `{}`", line.trim()));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these read-path functions were not found in main.rs, so this test would be checking \
         nothing. Update READ_PATH to where they went: {missing:?}"
    );
    assert!(
        offenders.is_empty(),
        "the read path may not wait on an integration (architecture § 5), but found:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_body_finder_stops_at_the_brace_that_closes_the_function() {
    // The check above is only as good as this. A brace in a comment is ignored, and nested
    // blocks are counted.
    let source = "fn before() {}\n\
                  fn target(x: u8) -> u8 {\n\
                  \x20   // a stray } in a comment\n\
                  \x20   if x > 0 { x } else { 0 }\n\
                  }\n\
                  fn after() { outbox(); }\n";
    let lines = body(source, "fn target(").expect("found");
    assert_eq!(lines.first().map(|(number, _)| *number), Some(2));
    assert_eq!(lines.last().map(|(number, _)| *number), Some(5));
    assert!(lines.iter().all(|(_, line)| !line.contains("outbox")));
    assert!(body(source, "fn absent(").is_none());
}
