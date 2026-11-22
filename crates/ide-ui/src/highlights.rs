//! Highlights queries for the grammars `gpui-component` ships without one.
//!
//! `gpui-component` 0.10 registers a tree-sitter grammar per language, but five
//! of them — C#, Swift, GraphQL, Proto and CMake — carry an empty `highlights`
//! query, so the editor would draw them unhighlighted. The queries live in
//! `crates/ide-language/queries/<lang>/highlights.scm` (the same files
//! `ide-language` uses for its own structural features); this module registers
//! them with the component's highlighter registry at start-up.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

/// One grammar whose highlights query the workbench supplies itself.
struct Grammar {
    /// The name the editor passes to `set_highlighter` (`language_for_id`).
    name: &'static str,
    /// The grammar name `gpui-component` registered for that language.
    grammar: &'static str,
    highlights: &'static str,
}

/// The five grammars with an empty query upstream.
const GRAMMARS: [Grammar; 5] = [
    Grammar {
        name: "csharp",
        grammar: "csharp",
        highlights: include_str!("../../ide-language/queries/csharp/highlights.scm"),
    },
    Grammar {
        name: "swift",
        grammar: "swift",
        highlights: include_str!("../../ide-language/queries/swift/highlights.scm"),
    },
    Grammar {
        name: "graphql",
        grammar: "graphql",
        highlights: include_str!("../../ide-language/queries/graphql/highlights.scm"),
    },
    Grammar {
        name: "proto",
        grammar: "proto",
        highlights: include_str!("../../ide-language/queries/proto/highlights.scm"),
    },
    Grammar {
        name: "cmake",
        grammar: "cmake",
        highlights: include_str!("../../ide-language/queries/cmake/highlights.scm"),
    },
];

/// Registers the missing highlights queries. Called once at start-up, before the
/// first editor opens; later registrations replace earlier ones.
pub fn install() {
    let registry = LanguageRegistry::singleton();
    for grammar in GRAMMARS {
        // Start from the shipped config so the grammar, injections and locals
        // stay exactly what the component registered; only the highlights query
        // changes. A grammar the component does not ship is skipped rather than
        // stubbed: a text-only entry would be worse than no highlight.
        let Some(mut config) = registry.language(grammar.grammar) else {
            tracing::warn!(grammar = grammar.grammar, "no tree-sitter grammar to attach a query to");
            continue;
        };
        config.name = grammar.name.into();
        config.highlights = grammar.highlights.into();
        registry.register(grammar.name, &config);
        tracing::debug!(language = grammar.name, "registered highlights query");
    }
}

/// The grammar config the registry holds for `name`, for tests.
pub fn grammar_config(name: &str) -> Option<LanguageConfig> {
    LanguageRegistry::singleton().language(name)
}
