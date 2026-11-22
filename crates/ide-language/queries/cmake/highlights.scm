; CMake highlighting (tree-sitter-cmake). Command names carry their arguments
; as `unquoted_argument`s, so the arguments are coloured as strings and the
; command itself as a call.

(line_comment) @comment
(bracket_comment) @comment
(quoted_argument) @string
(bracket_argument) @string
(unquoted_argument) @string

(normal_command (identifier) @function.call)
