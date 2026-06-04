# Bitwise Operators Proposal

This proposal adds ordinary integer bitwise and shift operators to Ciel. The
goal is to cover common systems tasks such as masks, flags, protocol fields,
runtime handle packing, and low-level arithmetic without pushing those patterns
into imported C helper functions.

## Proposal Order

```text
bitwise-operators || dispatch-actor-io-runtime[runtime handle representation]
bitwise-operators || unsafe[raw integer and handle glue]
```

This proposal is intentionally independent from `dispatch-actor-io-runtime`.
That runtime can use separate fields or C-side packing without waiting for
bitwise syntax in user code. If bitwise operators land later, the runtime and
standard library may adopt them for local encodings and masks.

`unsafe` remains the owner of raw-handle adoption and foreign contracts. This
proposal only adds integer operators. It does not mark any new operation as
unsafe.

## Problem

Current Ciel expressions support logical operators, comparisons, arithmetic, and
remainder, but they do not support:

1. bitwise `&`, `|`, `^`;
2. shifts `<<`, `>>`;
3. unary bitwise not.

That gap is small for pure application code, but it becomes awkward for systems
code:

```ciel
// desired style
u32 flags = READ | WRITE;
u64 token = (generation << 32) | slot;
u8 high = (byte >> 4) & 0x0f;
```

Without these operators, users must either:

1. route simple integer manipulation through imported C helpers; or
2. keep multiple fields separate even when a compact integer representation is
   the clearest runtime ABI.

Neither is a good language default.

## Goals

1. Add familiar integer bitwise and shift operators.
2. Keep the feature narrow: integers and `char`-adjacent byte-oriented code, not
   a generalized operator-overloading system.
3. Preserve the current explicit type discipline. Mixed-width expressions should
   not silently widen through ad hoc rules.
4. Reuse the existing parser, THIR, and C backend structure.

## Non-Goals

1. No operator overloading.
2. No arbitrary-precision integer semantics.
3. No new flag-set standard-library abstraction in this proposal.
4. No promise that signed right shift is portable across every future backend;
   the first implementation should specify the required lowering behavior.

## Syntax

Add these operators:

```text
Unary:  ~
Binary: &  |  ^  <<  >>
```

Updated expression grammar:

```text
Expr            ::= LogicalOr
LogicalOr       ::= LogicalAnd { "||" LogicalAnd }
LogicalAnd      ::= BitwiseOr { "&&" BitwiseOr }
BitwiseOr       ::= BitwiseXor { "|" BitwiseXor }
BitwiseXor      ::= BitwiseAnd { "^" BitwiseAnd }
BitwiseAnd      ::= Equality { "&" Equality }
Equality        ::= Relational { ( "==" | "!=" ) Relational }
Relational      ::= Shift { ( "<" | "<=" | ">" | ">=" ) Shift }
Shift           ::= Additive { ( "<<" | ">>" ) Additive }
Additive        ::= Multiplicative { ( "+" | "-" ) Multiplicative }
Multiplicative  ::= CastExpr { ( "*" | "/" | "%" ) CastExpr }
CastExpr        ::= UnaryExpr [ "as" Type ]
UnaryExpr       ::= ( "!" | "~" | "-" | "&" | "*" ) UnaryExpr | PostfixExpr
```

This precedence matches ordinary systems-language expectations:

1. shifts bind tighter than comparisons and looser than `+` / `-`;
2. bitwise operators sit below equality and above logical `&&` / `||`;
3. unary `~` follows the same prefix pattern as `!` and unary `-`.

## Type Rules

Bitwise operators apply only to integer-like scalar values:

1. `i8 i16 i32 i64 isize`
2. `u8 u16 u32 u64 usize`
3. optionally `char` only through explicit cast, not directly

The first implementation should reject `bool`, floating-point values, pointers,
closures, slices, structs, enums, and dynamic interfaces.

Rules:

1. `x & y`, `x | y`, and `x ^ y` require both operands to have the same integer
   type after literal inference. The result has that type.
2. `x << y` and `x >> y` require the left operand to be an integer type. The
   right operand must also be an integer type. The result type is the left
   operand type.
3. `~x` requires an integer type and returns the same type.
4. Integer literals continue using the existing expected-type and assignment
   rules. This proposal does not add default unsigned literal widening or
   numeric literal suffixes.

Examples:

```ciel
u32 mask = (1 as u32) << 5;
u8 nibble = (byte >> (4 as u8)) & (0x0f as u8);
i64 both = left ^ right;
```

Rejected:

```ciel
bool bad = true & false;   // error
i32 mix = 1 | (1 as u32);  // error
f64 nope = 1.0 << 2;       // error
```

## Shift Semantics

Left shift is logical shift on the underlying bit pattern.

Right shift should be defined as:

1. logical right shift for unsigned integers;
2. arithmetic right shift for signed integers.

The C backend must not rely on implementation-defined signed right-shift
behavior without documenting the requirement. The first implementation may:

1. require two's-complement plus arithmetic signed right shift on supported C
   targets; or
2. lower signed right shift through an explicit helper when needed.

The simpler route is acceptable if the supported-target contract is written
down.

Shift counts:

1. negative shift counts are impossible because the operator works on integer
   values, not signed literals with special treatment;
2. counts greater than or equal to the bit width should be rejected when they
   are compile-time constants;
3. dynamic out-of-range counts are left unspecified in the first slice unless
   the compiler chooses to insert masking or helper calls.

The narrowest initial rule is good enough: reject obviously invalid constant
counts and document dynamic behavior as target-defined until a stricter
semantics proposal is needed.

## Examples

Bit flags:

```ciel
u32 READ = (1 as u32) << 0;
u32 WRITE = (1 as u32) << 1;
u32 EXEC = (1 as u32) << 2;

bool has_write(u32 flags) {
    return (flags & WRITE) != (0 as u32);
}
```

Packed handle:

```ciel
u64 pack_handle(u32 generation, u32 slot) {
    return ((generation as u64) << 32) | (slot as u64);
}

u32 unpack_slot(u64 handle) {
    return (handle & (0xffff_ffff as u64)) as u32;
}
```

Byte parsing:

```ciel
u8 upper_nibble(u8 byte) {
    return (byte >> (4 as u8)) & (0x0f as u8);
}
```

## Implementation Sketch

1. Add lexer tokens for `~`, `^`, `<<`, and `>>`.
2. Extend the expression parser with bitwise precedence levels.
3. Add THIR and AST variants for the new binary and unary operators.
4. Extend type checking with integer-only validation rules.
5. Lower directly to C operators where semantics are acceptable on supported
   targets.
6. Add targeted parser, typeck, and codegen tests for precedence and signed vs
   unsigned shift behavior.

## Tests

1. parser precedence for `a + b << c & d ^ e | f && g`;
2. integer bitwise operations compile and run for signed and unsigned widths;
3. `bool`, float, pointer, and closure operands are rejected;
4. mixed signed/unsigned widths are rejected without an explicit cast;
5. constant out-of-range shifts are rejected;
6. packed-handle and bit-mask examples lower correctly to C.

## Acceptance Criteria

The proposal is implemented when:

1. `design.md` includes the new grammar and operator rules;
2. parser, typeck, and codegen all recognize the new operators;
3. negative tests cover non-integer misuse and mixed-type misuse;
4. run tests confirm basic signed and unsigned shift behavior on supported
   targets.
