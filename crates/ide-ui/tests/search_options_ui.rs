//! Find in Path: the search bar's options (regex, case, whole word, context,
//! masks) and the replacement preview / per-match replace helpers.

use ide_search::SearchContext;
use ide_ui::search::{SearchOptions, context_label, next_context, replace_in_line};

#[test]
fn options_build_the_backends_query() {
    let options = SearchOptions {
        is_regex: true,
        case_sensitive: true,
        whole_word: true,
        context: SearchContext::InComments,
        include: "*.rs  src/**".into(),
        exclude: "*.test.rs".into(),
    };
    let query = options.query("fn\\s+\\w+".into());
    assert!(query.is_regex, "regex toggle reaches the query");
    assert!(query.case_sensitive);
    assert!(query.whole_word);
    assert_eq!(query.context, SearchContext::InComments);
    assert_eq!(query.include, vec!["*.rs".to_owned(), "src/**".to_owned()]);
    assert_eq!(query.exclude, vec!["*.test.rs".to_owned()]);
}

#[test]
fn a_bang_prefix_is_accepted_in_either_mask_field() {
    // IDEA lets `!pattern` be typed in either field; the backend takes the
    // masks separately, so the `!` is stripped and the rest kept.
    let options = SearchOptions { include: "!*.min.js src/**".into(), ..Default::default() };
    let query = options.query("x".into());
    assert_eq!(query.include, vec!["*.min.js".to_owned(), "src/**".to_owned()]);
}

#[test]
fn blank_masks_mean_all_files() {
    let query = SearchOptions::default().query("x".into());
    assert!(query.include.is_empty());
    assert!(query.exclude.is_empty());
    assert_eq!(query.context, SearchContext::Any);
    assert!(!query.is_regex && !query.case_sensitive && !query.whole_word);
}

#[test]
fn the_context_filter_cycles_through_every_state() {
    let mut context = SearchContext::Any;
    let mut seen = vec![context];
    for _ in 0..5 {
        context = next_context(context);
        seen.push(context);
    }
    assert_eq!(next_context(context), SearchContext::Any, "the cycle wraps");
    assert_eq!(seen.len(), 6, "six distinct states");
    assert_eq!(context_label(SearchContext::ExceptCommentsAndStringLiterals), "Plain code");
}

#[test]
fn a_replacement_preview_replaces_only_the_matched_ranges() {
    // "score: 100, max: 100" with the first `100` matched.
    let text = "score: 100, max: 100";
    let ranges = vec![(7, 3)];
    assert_eq!(replace_in_line(text, &ranges, "42"), "score: 42, max: 100");
    // Several ranges in one line.
    let all = vec![(7, 3), (17, 3)];
    assert_eq!(replace_in_line(text, &all, "42"), "score: 42, max: 42");
    assert_eq!(replace_in_line(text, &[], "42"), text, "no ranges leaves the line alone");
}

#[test]
fn a_preview_handles_an_empty_match_set_and_out_of_range_offsets() {
    let text = "abc";
    assert_eq!(replace_in_line(text, &[(5, 2)], "x"), text, "offsets past the line are ignored");
}
