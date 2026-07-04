#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

enum TokenType {
    POINTER_DECLARATION_CONSTRUCTOR,
    DEREF_ASSIGNMENT_STAR,
    EXPRESSION_START_STAR,
};

static bool is_identifier_start(int32_t c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_';
}

static bool is_identifier_continue(int32_t c) {
    return is_identifier_start(c) || (c >= '0' && c <= '9');
}

static bool is_horizontal_space(int32_t c) { return c == ' ' || c == '\t'; }

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

static bool is_space(int32_t c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f';
}

static void skip_space(TSLexer *lexer) {
    while (is_space(lexer->lookahead)) {
        skip(lexer);
    }
}

static void skip_horizontal_space(TSLexer *lexer) {
    while (is_horizontal_space(lexer->lookahead)) {
        advance(lexer);
    }
}

static bool scan_keyword(TSLexer *lexer, const char *keyword) {
    for (const char *cursor = keyword; *cursor; cursor++) {
        if (lexer->lookahead != *cursor) {
            return false;
        }
        advance(lexer);
    }
    return !is_identifier_continue(lexer->lookahead);
}

static bool scan_identifier(TSLexer *lexer) {
    if (!is_identifier_start(lexer->lookahead)) {
        return false;
    }
    advance(lexer);
    while (is_identifier_continue(lexer->lookahead)) {
        advance(lexer);
    }
    return true;
}

static bool scan_pointer_constructor(TSLexer *lexer, bool *is_plain_star,
                                     bool mark_token_end) {
    *is_plain_star = false;
    if (lexer->lookahead == '?') {
        advance(lexer);
        if (lexer->lookahead != '*') {
            return false;
        }
        advance(lexer);
        if (mark_token_end) {
            lexer->mark_end(lexer);
        }
        if (lexer->lookahead == 'c') {
            if (scan_keyword(lexer, "const") && mark_token_end) {
                lexer->mark_end(lexer);
            }
        }
        return true;
    }

    if (lexer->lookahead != '*') {
        return false;
    }
    advance(lexer);
    *is_plain_star = true;
    if (mark_token_end) {
        lexer->mark_end(lexer);
    }
    if (lexer->lookahead == 'c') {
        if (scan_keyword(lexer, "const")) {
            *is_plain_star = false;
            if (mark_token_end) {
                lexer->mark_end(lexer);
            }
        }
    }
    return true;
}

static bool scan_binding_after_space(TSLexer *lexer) {
    skip_horizontal_space(lexer);
    if (lexer->lookahead == '@') {
        advance(lexer);
    }
    if (!scan_identifier(lexer)) {
        return false;
    }
    skip_horizontal_space(lexer);
    return lexer->lookahead == '=' || lexer->lookahead == ';';
}

static void scan_string(TSLexer *lexer) {
    if (lexer->lookahead != '"') {
        return;
    }
    advance(lexer);
    while (lexer->lookahead && lexer->lookahead != '\n') {
        if (lexer->lookahead == '\\') {
            advance(lexer);
            if (lexer->lookahead) {
                advance(lexer);
            }
            continue;
        }
        if (lexer->lookahead == '"') {
            advance(lexer);
            return;
        }
        advance(lexer);
    }
}

static bool scan_pointer_declaration_tail(TSLexer *lexer) {
    bool ignored = false;
    skip_horizontal_space(lexer);
    while (lexer->lookahead == '?' || lexer->lookahead == '*') {
        if (!scan_pointer_constructor(lexer, &ignored, false)) {
            return false;
        }
        skip_horizontal_space(lexer);
    }

    bool saw_type_token = false;
    uint32_t angle_depth = 0;
    uint32_t paren_depth = 0;
    uint32_t bracket_depth = 0;

    for (;;) {
        int32_t c = lexer->lookahead;
        if (c == 0 || c == '\n' || c == '\r' || c == ';' || c == '=') {
            return false;
        }
        if (angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 &&
            (c == ')' || c == '}' || c == ',' || c == '>')) {
            return false;
        }

        if (angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 &&
            is_horizontal_space(c)) {
            if (scan_binding_after_space(lexer)) {
                return true;
            }
            saw_type_token = true;
            continue;
        }

        saw_type_token = true;
        if (c == '"') {
            scan_string(lexer);
            continue;
        }
        if (c == '<') {
            angle_depth++;
        } else if (c == '>' && angle_depth > 0) {
            angle_depth--;
        } else if (c == '(') {
            paren_depth++;
        } else if (c == ')' && paren_depth > 0) {
            paren_depth--;
        } else if (c == '[') {
            bracket_depth++;
        } else if (c == ']' && bracket_depth > 0) {
            bracket_depth--;
        }
        advance(lexer);

        if (saw_type_token && angle_depth == 0 && paren_depth == 0 &&
            bracket_depth == 0 && is_horizontal_space(lexer->lookahead)) {
            if (scan_binding_after_space(lexer)) {
                return true;
            }
        }
    }
}

