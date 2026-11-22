; C# highlighting (tree-sitter-c-sharp).

(comment) @comment

; Literals
(string_literal) @string
(verbatim_string_literal) @string
(interpolated_string_expression) @string
(raw_string_literal) @string
(character_literal) @string
(integer_literal) @number
(real_literal) @number

; Modifiers and keywords
(modifier) @keyword.modifier
[
  "abstract" "as" "base" "break" "case" "catch" "checked" "class" "const"
  "continue" "default" "delegate" "do" "else" "enum" "event" "explicit"
  "extern" "finally" "fixed" "for" "foreach" "goto" "if" "implicit" "in"
  "interface" "internal" "is" "lock" "namespace" "new" "operator" "out"
  "override" "params" "private" "protected" "public" "readonly" "ref"
  "return" "sealed" "sizeof" "stackalloc" "static" "struct" "switch"
  "this" "throw" "try" "typeof" "unchecked" "unsafe" "using" "virtual"
  "volatile" "while" "async" "await" "yield" "var" "when" "with" "record"
  "init" "required" "partial" "where" "select" "from"
] @keyword

; Types and members
(class_declaration name: (identifier) @type)
(struct_declaration name: (identifier) @type)
(interface_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(record_declaration name: (identifier) @type)
(delegate_declaration name: (identifier) @type)
(namespace_declaration name: (_) @namespace)
(file_scoped_namespace_declaration name: (_) @namespace)

(method_declaration name: (identifier) @function.method)
(local_function_statement name: (identifier) @function)
(constructor_declaration name: (identifier) @function.method)
(property_declaration name: (identifier) @property)
(enum_member_declaration name: (identifier) @constant)
(field_declaration (variable_declaration (variable_declarator name: (identifier) @property)))
(variable_declarator name: (identifier) @variable)
(parameter name: (identifier) @variable.parameter)

(invocation_expression function: (identifier) @function.call)
(invocation_expression function: (member_access_expression name: (identifier) @function.call))
(member_access_expression name: (identifier) @property)

(predefined_type) @type.builtin
(attribute) @attribute
