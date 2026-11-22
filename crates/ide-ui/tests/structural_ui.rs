//! Search Structurally: the panel hands `ide-analysis` a pattern and groups the
//! matches it streams back. These tests exercise the backend through the same
//! query shape the panel builds, so a pattern/placeholder regression shows up
//! here rather than only in a manual scan.
#![recursion_limit = "512"]

use ide_analysis::structural::{StructuralQuery, search_structurally};
use ide_core::LanguageId;
use ide_language::Syntax;
use std::path::PathBuf;
use std::sync::Mutex;

/// Runs `query` over one in-memory file and returns `(match text, replacement)`.
fn search(pattern: &str, language: &str, text: &str, replacement: Option<&str>) -> Vec<(String, Option<String>)> {
    let syntax = Syntax::new();
    let language_id = LanguageId::new(language);
    let path = PathBuf::from(format!("/tmp/openidea-structural-test.{language}"));
    let files = vec![ide_analysis::SourceFile { path: path.clone(), rel_path: format!("test.{language}") }];
    let query = StructuralQuery {
        pattern: pattern.into(),
        language: language_id.clone(),
        replacement: replacement.map(str::to_owned),
        max_matches: 0,
    };
    let text = text.to_owned();
    let loader = move |asked: &std::path::Path| {
        (asked == path.as_path()).then(|| (language_id.clone(), text.clone()))
    };
    let found = Mutex::new(Vec::new());
    let cancel = ide_core::CancellationToken::new();
    let _ = search_structurally(&syntax, &files, &loader, &query, &cancel, &|file| {
        if let Ok(mut found) = found.lock() {
            found.extend(file.matches.into_iter().map(|matched| (matched.text, matched.replacement)));
        }
    });
    found.into_inner().unwrap_or_default()
}

#[test]
fn node_and_list_placeholders_match_a_syntax_shape() {
    // `$$$ARGS` takes any argument sequence, so both calls match; the
    // definition does not look like a call.
    let found = search("$F($$$ARGS)", "rust", "fn a() {}\nfn main() { a(); b(1); }\n", None);
    assert!(!found.is_empty(), "calls match the call shape: {found:?}");
    assert!(found.iter().any(|(text, _)| text.contains("a()")), "zero-arg call: {found:?}");
    assert!(found.iter().any(|(text, _)| text.contains("b(1)")), "one-arg call: {found:?}");
}

#[test]
fn replacement_templates_attach_to_matches() {
    let found = search(
        "println!($$$ARGS)",
        "rust",
        "fn main() { println!(\"hi\"); }\n",
        Some("log!($$$ARGS)"),
    );
    let replacements: Vec<&String> = found.iter().filter_map(|(_, replacement)| replacement.as_ref()).collect();
    assert!(!replacements.is_empty(), "matches carry their replacement: {found:?}");
    assert!(replacements.iter().any(|text| text.contains("log!")), "{replacements:?}");
}

#[test]
fn validation_rejects_a_language_without_a_grammar() {
    let syntax = Syntax::new();
    let query = StructuralQuery {
        pattern: "$X".into(),
        language: LanguageId::new("no-such-language"),
        replacement: None,
        max_matches: 0,
    };
    assert!(
        ide_analysis::validate_structural(&syntax, &query).is_err(),
        "a language without a bundled grammar is reported before the scan"
    );
}

#[test]
fn validation_accepts_a_pattern_the_grammar_can_parse() {
    // ast-grep tolerates incomplete code (it is a search tool, not a compiler),
    // so the panel must not gate on strict syntax.
    let syntax = Syntax::new();
    let query = StructuralQuery {
        pattern: "if ($COND) { $$$BODY }".into(),
        language: LanguageId::new("rust"),
        replacement: None,
        max_matches: 0,
    };
    assert!(ide_analysis::validate_structural(&syntax, &query).is_ok());
}

#[test]
fn a_language_without_a_grammar_yields_no_matches() {
    // The panel lets the user name any language; one without a bundled grammar
    // must produce no results instead of panicking.
    let found = search("$X", "no-such-language", "anything\n", None);
    assert!(found.is_empty(), "{found:?}");
}