static bool scan_until_assignment(TSLexer *lexer) {
    uint32_t angle_depth = 0;
    uint32_t paren_depth = 0;
    uint32_t bracket_depth = 0;
    int32_t previous = 0;

    for (;;) {
        int32_t c = lexer->lookahead;
        if (c == 0 || c == '\n' || c == '\r' || c == ';') {
            return false;
        }
        if (angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 &&
            (c == ')' || c == '}' || c == ',')) {
            return false;
        }
        if (angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 &&
            c == '=') {
            advance(lexer);
            if (lexer->lookahead == '=' || previous == '!' || previous == '<' ||
                previous == '>') {
                return false;
            }
            return true;
        }
        if (c == '"') {
            scan_string(lexer);
            continue;
        }
        if (c == '<') {
            angle_depth++;
        } else if (c == '>' && angle_depth > 0) {
            angle_depth--;
        } else if (c == '(') {
            paren_depth++;
        } else if (c == ')' && paren_depth > 0) {
            paren_depth--;
        } else if (c == '[') {
            bracket_depth++;
        } else if (c == ']' && bracket_depth > 0) {
            bracket_depth--;
        }
        previous = c;
        advance(lexer);
    }
}

static bool scan_pointer_or_deref_start(TSLexer *lexer, bool *is_declaration,
                                        bool *is_deref_assignment,
                                        bool *is_expression_start_star) {
    bool first_is_plain_star = false;
    if (!scan_pointer_constructor(lexer, &first_is_plain_star, true)) {
        return false;
    }
    *is_expression_start_star = first_is_plain_star;

    *is_declaration = scan_pointer_declaration_tail(lexer);
    if (*is_declaration) {
        *is_deref_assignment = false;
        return true;
    }
    *is_deref_assignment = first_is_plain_star && scan_until_assignment(lexer);
    return true;
}

void *tree_sitter_ciel_external_scanner_create(void) { return NULL; }

void tree_sitter_ciel_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_ciel_external_scanner_serialize(void *payload,
                                                     char *buffer) {
    (void)payload;
    (void)buffer;
    return 0;
}

void tree_sitter_ciel_external_scanner_deserialize(void *payload,
                                                   const char *buffer,
                                                   unsigned length) {
    (void)payload;
    (void)buffer;
    (void)length;
}

bool tree_sitter_ciel_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
    (void)payload;
    skip_space(lexer);
    if (valid_symbols[POINTER_DECLARATION_CONSTRUCTOR] ||
        valid_symbols[DEREF_ASSIGNMENT_STAR] ||
        valid_symbols[EXPRESSION_START_STAR]) {
        bool is_declaration = false;
        bool is_deref_assignment = false;
        bool is_expression_start_star = false;
        if (!scan_pointer_or_deref_start(lexer, &is_declaration,
                                         &is_deref_assignment,
                                         &is_expression_start_star)) {
            return false;
        }
        if (is_declaration && valid_symbols[POINTER_DECLARATION_CONSTRUCTOR]) {
            lexer->result_symbol = POINTER_DECLARATION_CONSTRUCTOR;
            return true;
        }
        if (is_deref_assignment && valid_symbols[DEREF_ASSIGNMENT_STAR]) {
            lexer->result_symbol = DEREF_ASSIGNMENT_STAR;
            return true;
        }
        if (is_expression_start_star && valid_symbols[EXPRESSION_START_STAR]) {
            lexer->result_symbol = EXPRESSION_START_STAR;
            return true;
        }
    }
    return false;
}
