[
  "as"
  "break"
  "case"
  "const"
  "continue"
  "default"
  "defer"
  "else"
  "enum"
  "export"
  "extern"
  "for"
  "if"
  "impl"
  "import"
  "interface"
  "noescape"
  "opaque"
  "return"
  "struct"
  "switch"
  "type"
  "unsafe"
  "while"
] @keyword

(if_config "#if" @keyword)
(elif_config "#elif" @keyword)
(else_config) @keyword
(config_item "#endif" @keyword)
(c_include_declaration "#c_include" @keyword)

(function_declaration "async" @keyword)
(closure_expression "async" @keyword)
(await_expression "await" @keyword)
(select_expression "biased" @keyword)
(select_expression "select" @keyword)
(fn_suffix "fn" @keyword)
(struct_declaration "resource" @keyword)
(generic_parameter "resource" @keyword)
(derive_declaration "derive" @keyword)
(derivable_impl_declaration "derivable" @keyword)

(primitive_type) @type.builtin
(never_type) @type.builtin
(void_type) @type.builtin
(bool_literal) @boolean
(null_literal) @constant.builtin
(integer_literal) @number
(float_literal) @number.float
(string_literal) @string
(char_literal) @string.special
(comment) @comment

(function_signature name: (identifier) @function)
(interface_signature name: (identifier) @function)
(receiver_selector name: (identifier) @function)
(impl_declaration name: (identifier) @function)
(impl_declaration name: (qualified_name (identifier) @function .))
(derive_declaration name: (identifier) @type)
(derive_declaration name: (qualified_name (identifier) @type .))
(generic_item_expression function: (identifier) @function)
(generic_item_expression function: (qualified_name (identifier) @function .))
(call_expression function: (expression (identifier) @function.call))
(call_expression function: (expression (qualified_name (identifier) @function.call .)))
(call_expression
  function: (expression
    (field_expression field: (identifier) @function.call)))
(call_expression
  function: (expression
    (receiver_selector_expression
      selector: (qualified_name (identifier) @function.call .))))
(call_expression function: (expression (qualified_name (identifier) @constant .))
  (#match? @constant "^[A-Z]"))
(expression (qualified_name (identifier) @constant .)
  (#match? @constant "^[A-Z]"))

(type_alias_declaration name: (identifier) @type.definition)
(struct_declaration name: (identifier) @type.definition)
(enum_declaration name: (identifier) @type.definition)
(interface_alias_declaration name: (identifier) @type.definition)
(opaque_struct_declaration name: (identifier) @type.definition)
(named_type name: (identifier) @type)
(named_type name: (qualified_name (identifier) @type .))
(generic_parameter name: (identifier) @type.parameter)
(constraint_binding name: (identifier) @type.parameter)
(constraint_binding (type_hole) @type.parameter)
(opaque_return_type (type_hole) @type.parameter)
(interface_term name: (identifier) @type)
(interface_term name: (qualified_name (identifier) @type .))
(constraint_term name: (identifier) @type)
(constraint_term name: (qualified_name (identifier) @type .))

(field_declaration name: (identifier) @property.definition)
(field_initializer name: (identifier) @property)
(field_expression field: (identifier) @property)
(arrow_expression field: (identifier) @property)
(variant_declaration name: (identifier) @constant)
(variant_pattern (identifier) @constant)
(variant_pattern (qualified_name (identifier) @constant .))

(parameter name: (binding_name (identifier) @variable.parameter))
(closure_parameter (binding_name (identifier) @variable.parameter))
(var_declaration_clause name: (binding_name (identifier) @variable))
(select_arm binding: (identifier) @variable)
(module_path (identifier) @namespace)

[
  "+"
  "-"
  "*"
  "/"
  "%"
  "!"
  "~"
  "&"
  "|"
  "^"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "<<"
  ">>"
  "&&"
  "||"
  "="
  "->"
  ":"
  "?"
  "?*"
  "*const"
  "?*const"
] @operator
