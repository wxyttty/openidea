; Protocol Buffers highlighting (tree-sitter-proto).

(comment) @comment
(string) @string
(int_lit) @number
(float_lit) @number
(bool) @constant.builtin

[
  "syntax" "package" "import" "option" "message" "enum" "service" "rpc"
  "returns" "repeated" "optional" "required" "reserved" "extensions" "extend"
  "oneof" "map" "stream" "public" "weak" "to" "max"
] @keyword

(message_name (identifier) @type)
(enum_name (identifier) @type)
(service_name (identifier) @type)
(rpc_name (identifier) @function)
(type) @type.builtin
(import (string) @string.special)
