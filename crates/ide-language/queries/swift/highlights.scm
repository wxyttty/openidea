; Swift highlighting (tree-sitter-swift).

(comment) @comment
(multiline_comment) @comment

(line_string_literal) @string
(multi_line_string_literal) @string
(line_str_text) @string
(raw_string_literal) @string
(boolean_literal) @constant.builtin
(integer_literal) @number
(real_literal) @number
(hex_literal) @number

; Every declaration — class, struct, enum and extension alike — is a
; `class_declaration` in this grammar; only its body's name differs.
(class_declaration name: (type_identifier) @type)
(class_declaration name: (user_type (type_identifier) @type))
(type_identifier) @type
(user_type (type_identifier) @type)

(function_declaration name: (simple_identifier) @function)
(init_declaration) @function.method
(deinit_declaration) @function.method
(property_declaration name: (pattern) @property)
(pattern (simple_identifier) @property)
(enum_entry name: (simple_identifier) @constant)
(parameter name: (simple_identifier) @variable.parameter)
(simple_identifier) @variable

(call_expression (simple_identifier) @function.call)
(navigation_suffix (simple_identifier) @property)
(value_binding_pattern) @keyword
(attribute) @attribute
