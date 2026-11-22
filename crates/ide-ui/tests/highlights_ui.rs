//! Syntax highlighting for the five grammars `gpui-component` ships with an
//! empty highlights query. `ide_ui::highlights::install` registers the workbench's
//! own queries (crates/ide-language/queries/<lang>/highlights.scm); these tests
//! parse real source and require highlight spans, so a query that fails to
//! compile (which silently falls back to plain text) fails the test.
#![recursion_limit = "512"]

use gpui_kit::base::input::Rope;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use ide_ui::highlights;

fn styled_text(language: &str, source: &str) -> (String, Vec<String>) {
    let mut highlighter = SyntaxHighlighter::new(language);
    let rope = Rope::from(source);
    highlighter.update(None, &rope, None);
    let theme = (*HighlightTheme::default_dark()).clone();
    let mut covered = String::new();
    let mut names = Vec::new();
    for (range, _style) in highlighter.styles(&(0..source.len()), &theme) {
        if let Some(text) = source.get(range.clone()) {
            covered.push_str(text);
            covered.push('\n');
            names.push(text.to_owned());
        }
    }
    (covered, names)
}

#[test]
fn csharp_highlights_keywords_types_and_strings() {
    highlights::install();
    let source = "public class Widget {\n    void Run() { var s = \"hi\"; }\n}\n";
    let (_, names) = styled_text("csharp", source);
    assert!(names.iter().any(|n| n == "public"), "modifier: {names:?}");
    assert!(names.iter().any(|n| n == "class"), "keyword: {names:?}");
    assert!(names.iter().any(|n| n == "Widget"), "type: {names:?}");
    assert!(names.iter().any(|n| n.contains("hi")), "string: {names:?}");
}

#[test]
fn swift_highlights_declarations_and_strings() {
    highlights::install();
    let source = "struct Point { let x: Int = 1 }\nfunc f() { let s = \"hi\" }\n";
    let (_, names) = styled_text("swift", source);
    assert!(names.iter().any(|n| n == "Point"), "type: {names:?}");
    assert!(names.iter().any(|n| n == "let"), "binding: {names:?}");
    assert!(names.iter().any(|n| n.contains("hi")), "string: {names:?}");
}

#[test]
fn proto_highlights_messages_and_keywords() {
    highlights::install();
    let source = "syntax = \"proto3\";\nmessage User {\n  string name = 1;\n}\n";
    let (_, names) = styled_text("proto", source);
    assert!(names.iter().any(|n| n == "message"), "keyword: {names:?}");
    assert!(names.iter().any(|n| n == "User"), "message type: {names:?}");
    assert!(names.iter().any(|n| n == "string"), "builtin type: {names:?}");
}

#[test]
fn cmake_highlights_commands_and_arguments() {
    highlights::install();
    let source = "cmake_minimum_required(VERSION 3.20)\nset(MY_VAR \"value\")\n";
    let (_, names) = styled_text("cmake", source);
    assert!(names.iter().any(|n| n == "cmake_minimum_required"), "command: {names:?}");
    assert!(names.iter().any(|n| n == "set"), "command: {names:?}");
}

#[test]
fn graphql_highlights_fields_and_keywords() {
    highlights::install();
    let source = "query Hero {\n  hero { name }\n}\n";
    let (_, names) = styled_text("graphql", source);
    assert!(names.iter().any(|n| n == "query"), "keyword: {names:?}");
}

#[test]
fn every_supplied_query_is_registered_with_a_grammar() {
    highlights::install();
    for language in ["csharp", "swift", "graphql", "proto", "cmake"] {
        let config = highlights::grammar_config(language)
            .unwrap_or_else(|| panic!("{language} is registered"));
        assert!(!config.highlights.is_empty(), "{language} has a highlights query");
        assert!(config.language.is_some(), "{language} keeps its grammar");
    }
}
