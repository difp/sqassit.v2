//! Opt-in comparison of `search_norm` with the Python reference.
//!
//! Case folding is a per-codepoint operation, so the oracle emits one folded
//! string per Unicode chunk; comparing all chunks covers every scalar value
//! without shipping a million-element JSON array.
use serde_json::Value;

#[test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
fn same_search_normalization_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let output = std::process::Command::new(python)
        .arg("tests/search_norm_oracle.py")
        .output()
        .expect("run Python search_norm reference");
    assert!(
        output.status.success(),
        "Python search_norm reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let curated = expected["curated"].as_array().expect("curated vectors");
    assert!(!curated.is_empty());
    for vector in curated {
        let input = vector["input"].as_str().unwrap();
        let normalized = vector["normalized"].as_str().unwrap();
        assert_eq!(
            sqassit::storage::search_norm(input),
            normalized,
            "curated vector {input:?}"
        );
    }

    let chunks = expected["chunks"].as_array().expect("Unicode chunks");
    assert!(chunks.len() > 0x100);
    for chunk in chunks {
        let start = chunk["start"].as_u64().unwrap() as u32;
        let expected = chunk["normalized"].as_str().unwrap();
        let input: String = (start..start + 0x1000)
            .filter(|code| !(0xD800..=0xDFFF).contains(code))
            .filter_map(char::from_u32)
            .collect();
        assert_eq!(
            sqassit::storage::search_norm(&input),
            expected,
            "Unicode chunk starting at U+{start:04X}"
        );
    }

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    let null: String = db
        .query_row("SELECT search_norm(NULL)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(null, "");
    let folded: String = db
        .query_row("SELECT search_norm(?1)", ["ЁЖ Straße"], |row| row.get(0))
        .unwrap();
    assert_eq!(folded, sqassit::storage::search_norm("ЁЖ Straße"));
}
