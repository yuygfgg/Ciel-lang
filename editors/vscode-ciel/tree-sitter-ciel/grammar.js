const PREC = {
    closure: 15,
    logical_or: 1,
    logical_and: 2,
    bitwise_or: 3,
    bitwise_xor: 4,
    bitwise_and: 5,
    equality: 6,
    relational: 7,
    shift: 8,
    additive: 9,
    multiplicative: 10,
    cast: 11,
    unary: 12,
    call: 13,
    member: 14,
};

module.exports = grammar({
    name: 'ciel',

    word: $ => $.regular_identifier,

    extras: $ => [
        /\s/,
        $.comment,
    ],

    conflicts: $ => [
        [$.type],
        [$.named_type],
        [$.named_type, $.expression],
        [$.struct_literal, $.block],
        [$.array_type, $.literal],
        [$.slice_type, $.array_literal],
        [$.binding_name, $.named_type],
        [$.call_expression, $.binary_expression],
        [$.binary_expression, $.unary_expression, $.call_expression],
        [$.binary_expression, $.call_expression, $.await_expression],
        [$.function_declaration, $.contextual_identifier],
        [$.select_expression, $.contextual_identifier],
    ],

    rules: {
        source_file: $ => repeat(choice(
            $.config_item,
            $.top_level_item,
        )),

        comment: _ => token(choice(
            seq('//', /[^\n]*/),
            seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
        )),

        top_level_item: $ => seq(
            optional('export'),
            choice(
                $.import_declaration,
                $.c_include_declaration,
                $.type_alias_declaration,
                $.struct_declaration,
                $.enum_declaration,
                $.interface_declaration,
                $.interface_alias_declaration,
                $.impl_declaration,
                $.extern_block,
                $.function_declaration,
            ),
        ),

        config_item: $ => seq(
            $.if_config,
            repeat($.config_line),
            repeat(seq($.elif_config, repeat($.config_line))),
            optional(seq($.else_config, repeat($.config_line))),
            '#endif',
        ),

        if_config: $ => seq(
            '#if',
            $.config_expression,
        ),

        elif_config: $ => seq(
            '#elif',
            $.config_expression,
        ),

        else_config: _ => '#else',

        config_line: _ => token(prec(-1, choice(
            /#c_include[^\n]*/,
            /[^#\n][^\n]*/,
        ))),

        config_expression: $ => choice(
            $.config_call,
            $.config_unary_expression,
            $.config_binary_expression,
            $.config_parenthesized_expression,
        ),

        config_call: $ => seq(
            field('function', choice('has_feature', 'is_target_os', 'is_target_arch')),
            '(',
            $.string_literal,
            ')',
        ),

        config_unary_expression: $ => prec(PREC.unary, seq(
            '!',
            $.config_expression,
        )),

        config_binary_expression: $ => choice(
            prec.left(PREC.logical_and, seq($.config_expression, '&&', $.config_expression)),
            prec.left(PREC.logical_or, seq($.config_expression, '||', $.config_expression)),
        ),

        config_parenthesized_expression: $ => seq(
            '(',
            $.config_expression,
            ')',
        ),

        import_declaration: $ => seq(
            'import',
            field('path', $.module_path),
            optional(seq('as', field('alias', $.identifier))),
            ';',
        ),

        module_path: $ => seq(
            optional(choice('/', seq('.', '/'), repeat1(seq('..', '/')))),
            $.identifier,
            repeat(seq('/', $.identifier)),
        ),

        c_include_declaration: $ => seq(
            '#c_include',
            $.string_literal,
            optional(';'),
        ),

        type_alias_declaration: $ => seq(
            optional($.abi_spec),
            'type',
            field('name', $.identifier),
            optional($.generic_parameter_list),
            '=',
            field('target', choice($.type, $.string_literal)),
            ';',
        ),

        struct_declaration: $ => seq(
            optional('unsafe'),
            'struct',
            field('name', $.identifier),
            optional($.generic_parameter_list),
            field('body', $.struct_body),
        ),

        struct_body: $ => seq(
            '{',
            repeat($.field_declaration),
            '}',
        ),

        field_declaration: $ => seq(
            field('type', $.type),
            field('name', $.identifier),
            ';',
        ),

        enum_declaration: $ => seq(
            'enum',
            field('name', $.identifier),
            optional($.generic_parameter_list),
            field('body', $.enum_body),
        ),

        enum_body: $ => seq(
            '{',
            optional(seq(
                $.variant_declaration,
                repeat(seq(',', $.variant_declaration)),
                optional(','),
            )),
            '}',
        ),

        variant_declaration: $ => seq(
            field('name', $.identifier),
            optional(seq(
                '(',
                optional($.type_list),
                ')',
            )),
        ),

        interface_declaration: $ => seq(
            optional('unsafe'),
            'interface',
            $.generic_parameter_list,
            $.interface_signature,
            ';',
        ),

        interface_signature: $ => seq(
            field('return_type', $.type),
            field('name', $.identifier),
            field('parameters', $.parameter_list),
        ),

        interface_alias_declaration: $ => seq(
            'interface',
            field('name', $.identifier),
            optional($.generic_parameter_list),
            '=',
            field('value', $.interface_expr),
            ';',
        ),

        interface_expr: $ => prec.left(seq(
            $.interface_term,
            repeat(seq(choice('+', '-'), $.interface_term)),
        )),

        interface_term: $ => seq(
            optional('!'),
            field('name', choice($.identifier, $.qualified_name)),
            optional($.type_argument_list),
        ),

        impl_declaration: $ => seq(
            optional('unsafe'),
            'impl',
            optional($.generic_parameter_list),
            field('name', choice($.identifier, $.qualified_name)),
            optional($.type_argument_list),
            field('parameters', $.parameter_list),
            field('body', $.block),
        ),

        extern_block: $ => seq(
            optional('unsafe'),
            $.abi_spec,
            '{',
            repeat($.extern_item),
            '}',
        ),

        extern_item: $ => choice(
            $.opaque_struct_declaration,
            seq(optional('noescape'), $.function_signature, ';'),
            $.type_alias_declaration,
        ),

        opaque_struct_declaration: $ => seq(
            'opaque',
            'struct',
            field('name', $.identifier),
            ';',
        ),

        function_declaration: $ => seq(
            optional('unsafe'),
            optional($.abi_spec),
            optional('async'),
            $.function_signature,
            choice($.block, ';'),
        ),

        function_signature: $ => seq(
            field('return_type', $.type),
            field('name', $.identifier),
            optional($.generic_parameter_list),
            field('parameters', $.parameter_list),
        ),

        abi_spec: $ => seq(
            'extern',
            $.string_literal,
        ),

        generic_parameter_list: $ => seq(
            '<',
            commaSep1($.generic_parameter),
            '>',
        ),

        generic_parameter: $ => seq(
            field('name', $.identifier),
            optional(seq(':', $.constraint_expr)),
        ),

        constraint_expr: $ => prec.left(seq(
            $.constraint_term,
            repeat(seq(choice('+', '-'), $.constraint_term)),
        )),

        constraint_term: $ => seq(
            optional('!'),
            field('name', choice($.identifier, $.qualified_name)),
            optional($.type_argument_list),
        ),

        parameter_list: $ => seq(
            '(',
            optional(commaSep1($.parameter)),
            ')',
        ),

        parameter: $ => seq(
            field('type', $.type),
            field('name', $.binding_name),
        ),

        binding_name: $ => seq(
            optional('@'),
            $.identifier,
        ),

        type: $ => prec.right(seq(
            optional('unsafe'),
            optional($.abi_spec),
            $.prefix_type,
            repeat($.callable_suffix),
        )),

        prefix_type: $ => seq(
            repeat($.pointer_constructor),
            $.primary_type,
        ),

        pointer_constructor: $ => choice(
            token('?*const'),
            token('*const'),
            token('?*'),
            $._star,
        ),

        primary_type: $ => choice(
            $.primitive_type,
            $.named_type,
            $.type_hole,
            $.never_type,
            $.void_type,
            $.array_type,
            $.slice_type,
            seq('(', $.type, ')'),
        ),

        primitive_type: _ => token(prec(1, choice(
            'bool',
            'char',
            'i8',
            'i16',
            'i32',
            'i64',
            'u8',
            'u16',
            'u32',
            'u64',
            'usize',
            'f32',
            'f64',
        ))),

        never_type: _ => token(prec(1, 'never')),

        void_type: _ => token(prec(1, 'void')),

        named_type: $ => seq(
            field('name', choice($.identifier, $.qualified_name)),
            optional($.type_argument_list),
        ),

        type_hole: _ => '_',

        type_argument_list: $ => seq(
            '<',
            $.type_list,
            '>',
        ),

        type_list: $ => commaSep1($.type),

        array_type: $ => seq(
            '[',
            $.integer_literal,
            ']',
            $.type,
        ),

        slice_type: $ => seq(
            '[',
            ']',
            optional('const'),
            $.type,
        ),

        callable_suffix: $ => choice(
            $.fn_suffix,
            $.closure_suffix,
        ),

        fn_suffix: $ => seq(
            'fn',
            '(',
            optional($.type_list),
            ')',
        ),

        closure_suffix: $ => seq(
            '|',
            '(',
            optional($.type_list),
            ')',
            optional(seq(':', $.constraint_expr)),
            '|',
        ),

        block: $ => seq(
            '{',
            repeat($.statement),
            '}',
        ),

        statement: $ => choice(
            $.block,
            $.pointer_var_declaration_statement,
            $.var_declaration_statement,
            $.assignment_statement,
            $.if_statement,
            $.while_statement,
            $.for_statement,
            $.switch_statement,
            $.defer_statement,
            $.return_statement,
            $.break_statement,
            $.continue_statement,
            $.expression_statement,
        ),

        var_declaration_statement: $ => prec.dynamic(2, seq(
            $.var_declaration_clause,
            ';',
        )),

        pointer_var_declaration_statement: $ => prec(3, prec.dynamic(3, seq(
            $.pointer_declaration_head,
            optional(seq('=', field('value', $.expression))),
            ';',
        ))),

        var_declaration_clause: $ => prec.dynamic(2, seq(
            field('type', $.type),
            field('name', $.binding_name),
            optional(seq('=', field('value', $.expression))),
        )),

        // TODO: Replace this tokenized pointer declaration head with structured
        // type/name nodes once the `*Type name` vs `*target = value` ambiguity is
        // handled without relying on error recovery. This currently keeps parsing
        // broad source inputs, but it hides the pointer type and binding internals.
        pointer_declaration_head: _ => token(/(\?\*const|\*const|\?\*|\*)+[ \t]*(?:[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*(<[^>\n]*>)?|_)[ \t]+@?[A-Za-z_][A-Za-z0-9_]*/),

        assignment_statement: $ => choice(
            $.deref_assignment_statement,
            prec.dynamic(-1, seq(
                field('left', $.lvalue),
                '=',
                field('right', $.expression),
                ';',
            )),
        ),

        deref_assignment_statement: $ => seq(
            field('left', $.deref_assignment_target),
            field('right', $.expression),
            ';',
        ),

        expression_statement: $ => seq(
            $.expression,
            ';',
        ),

        if_statement: $ => prec.right(seq(
            'if',
            '(',
            field('condition', $.expression),
            ')',
            field('consequence', $.block),
            optional(seq(
                'else',
                field('alternative', choice($.block, $.if_statement)),
            )),
        )),

        while_statement: $ => seq(
            'while',
            '(',
            field('condition', $.expression),
            ')',
            field('body', $.block),
        ),

        for_statement: $ => seq(
            'for',
            '(',
            optional(field('initializer', choice(
                $.var_declaration_clause,
                $.assignment_clause,
                $.expression,
            ))),
            ';',
            optional(field('condition', $.expression)),
            ';',
            optional(field('step', choice(
                $.assignment_clause,
                $.expression,
            ))),
            ')',
            field('body', $.block),
        ),

        assignment_clause: $ => choice(
            seq(
                field('left', $.deref_assignment_target),
                field('right', $.expression),
            ),
            seq(
                field('left', $.lvalue),
                '=',
                field('right', $.expression),
            ),
        ),

        switch_statement: $ => seq(
            'switch',
            '(',
            field('value', $.expression),
            ')',
            '{',
            repeat($.case_clause),
            optional($.default_clause),
            '}',
        ),

        case_clause: $ => seq(
            'case',
            field('pattern', $.pattern),
            ':',
            repeat($.statement),
        ),

        default_clause: $ => seq(
            'default',
            ':',
            repeat($.statement),
        ),

        defer_statement: $ => seq(
            'defer',
            field('call', $.call_expression),
            ';',
        ),

        return_statement: $ => seq(
            'return',
            optional($.expression),
            ';',
        ),

        break_statement: _ => seq('break', ';'),

        continue_statement: _ => seq('continue', ';'),

        lvalue: $ => choice(
            $.identifier,
            $.field_expression,
            $.arrow_expression,
            $.index_expression,
        ),

        // TODO: Model dereference assignment as structured syntax instead of
        // consuming the assignment operator in the target token. This deliberately
        // avoids parsing `*Type name` as `*target =`, but it does not yet cover
        // every assignable dereference shape such as `*ptr->field` or `*(ptr)`.
        deref_assignment_target: _ => token(/\*[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*[ \t]*=/),

        expression: $ => choice(
            $.literal,
            $.identifier,
            $.qualified_name,
            $.parenthesized_expression,
            $.struct_literal,
            $.array_literal,
            $.closure_expression,
            $.await_expression,
            $.select_expression,
            $.unsafe_block_expression,
            $.unary_expression,
            $.binary_expression,
            $.cast_expression,
            $.call_expression,
            $.field_expression,
            $.arrow_expression,
            $.index_expression,
            $.slice_expression,
            $.try_expression,
        ),

        parenthesized_expression: $ => seq(
            '(',
            $.expression,
            ')',
        ),

        binary_expression: $ => choice(
            ...[
                ['||', PREC.logical_or],
                ['&&', PREC.logical_and],
                ['|', PREC.bitwise_or],
                ['^', PREC.bitwise_xor],
                ['&', PREC.bitwise_and],
                ['==', PREC.equality],
                ['!=', PREC.equality],
                ['<', PREC.relational],
                ['<=', PREC.relational],
                ['>', PREC.relational],
                ['>=', PREC.relational],
                ['<<', PREC.shift],
                ['>>', PREC.shift],
                ['+', PREC.additive],
                ['-', PREC.additive],
                [$._star, PREC.multiplicative],
                ['/', PREC.multiplicative],
                ['%', PREC.multiplicative],
            ].map(([operator, precedence]) => prec.left(precedence, seq(
                field('left', $.expression),
                field('operator', operator),
                field('right', $.expression),
            ))),
        ),

        cast_expression: $ => prec.left(PREC.cast, seq(
            field('value', $.expression),
            'as',
            field('target', $.type),
        )),

        unary_expression: $ => prec(PREC.unary, seq(
            field('operator', choice('!', '~', '-', '&', $._star)),
            field('operand', $.expression),
        )),

        call_expression: $ => prec(PREC.call, seq(
            field('function', $.expression),
            optional($.type_argument_list),
            field('arguments', $.argument_list),
        )),

        argument_list: $ => seq(
            '(',
            optional(commaSep1($.expression)),
            ')',
        ),

        field_expression: $ => prec(PREC.member, seq(
            field('object', $.expression),
            '.',
            field('field', $.identifier),
        )),

        arrow_expression: $ => prec(PREC.member, seq(
            field('object', $.expression),
            '->',
            field('field', $.identifier),
        )),

        index_expression: $ => prec(PREC.member, seq(
            field('object', $.expression),
            '[',
            field('index', $.expression),
            ']',
        )),

        slice_expression: $ => prec(PREC.member, seq(
            field('object', $.expression),
            '[',
            optional(field('start', $.expression)),
            '..',
            optional(field('end', $.expression)),
            ']',
        )),

        try_expression: $ => prec(PREC.call, seq(
            field('value', $.expression),
            '?',
        )),

        await_expression: $ => prec(PREC.unary, seq(
            'await',
            field('operand', $.expression),
        )),

        select_expression: $ => seq(
            optional('biased'),
            'select',
            '{',
            repeat1($.select_arm),
            '}',
        ),

        select_arm: $ => seq(
            'case',
            field('binding', $.identifier),
            '=',
            field('future', $.expression),
            ':',
            field('body', $.expression),
            optional(';'),
        ),

        unsafe_block_expression: $ => seq(
            'unsafe',
            '{',
            repeat($.statement),
            optional($.expression),
            '}',
        ),

        qualified_name: $ => seq(
            $.identifier,
            repeat1(seq('::', $.identifier)),
        ),

        literal: $ => choice(
            $.integer_literal,
            $.float_literal,
            $.char_literal,
            $.string_literal,
            $.bool_literal,
            $.null_literal,
        ),

        bool_literal: _ => choice('true', 'false'),

        null_literal: _ => 'null',

        struct_literal: $ => seq(
            '{',
            optional(commaSep1($.field_initializer)),
            '}',
        ),

        field_initializer: $ => seq(
            field('name', $.identifier),
            ':',
            field('value', $.expression),
        ),

        array_literal: $ => seq(
            '[',
            optional(choice(
                seq($.expression, repeat(seq(',', $.expression)), optional(',')),
                seq($.expression, ';', optional($.integer_literal)),
            )),
            ']',
        ),

        closure_expression: $ => prec(PREC.closure, seq(
            optional('async'),
            $.closure_intro,
            $.closure_body,
        )),

        closure_intro: $ => choice(
            '||',
            seq('|', optional($.closure_parameter_list), '|'),
        ),

        closure_parameter_list: $ => commaSep1($.closure_parameter),

        closure_parameter: $ => choice(
            $.binding_name,
            seq($.type, $.binding_name),
        ),

        closure_body: $ => choice(
            $.block,
            $.expression,
        ),

        pattern: $ => choice(
            $.wildcard_pattern,
            $.variant_pattern,
            $.binding_name,
        ),

        wildcard_pattern: _ => '_',

        variant_pattern: $ => prec(1, seq(
            choice($.qualified_name, $.identifier),
            optional(seq(
                '(',
                optional($.pattern_list),
                ')',
            )),
        )),

        pattern_list: $ => commaSep1($.pattern),

        string_literal: _ => token(seq(
            '"',
            repeat(choice(
                /[^"\\\n]/,
                /\\(["'\\0nrt]|x[0-9A-Fa-f]{2})/,
            )),
            '"',
        )),

        char_literal: _ => token(seq(
            "'",
            choice(
                /[^'\\\n]/,
                /\\(["'\\0nrt]|x[0-9A-Fa-f]{2})/,
            ),
            "'",
        )),

        float_literal: _ => token(prec(1, choice(
            /[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
            /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/,
        ))),

        integer_literal: _ => token(choice(
            /0x[0-9A-Fa-f][0-9A-Fa-f_]*/,
            /[0-9][0-9_]*/,
        )),

        identifier: $ => choice(
            $.regular_identifier,
            $.contextual_identifier,
        ),

        regular_identifier: _ => /[A-Za-z_][A-Za-z0-9_]*/,

        _star: _ => token(prec(-1, '*')),

        contextual_identifier: _ => choice(
            'async',
            'await',
            'biased',
            'select',
            'fn',
        ),
    },
});

function commaSep1(rule) {
    return seq(rule, repeat(seq(',', rule)), optional(','));
}
