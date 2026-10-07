//! Increment 1 integration test: every program in c0m-tests/ must parse.
//! New test programs are picked up automatically.

use std::fs;
use std::path::PathBuf;

use c0m_frontend::parse_source;

#[test]
fn every_test_program_parses() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("c0m-tests");
    let mut checked = 0;

    for entry in fs::read_dir(&dir).expect("c0m-tests/ should exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("c0m") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        if let Err(e) = parse_source(&src, 0) {
            panic!(
                "{} failed to parse at {}:{}: {}",
                path.display(),
                e.span.line,
                e.span.col,
                e.message
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no .c0m programs found in {}", dir.display());
}
