# Ciel Specification

This document is the normative specification for Ciel. Ciel compiles whole
programs to a single generated C translation unit, then invokes the target
system C compiler. The runtime uses BDWGC/libgc and pthread.

## 1. Language Model

Ciel is a whole-program, ahead-of-time compiled language with C interop as a
core goal. The source program is resolved as a closed set of imported files,
checked, lowered to one generated C translation unit, and then compiled by the
target C compiler.

Ciel is garbage-collected. Local values do not expose stack or heap placement;
the compiler chooses storage and promotes values to the GC heap when required
for safety. Safe Ciel prevents dangling local-address use, null dereference of
non-null pointers, unchecked enum pattern omissions, and unsafe C ABI mismatch
inside Ciel declarations. Safe concurrency is actor-first: ordinary mutable
objects are actor-local, cross-actor communication uses explicit `Message`
conversion, and shared identity is exposed only through synchronized handles.
Allocation placement is a compiler and runtime decision rather than a
source-level operation.

The language uses value semantics for structs, enums, and fixed-size arrays.
Assignment is shallow field-wise or element-wise copy. Resource-like
standard-library values are GC-managed handles with explicit operations such as
`close`, usually paired with `defer`.

Program execution starts at `i64 main()` unless the program is built as a C ABI
library. A nonzero `main` result is returned to the host process.

Top-level source constructs are declarations, imports, type definitions,
interfaces, impls, and functions.
Global escape in this specification refers to compiler/runtime global storage
or storage reachable through external C code.

Local variables may be declared without an initializer, but compile-time
definite-assignment analysis rejects every read, address take, return, field
access, or index access unless the value is definitely assigned.
Function parameters are always definitely assigned.

## 2. Notation

The grammar uses EBNF:

```ebnf
{ X }     zero or more X
[ X ]     optional X
X | Y     either X or Y
"tok"     literal token
```

Whitespace and comments separate tokens and are otherwise ignored. Line comments
start with `//` and run to the end of the line. Block comments use `/* ... */`
and do not nest.
Source files are UTF-8 encoded.

## 3. Lexical Elements

```ebnf
Identifier      ::= IdentStart { IdentContinue }
IdentStart      ::= "_" | "A"..."Z" | "a"..."z"
IdentContinue   ::= IdentStart | "0"..."9"

IntegerLiteral  ::= DecimalInteger | HexInteger
DecimalInteger  ::= Digit { Digit | "_" }
HexInteger      ::= "0x" HexDigit { HexDigit | "_" }
Digit           ::= "0"..."9"
HexDigit        ::= Digit | "A"..."F" | "a"..."f"

FloatLiteral    ::= DecimalInteger "." DecimalInteger [ Exponent ]
                 | DecimalInteger Exponent
Exponent        ::= ( "e" | "E" ) [ "+" | "-" ] DecimalInteger

CharLiteral     ::= "'" CharBody "'"
StringLiteral   ::= '"' { StringChar } '"'
CharBody        ::= CharText | EscapeSeq
StringChar      ::= StringText | EscapeSeq
CharText        ::= any source character except "'" or "\\" or newline
StringText      ::= any source character except '"' or "\\" or newline
EscapeSeq       ::= "\\" ( "\\" | "'" | '"' | "0" | "n" | "r" | "t"
                    | "x" HexDigit HexDigit )

BoolLiteral     ::= "true" | "false"
NullLiteral     ::= "null"
```

Keywords are reserved and cannot be used as identifiers:

```text
as bool break case char const continue default defer else enum export extern
false for if impl import interface never noescape null opaque return struct switch
true type void while
```

`fn` is not reserved. It is a contextual token recognized only while parsing a
function-pointer type suffix.

Primitive type names are also reserved:

```text
i8 i16 i32 i64 u8 u16 u32 u64 usize f32 f64
```

`usize` is an unsigned integer large enough to hold an object size on the
target platform. `char` is one byte. Unicode handling is a standard-library
concern.

## 4. Source Files, Modules, and Configuration

Each file is a module. Only `export` items are visible to importers. A program
is the transitive closure of files imported from the entry file. Import cycles
are compile errors.

There are no implicit imports. Standard-library names are available only after
an explicit `import`. Examples in this document may omit unrelated imports for
brevity; real source files must import every external name they use.

`import /x/y` imports a standard-library file. `import ./x` or `import x`
imports a relative file. Directories are not importable modules. If a directory
wants a facade, it must provide an explicit file such as `lib.ciel`, and users
must import that file explicitly.

Module paths omit the `.ciel` extension.

```ebnf
SourceFile      ::= { ConfigItem | TopLevel }

ConfigItem      ::= IfConfig { ElifConfig } [ ElseConfig ] "#endif"
IfConfig        ::= "#if" ConfigExpr { ConfigItem | TopLevel }
ElifConfig      ::= "#elif" ConfigExpr { ConfigItem | TopLevel }
ElseConfig      ::= "#else" { ConfigItem | TopLevel }

ConfigExpr      ::= ConfigOr
ConfigOr        ::= ConfigAnd { "||" ConfigAnd }
ConfigAnd       ::= ConfigUnary { "&&" ConfigUnary }
ConfigUnary     ::= "!" ConfigUnary | ConfigPrimary
ConfigPrimary   ::= ConfigCall | "(" ConfigExpr ")"
ConfigCall      ::= ConfigFunc "(" StringLiteral ")"
ConfigFunc      ::= "has_feature" | "is_target_os" | "is_target_arch"

TopLevel        ::= [ "export" ] ( ExportableItem | ExternBlock )
                 | ImplDecl
                 | CIncludeDecl

ExportableItem  ::= ImportDecl
                 | TypeAliasDecl
                 | CSpellingTypeDecl
                 | StructDecl
                 | EnumDecl
                 | InterfaceDecl
                 | InterfaceAliasDecl
                 | FunctionDecl

ImportDecl      ::= "import" ModulePath [ "as" Identifier ] ";"
ModulePath      ::= AbsoluteModulePath | RelativeModulePath

AbsoluteModulePath ::= "/" ModulePathPart { "/" ModulePathPart }
RelativeModulePath ::= [ "./" ] ModulePathPart { "/" ModulePathPart }
ModulePathPart      ::= Identifier

CIncludeDecl    ::= "#c_include" StringLiteral
```

Configuration gates are item-level only. They cannot appear inside statements,
parameter lists, type lists, or expressions. Inactive branches must be
lexically valid but are not parsed or semantically analyzed. Unknown features
evaluate to `false`.

No Ciel text macros exist: there is no `#define`, token pasting, or
macro-generated Ciel source. C preprocessing is only used later on generated C.
`#c_include` is copied to the generated C file but does not declare any Ciel
names; C APIs still need explicit `extern "C"` declarations.

## 5. Names and Scopes

Ciel has a single namespace for values, functions, types, enum variants,
interfaces, and aliases. Function overloading is forbidden. Two visible
bare declarations with the same name are an error only when a bare use is
ambiguous. Names inside aliased imports are not bare declarations.

Lexical declarations shadow outer declarations from their declaration point
forward. Each block introduces a new lexical scope. This includes local
declarations shadowing imported symbols.

Variables declared in a `for` initializer are scoped to that `for` statement.

```rust
import ./reader;
import ./writer as writer;

open();         // bare lookup; can only see local names and unaliased imports
writer::open(); // explicit lookup through alias
```

Aliased imports do not introduce their exported names into the bare namespace.
Bare lookup never searches `alias::name` namespaces. In the example above,
`open()` can resolve only to local declarations or unaliased imports such as
`./reader`; `writer::open` is reachable only through the `writer::` qualifier.
Unaliased imports do not create a qualifier namespace.
`export import ./x as y;` re-exports the namespace `y`; it does not re-export
`x`'s symbols as bare names.

`export import ./x;` re-exports `x`'s exported bare names as bare names from the
current module. If two re-exported bare names become visible and a downstream
bare use cannot choose exactly one declaration, that use is a compile error.

## 6. Types

```ebnf
Type            ::= [ AbiSpec ] PrefixType { CallableSuffix }
PrefixType      ::= { PointerConstructor } PrimaryType
PointerConstructor ::= "*" | "*const" | "?*" | "?*const"
PrimaryType     ::= NamedType
                 | TypeHole
                 | "never"
                 | "void"
                 | ArrayType
                 | SliceType
                 | "(" Type ")"

NamedType       ::= Identifier [ TypeArgList ]
TypeHole        ::= "_"
TypeArgList     ::= "<" TypeList ">"
TypeList        ::= Type { "," Type } [ "," ]

ArrayType       ::= "[" IntegerLiteral "]" Type
SliceType       ::= "[" "]" Type | "[" "]" "const" Type

CallableSuffix  ::= FnSuffix | ClosureSuffix
FnSuffix        ::= "fn" "(" [ TypeList ] ")"
ClosureSuffix   ::= "|" "(" [ TypeList ] ")" "|"
AbiSpec         ::= "extern" StringLiteral
```

Pointer and slice views carry write permission on the view edge:

```rust
*T          // writable non-null pointer to T
*const T    // read-only non-null pointer to T
?*T         // writable nullable pointer to T
?*const T   // read-only nullable pointer to T
[]T         // writable slice view over T elements
[]const T   // read-only slice view over T elements
```

`null` has nullable pointer type and cannot be assigned to `*T` or `*const T`.
`*T` implicitly widens to `?*T` when a nullable pointer is expected, and a
writable view may weaken to the corresponding read-only view. Standalone
`const T` is not a Ciel source type. `const` appears only inside the pointer
and slice constructors `*const`, `?*const`, and `[]const`.

Read-only views are not deep immutability and do not create const-qualified
value types. Reading through `*const T` or `[]const T` produces an ordinary
`T` value. If that stored value is itself a writable pointer or slice, the
loaded view keeps its own write permission.

Standalone `const` forms are invalid:

```rust
const i64 value = 1;        // error
const bool flag = true;     // error
const Point p = make();     // error
[4]const i64 values;        // error
Result<const i64, Error> r; // error
```

`void` is a zero-size, single-value type. It is valid as a function return type,
as a type argument such as `Result<void, E>`, and in locals, fields, parameters,
enum payloads, and pattern bindings. Concrete `void` values are implicit:
locals and fields of type `void` are not explicitly initialized or assigned.
The backend erases `void` storage while preserving expression evaluation order.
Taking the address of a `void` value is invalid.

`never` is the uninhabited type for expressions that never complete normally.
It is valid as a function return type. Plain locals, fields, and parameters of
type `never` are invalid.

`[N]T` is a fixed-size array value containing `N` contiguous elements of type
`T`. Arrays and slices are zero-indexed. Index expressions are bounds-checked
and panic on out-of-bounds access. Slice subview expressions are range-checked
and panic if the range is invalid. Arrays do not decay to pointers.
Array-to-slice conversion is implicit when a slice is expected, but it follows
the source access path: a writable array lvalue can become `[]T`, while a
read-only array lvalue can become only `[]const T`.

`[N]void` has a length but no element storage. It is declared without an
initializer, and indexing performs the normal bounds check before producing the
implicit `void` value. `[]void` stores a length with a null data pointer.

`_` in type grammar is a local type hole. It is valid only in initialized local
declarations and initialized `for` declarations:

```rust
_ handler = |State<i64> state, Command<i64> command| handle(state, command);
Actor<_> actor = must(spawn_actor<State<i64>, Command<i64>>(initial, handler));
Result<Actor<_>, Error> pending =
    spawn_actor<State<i64>, Command<i64>>(initial, handler);
[]_ values = [1, 2, 3];
[3]_ fixed = [1, 2, 3];
```

Every hole is solved from the declaration initializer while type checking that
declaration. The solved concrete type is stored on the local before later
assignments, monomorphization, and code generation. Holes do not infer from
later uses:

```rust
_ value = 1;  // i64
value = 2;    // ok
value = 2.0;  // error: expected i64

_ ptr = null;  // error: null needs an expected nullable pointer type
?*i64 ok = null;
```

Partial annotations provide context, but expressions that already require an
expected type still require one:

```rust
_ point = { x: 1, y: 2 }; // error: struct literal needs a struct type
_ empty = [];             // error: empty array literal has no element type

Point point = { x: 1, y: 2 };
[]i64 empty = [];
```

A fully typed closure can infer a concrete compiler-created closure type. An
untyped closure still needs an expected callable type, and `_` alone does not
infer block-bodied closure return types:

```rust
_ inc = |i64 value| value + 1;
_ bad = |value| value + 1; // error: parameter type is not known
```

Type holes are rejected in function signatures, struct fields, enum payloads,
interface declarations, impl signatures, type aliases, extern declarations,
casts, and explicit generic type arguments. `_` remains the pattern wildcard in
pattern grammar.

Type aliases are transparent. They introduce a new name for an existing type;
they do not introduce nominal identity.

C spelling type declarations introduce transparent scalar C ABI types whose
generated C spelling is preserved exactly:

```rust
export extern "C" {
    type c_int = "int";
    type c_size_t = "size_t";
}
```

A C spelling type is not lowered to a fixed-width Ciel primitive before code
generation. For example, `c_int` above is emitted as `int`, not `int32_t`.
This is required for C headers where width-equivalent C types are still
distinct C types. C spelling type declarations cannot be generic. They are
primarily for C ABI declarations, local storage, returns, and explicit casts.
They are assignable only to the same C spelling type, except for explicit casts
between C spelling types and Ciel numeric or `char` types.

Structs, enums, and opaque structs are nominal types.

`T fn(A, B)` is a non-null function-pointer type with return type `T`.
Function-pointer values do not carry captured state. `fn` is parsed as a
function-type suffix only in type grammar; it can otherwise be used as an
ordinary identifier. The `fn` suffix has lower precedence than pointer prefixes:

```rust
*i32 fn(i64)       // function returning *i32
?*i32 fn(i64)      // function returning ?*i32
?*(i32 fn(i64))    // nullable reference to a function type
```

Repeated `fn` suffixes construct functions returning functions:

```rust
i32 fn(i64) fn(*void) // takes *void, returns i32 fn(i64)
```

Complex function types can always be written directly. Aliases give them
stable names:

```rust
void fn(i32) fn(i32, void fn(i32)) signal;

type SignalHandler = void fn(i32);
SignalHandler fn(i32, SignalHandler) signal2;
```

If no `extern "C"` ABI is written and no enclosing `extern "C"` block applies,
a function type uses the Ciel ABI. The Ciel ABI is an implementation detail.
`extern "C" T fn(...)` uses the target platform C ABI.

An ABI specifier in a type is valid only when the resulting type is a
function-pointer type. At the start of a function declaration, a leading ABI
specifier applies to the declared function; use parentheses when the return
type itself needs an ABI-qualified function-pointer type.

`T |(A, B)|` is a non-null erased closure signature type with return type `T`.
`T |(A, B): ConstraintExpr|` has the same callable shape and additionally
retains capability witnesses, using the same constraint expression surface as
generic bounds. Each closure literal first has a unique, unnameable concrete
closure type whose fields are its captured values. The concrete closure type
can be coerced to a matching erased closure signature when such a type is
expected.

A closure value is a callable value containing a generated code pointer and an
environment pointer. A retained-capability closure value also stores generated
witnesses for the retained capabilities. It may be implemented as a generated
struct similar to:

```text
*void env;
T fn(*void, A, B) call;
... retained capability witnesses ...
```

The exact layout is an implementation detail. Closure types always use the
Ciel ABI and cannot be marked `extern "C"`. They are invalid in `extern "C"`
declarations and exported C ABI declarations. Use a named `extern "C"`
function or an `extern "C" ... fn(...)` value for C callbacks.

Closure values have value semantics at the Ciel level. Ordinary copying copies
the callable value; the generated environment is GC-managed and may be shared
between closure copies. Captured bindings inside the closure body are read-only
snapshots. To mutate shared state from a closure, capture an explicit pointer,
actor handle, channel, atomic, or other synchronized handle.

Function items and function-pointer values may be used where a matching
closure type is expected; the compiler wraps them in an empty-environment
closure. A closure expression with no captures may be used where a matching
Ciel-ABI `fn` type is expected. A closure expression with captures cannot
convert to any `fn` type. Closure expressions never produce `extern "C"`
function-pointer values.

The callable suffixes compose from left to right:

```rust
i64 |(i64)|        // closure taking i64 and returning i64
i64 fn(i64) |()|   // closure taking no arguments and returning i64 fn(i64)
i64 |(i64)| fn()   // function taking no arguments and returning a closure
```

### Slice Semantics

`[]T` and `[]const T` are built-in slice view types. A writable slice value
contains:

```text
*T ptr;
usize len;
```

The read-only form has the same descriptor shape but its pointer field is
read-only:

```text
*const T ptr;
usize len;
```

Slice fields are accessed directly:

```rust
[4]i64 @values = [1, 2, 3, 4];
[]i64 view = values;
*i64 raw_values = view.ptr;

[]const char text = "hello";
usize n = text.len;
*const char raw_text = text.ptr;
char first = text[0];
```

`s.len` returns the element count. `s.ptr` returns a non-null pointer to the
first element when `s.len > 0`. For an empty slice, `s.ptr` is still non-null
but must not be dereferenced or passed to C as an element pointer unless the C
API accepts an empty range.

A slice does not own its storage. Escape analysis keeps the backing storage
alive when a slice escapes. Slices can be created by:

```rust
[4]i64 @values = [1, 2, 3, 4];
[]i64 view = values; // array-to-slice
[]i64 tail = view[2..]; // subview
[]i64 mid = values[1..3]; // array subview

[4]i64 read_only_values = [1, 2, 3, 4];
[]const i64 read_only_view = read_only_values;

[]const char text = "hello"; // string-literal slice
```

The core slice creation forms are array-to-slice conversion, slice subview
expressions, string literals, and library functions that construct slices from
pointer/length pairs.

## 7. Declarations

```ebnf
TypeAliasDecl       ::= [ AbiSpec ] "type" Identifier [ GenericParamList ]
                        "=" Type ";"
CSpellingTypeDecl   ::= [ AbiSpec ] "type" Identifier "=" StringLiteral ";"

StructDecl          ::= "struct" Identifier [ GenericParamList ] StructBody
StructBody          ::= "{" { FieldDecl } "}"
FieldDecl           ::= Type Identifier ";"

EnumDecl            ::= "enum" Identifier [ GenericParamList ] EnumBody
EnumBody            ::= "{" [ VariantDecl { "," VariantDecl } [ "," ] ] "}"
VariantDecl         ::= Identifier [ "(" TypeList ")" ]

InterfaceDecl       ::= "interface" GenericParamList InterfaceSignature ";"
InterfaceSignature  ::= Type Identifier "(" [ ParamList ] ")"
InterfaceAliasDecl  ::= "interface" Identifier "=" InterfaceExpr ";"

InterfaceExpr       ::= InterfaceTerm { ( "+" | "-" ) InterfaceTerm }
InterfaceTerm       ::= Identifier [ TypeArgList ]

ImplDecl            ::= "impl" [ GenericParamList ] Identifier [ TypeArgList ]
                        "(" [ ParamList ] ")" Block

FunctionDecl        ::= [ AbiSpec ] FunctionSignature ( Block | ";" )
FunctionSignature   ::= Type Identifier [ GenericParamList ]
                        "(" [ ParamList ] ")"

GenericParamList    ::= "<" GenericParam { "," GenericParam } [ "," ] ">"
GenericParam        ::= Identifier [ ":" ConstraintExpr ]
ConstraintExpr      ::= ConstraintTerm { ( "+" | "-" ) ConstraintTerm }
ConstraintTerm      ::= [ "!" ] Identifier [ TypeArgList ]

ParamList           ::= Param { "," Param } [ "," ]
Param               ::= Type BindingName
BindingName         ::= [ "@" ] Identifier

ExternBlock         ::= "extern" StringLiteral "{" { ExternItem } "}"
ExternItem          ::= OpaqueStructDecl
                     | [ "noescape" ] FunctionSignature ";"
                     | TypeAliasDecl
                     | CSpellingTypeDecl
OpaqueStructDecl    ::= "opaque" "struct" Identifier ";"
```

Local variables and function parameters are declared with type syntax.
`BindingName` controls whether the binding may be assigned again:

```rust
i64 value = 1;      // immutable binding
i64 @count = 0;     // mutable binding

void step(i64 input, i64 @state) {
    state = state + input;
}
```

A binding without `@` is immutable after initialization. A binding with `@`
may be assigned repeatedly. `@` belongs to the binding name, not to the type;
a mutable binding may hold a read-only pointer or slice view.

```rust
*const i64 @cursor = start;
cursor = next; // ok: the pointer binding is mutable
*cursor = 1;   // error: the pointer view is read-only
```

Local declarations may use `_` type holes only when they have an initializer.
There is no assignment-based `auto`.

Struct and enum assignment is shallow field-wise copy. Fixed-size array
assignment is element-wise copy. Slice assignment copies only the slice view.
Assignment evaluates the right-hand side first, then stores into the
left-hand-side lvalue. Returning a struct, enum, or array value uses the same
value semantics at the Ciel level; backend lowering may avoid physical copies.

A local declaration without an initializer creates an uninitialized binding,
unless its type contains a hole. Declarations with type holes require an
initializer. The binding must be definitely assigned before any use. Assigning
to the whole local initializes it. Aggregate construction initializes the whole
local at once.

Immutable locals may be declared before their initializer, but they may be
initialized only once on every control-flow path. The initializing assignment
must target the whole binding:

```rust
i64 x;
if (cond) {
    x = 1;
} else {
    x = 2;
}

x = 3; // error: x is already initialized
```

Partial writes cannot initialize an immutable aggregate:

```rust
Point p;
p.x = 1; // error: immutable delayed initialization must assign the whole value
```

Definite assignment is a compile-time forward data-flow analysis:

1. function parameters start assigned
2. `T x;` starts unassigned
3. `T x = expr;` checks `expr`, then marks `x` assigned
4. assigning to a mutable binding checks `expr`, stores, and marks it assigned
5. assigning to an unassigned immutable local initializes it only when the
   target is the whole binding
6. assigning to an assigned immutable local is an error
7. `x.field = expr` and `x[i] = expr` require `x` to already be assigned and
   require a writable access path
8. branch merges use three states:
   `assigned + assigned => assigned`,
   `unassigned + unassigned => unassigned`,
   `assigned + unassigned => maybe-assigned`, and
   `maybe-assigned + anything => maybe-assigned`
9. loop bodies are conservative: assigning an immutable local declared outside
   a `while` or `for` body is rejected unless a later specification adds
   stronger control-flow proof

No runtime initialized-bit checks are part of the language.

Type holes still require initializers:

```rust
_ value = make_value(); // ok
_ value;                // error
_ @value;               // error
```

Struct declarations do not define default field values. A struct value is
created by a named-field struct literal, by copying another value, by a
function return, or by C interop according to the declared ABI.

At most one function body may exist for a given fully qualified name. A
non-`extern` function declaration ending in `;` is a prototype and must match
the eventual body exactly. `extern "C"` declarations do not require a Ciel body.

By-value recursive types are illegal. During type layout, if a struct or enum
contains itself by value through a cycle, semantic analysis reports an error.
Recursive references must use pointers.

Generic calls are resolved by unification over explicit type arguments,
argument types, constraints, and the expected result type. If any generic
parameter remains unresolved, the program is rejected. This is a general typing
rule, not a special case for any particular function.

Generic functions and generic aggregate types are monomorphized for the concrete
types reachable in the whole program. Infinite generic instantiation cycles are
compile errors.

## 8. Expressions and Statements

```ebnf
Block           ::= "{" { Statement } "}"

Statement       ::= Block
                 | VarDeclStmt
                 | AssignStmt
                 | IfStmt
                 | WhileStmt
                 | ForStmt
                 | SwitchStmt
                 | DeferStmt
                 | ReturnStmt
                 | BreakStmt
                 | ContinueStmt
                 | ExprStmt

VarDeclStmt     ::= Type BindingName [ "=" Expr ] ";"
AssignStmt      ::= LValue "=" Expr ";"
ExprStmt        ::= Expr ";"

IfStmt          ::= "if" "(" Expr ")" Block [ "else" ( Block | IfStmt ) ]
WhileStmt       ::= "while" "(" Expr ")" Block
ForStmt         ::= "for" "(" [ ForInit ] ";" ExprOpt ";"
                    [ ForStep ] ")" Block
ForInit         ::= Type BindingName [ "=" Expr ]
                 | LValue "=" Expr
                 | Expr
ForStep         ::= LValue "=" Expr
                 | Expr
ExprOpt         ::= [ Expr ]

SwitchStmt      ::= "switch" "(" Expr ")" "{" { CaseClause } [ DefaultClause ] "}"
CaseClause      ::= "case" Pattern ":" { Statement }
DefaultClause   ::= "default" ":" { Statement }

DeferStmt       ::= "defer" CallExpr ";"
ReturnStmt      ::= "return" [ Expr ] ";"
BreakStmt       ::= "break" ";"
ContinueStmt    ::= "continue" ";"

Expr            ::= LogicalOr
LogicalOr       ::= LogicalAnd { "||" LogicalAnd }
LogicalAnd      ::= Equality { "&&" Equality }
Equality        ::= Relational { ( "==" | "!=" ) Relational }
Relational      ::= Additive { ( "<" | "<=" | ">" | ">=" ) Additive }
Additive        ::= Multiplicative { ( "+" | "-" ) Multiplicative }
Multiplicative  ::= CastExpr { ( "*" | "/" | "%" ) CastExpr }
CastExpr        ::= UnaryExpr [ "as" Type ]
UnaryExpr       ::= ( "!" | "-" | "&" | "*" ) UnaryExpr | PostfixExpr

PostfixExpr     ::= PrimaryExpr { PostfixOp }
PostfixOp       ::= CallSuffix
                 | FieldSuffix
                 | ArrowSuffix
                 | IndexSuffix
                 | SliceSuffix
                 | TrySuffix
CallSuffix      ::= [ TypeArgList ] "(" [ ArgList ] ")"
FieldSuffix     ::= "." Identifier
ArrowSuffix     ::= "->" Identifier
IndexSuffix     ::= "[" Expr "]"
SliceSuffix     ::= "[" [ Expr ] ".." [ Expr ] "]"
TrySuffix       ::= "?"

CallExpr        ::= PostfixExpr
ArgList         ::= Expr { "," Expr } [ "," ]

PrimaryExpr     ::= Identifier
                 | QualifiedName
                 | Literal
                 | StructLiteral
                 | ArrayLiteral
                 | ClosureExpr
                 | "(" Expr ")"

QualifiedName   ::= Identifier "::" Identifier { "::" Identifier }
Literal         ::= IntegerLiteral | FloatLiteral | CharLiteral
                 | StringLiteral | BoolLiteral | NullLiteral

StructLiteral   ::= "{" [ FieldInit { "," FieldInit } [ "," ] ] "}"
FieldInit       ::= Identifier ":" Expr
ArrayLiteral    ::= "[" [ Expr { "," Expr } [ "," ] | Expr ";" [ IntegerLiteral ] ] "]"

ClosureExpr     ::= ClosureIntro ClosureBody
ClosureIntro    ::= "||" | "|" ClosureParamList "|"
ClosureParamList ::= ClosureParam { "," ClosureParam } [ "," ]
ClosureParam    ::= BindingName | Type BindingName
ClosureBody     ::= Block | Expr

LValue          ::= Identifier
                 | PostfixExpr "." Identifier
                 | PostfixExpr "->" Identifier
                 | PostfixExpr "[" Expr "]"
                 | "*" UnaryExpr

Pattern         ::= QualifiedName [ "(" PatternList ")" ]
                 | BindingName
                 | "_"
PatternList     ::= Pattern { "," Pattern } [ "," ]
```

Expressions are evaluated left-to-right. Function designators are evaluated
before arguments, and arguments are evaluated in source order. Struct literals
evaluate field initializers in written order. Array literals evaluate their
elements in source order. `&&` and `||` short-circuit.

Closure expressions create closure values. The closure body is not evaluated
when the closure is created. A closure captures bare references to local value
bindings and parameters from enclosing scopes. Top-level functions, imported
functions, types, enum variants, and interface names are resolved directly and
are not captured.

```rust
i64 base = 10;
i64 |(i64)| add_base = |x| x + base;
i64 y = add_base(5); // 15
```

Captures are by value only. There are no by-reference captures and no implicit
capture of an enclosing variable's storage location. Every captured local must
be definitely assigned at the closure expression. Captured bindings inside the
closure body are read-only snapshots: the closure body cannot reassign them,
take their binding address, or assign through their fields or indices. If
mutable shared state is needed, the program must capture an explicit pointer or
synchronized handle and use that value's API.

Closure parameters may write either `BindingName` or `Type BindingName`. If a
parameter type is omitted, it must be supplied by an expected callable type.
Expected callable types come from assignment, parameter passing, return
context, or an explicit `as` type annotation. If no expected callable type
exists, every closure parameter must write its type. Closure parameters use the
same `@` mutability rule as function parameters:

```rust
i64 |(i64)| bump = |i64 @value| {
    value = value + 1;
    return value;
};
```

Closure literals do not have a return-type annotation. The return type is
supplied by the expected callable type when one exists. Without an expected
callable type, an expression-bodied closure infers its return type from the
body expression, but a block-bodied closure is rejected because Ciel does not
infer block return types.

Every closure literal creates a distinct concrete closure type even when two
closures have the same callable signature. The concrete type is a compiler
detail, but it remains visible to generic inference until it is coerced to an
erased closure signature type such as `i64 |(i64)|`. This lets later capability
checks reason about the captured environment instead of treating all closures
with the same signature as the same type.

The `as` operator can provide the expected callable type for a closure literal:

```rust
(|x| x + 1) as i64 |(i64)|;
(|x| { return x + 1; }) as i64 |(i64)|;
(|x| x + 1) as i64 |(i64): Message|;
```

This use of `as` is a compile-time closure type annotation, not a runtime cast.
The target type must be a closure type or a Ciel-ABI `fn` type. If the target
is a `fn` type, the closure must not capture anything. Closure expressions
cannot be annotated as `extern "C"` function-pointer types. Expression-bodied
closures use parentheses before `as` so the annotation applies to the closure
literal rather than to the body expression.

In a block-bodied closure, `return` returns from the closure, not from the
enclosing function. `defer`, `?`, definite assignment, and return-path analysis
inside a closure use the same rules as a function body. An expression-bodied
closure returns the value of its expression and cannot contain statements.

A call suffix may call a function item, function-pointer value, or closure
value. Closure arguments are evaluated in source order, then the closure's
generated call function is invoked with its environment and the arguments.

`if`, `while`, and `for` conditions must have type `bool`. A missing `for`
condition is treated as `true`. In `for (init; cond; step)`, `init` runs once,
`cond` is checked before each iteration, and `step` runs after each normal
iteration before the next condition check.

`break` exits the nearest enclosing loop or `switch`. `continue` exits the
current loop iteration and then runs the loop step expression when one exists.
`switch` cases do not fall through: only the selected case body executes.

Non-`void` functions must return a value on every normal control-flow path.
`void` functions may fall through. `return expr;` in a `void` function is valid
only when `expr` has type `void`; this supports generic functions instantiated
with `T = void`. Bare `return;` in a non-`void` function is invalid.
`never` functions must not fall through and cannot use `return`; they terminate
the process, abort, panic, or loop forever. Calls to functions returning
`never` are not normal fallthrough paths.

Lvalue access is tracked separately from expression type. An lvalue is either
writable or read-only:

- ordinary immutable bindings are read-only after initialization
- `@` bindings are writable
- captured bindings are read-only closure snapshots
- struct fields and fixed-array elements follow the base lvalue's access mode
- pointer dereference and `->` follow the pointer view's mutability
- slice element and subview access follow the slice view's mutability

Assignments require a writable lvalue, except for the one allowed whole-binding
initialization of an unassigned immutable local.

```rust
Point p = make_point();
p.x = 1; // error: field of an immutable owned binding

Point @m = make_point();
m.x = 1; // ok

*Point mp = &m;
mp->x = 1; // ok

*const Point rp = &m;
rp->x = 1; // error

[]Point points = get_mut_points();
points[0].x = 1; // ok

[]const Point view = points;
view[0].x = 1; // error
```

Read-only lvalues are not const-qualified rvalues. Reading a field, pointer, or
slice descriptor from a read-only aggregate produces the ordinary stored value,
including whatever view mutability that stored value carries:

```rust
struct Holder {
    *i64 ptr;
}

*const Holder h = get_holder();
h->ptr = other; // error: cannot overwrite the field
*(h->ptr) = 1;  // ok: the stored pointer value is *i64

struct ViewHolder {
    []u8 bytes;
}

*const ViewHolder vh = get_view_holder();
vh->bytes = other; // error: cannot overwrite the slice descriptor
vh->bytes[0] = 1;  // ok: the stored slice value is []u8
```

`&expr` requires an lvalue and produces a non-null pointer whose view
mutability follows the lvalue access mode:

```rust
i64 x = 1;
i64 @y = 2;

*const i64 px = &x;
*i64 py = &y;
```

Taking a writable pointer from a read-only lvalue is rejected, but taking a
read-only pointer from a writable lvalue is allowed by view weakening.

Parameters follow the same address-of rule as initialized locals. `T value` is
a read-only lvalue and `T @value` is a writable lvalue:

```rust
Result<T, Error> clone<T: Message>(T value) {
    return clone_message(&value); // &value has type *const T
}

void update(Point @p) {
    mutate(&p); // &p has type *Point
}
```

`*ptr` requires a non-null pointer. `p->field` is equivalent to `(*p).field`
after type checking. Indexing requires an array or slice operand and an integer
index; indices are zero-based and bounds-checked. Slice subview syntax
`s[start..end]`, `s[start..]`, `s[..end]`, and `s[..]` requires an array or
slice operand and integer bounds. The omitted start is `0`; the omitted end is
the operand length. The result is a slice view over the original storage whose
view mutability follows the operand access path. The valid condition is
`start <= end <= len`; invalid ranges panic.

Struct literals are named-field literals and require an expected struct type.
Every field must be initialized exactly once. Field order is irrelevant to type
checking, but evaluation follows the written initializer order:

```rust
Point p = { x: 1, y: 2 };
Point q = { y: 2, x: 1 };
```

Positional struct literals such as `{ 1, 2 }` are invalid. Array literals
require an expected array or slice element type. For `[N]T`, the literal
element count must be exactly `N`. Repeat array literals use `[expr;]` in an
expected fixed-array context; the repeat count is inferred from `[N]T`.
Repeat slice literals use `[expr; N]`, where `N` is a compile-time integer
literal. When an array literal is used where `[]T` is expected, the compiler
creates a backing array and escape analysis chooses its storage.

Integer literals default to `i64`; floating literals default to `f64`. Character
literals have type `char`. If an expected type exists, literals are checked
against that type. Literal values must be in range for the inferred type.
`null` requires an expected nullable pointer type.

String literals have type `[]const char`:

```rust
[]const char s = "hello"; // { ptr: static NUL-terminated bytes, len: 5 }
*const char p = s.ptr;    // for C APIs that expect a read-only C string
```

Each string literal occurrence denotes a program-lifetime NUL-terminated byte
array and a read-only slice whose `len` excludes the trailing NUL. The backing
bytes are emitted as static const storage.

`bool` is separate from integers. Only `true` and `false` produce bool
literals. Integers do not implicitly or explicitly convert to bool, and bool
does not convert to integers.

Binary arithmetic operators require numeric operands of the same type after
literal inference. `%` is integer-only. Relational operators require numeric or
`char` operands of the same type and return `bool`. `==` and `!=` are defined
for `bool`, numeric types, `char`, pointers of the same type, nullable pointers
of the same type, and function values of the same type. Structs, enums, and
closure values do not get structural equality by default; use explicit
functions or capabilities.

Logical `&&`, `||`, and `!` operate only on `bool`. Unary `-` operates on signed
integer and floating-point types. Pointer arithmetic is not part of Ciel.

Integer overflow traps in debug builds and wraps in release builds. Integer
division by zero panics. Floating-point operations follow IEEE 754.

`as` permits numeric-to-numeric casts, integer-to-`char` casts,
`char`-to-integer casts, and pointer casts involving `*void`, `?*void`,
`*const void`, or `?*const void`. Integer narrowing casts truncate in release
builds and trap on out-of-range values in debug builds. Pointer casts preserve
nullability and never remove read-only view mutability; converting `?*T` to
`*U` requires nullable narrowing first, and converting `*const T` to `*U` is
rejected. When the left-hand side is a closure literal or a parenthesized
closure literal, `as` may also supply a closure or Ciel-ABI function-pointer
expected type as specified above.

The pointer and slice view constructors have only these implicit view
conversions:

```rust
*T       -> *const T
*T       -> ?*T
*T       -> ?*const T
*const T -> ?*const T
?*T      -> ?*const T
[]T      -> []const T
```

Conversions that remove read-only view mutability are rejected, including under
`as`:

```rust
*const T ro = get_ro();
*T rw = ro;        // error
*T rw2 = ro as *T; // error

[]const T readonly = get_ro_slice();
[]T writable = readonly; // error
```

Implicit conversions are intentionally small: literal typing by context,
writable-to-read-only view weakening, non-null-to-nullable pointer widening,
array-to-slice conversion according to source access, function item or function
pointer to matching closure, and noncapturing closure to matching Ciel-ABI
function pointer. Other conversions require `as` or an explicit function.

## 9. Nullable Pointer Narrowing

Narrowing applies only to local bindings of nullable pointer type, including
parameters. It never applies to struct fields, globals, or arbitrary
expressions.

```rust
?*T p = get();
if (p != null) {
    use(p); // p is narrowed to *T inside this branch
}
```

`if (p == null) return;` narrows `p` to `*T` after the statement. Short-circuit
`&&` is supported:

```rust
if (p != null && p->value > 0) {
    use(p);
}
```

Reassigning `p`, assigning through a pointer to `p`, or passing `&p` to code
that may write it invalidates the narrowing immediately. Fields must be copied
to locals before narrowing:

```rust
?*T temp = obj->ptr;
if (temp != null) {
    use(temp);
}
```

## 10. Interfaces and Capabilities

An `interface` declaration introduces a capability. An `impl` implements that
capability for the receiver type shown in its first parameter.

`T: capability` in a generic parameter list is a static constraint and is
monomorphized for concrete receiver types.

The callable name introduced by an interface is resolved like any other name in
the single namespace. A call to that name is a capability call. If the receiver
argument has a concrete type, the call is statically resolved through the global
impl table. If the receiver argument is a dynamic interface value, the call is a
vtable dispatch.

An `impl` must match exactly one visible interface declaration by name. Its
parameter and return types must match the interface after substituting the
receiver type and any supplied or inferred non-receiver type arguments.

Type arguments written on an `impl` also bind only non-receiver generic
parameters.

An `impl` may have its own generic parameter list. Those parameters are inferred
from the receiver and other interface arguments, then monomorphized like a
generic function:

```ciel
impl<T> clone_message(*const Actor<T> value) {
    return Ok(*value);
}
```

A bare interface name used as a type, such as `measure value`, denotes a
dynamic interface value. A dynamic interface value stores:

```text
data pointer + vtable pointer
```

When a dynamic interface type is expected, a concrete receiver value or pointer
can be coerced to that dynamic value if the concrete receiver type implements
the required interface view. The dynamic value stores an address to receiver
storage; escape analysis keeps that storage alive. Static generic constraints
do not create dynamic values; they are monomorphized capability checks.

Dynamic interface use is valid only under these rules:

1. The first generic parameter of the interface is the receiver type.
2. The receiver type is erased into the dynamic value.
3. Every later generic parameter must be statically supplied.
4. The erased receiver parameter must appear in an input value position.

When an interface name is used as a dynamic type or as a constraint, written
type arguments bind only the non-receiver generic parameters. The receiver is
provided by the concrete constrained type or erased dynamic value.

Examples:

```rust
interface<T> i64 measure(*const T value);
i64 call_measure(measure value);
```

```rust
interface<T, U> bool eq(*const T value, U other);

bool check_eq(eq<i64> value, i64 target) {
    return eq(value, target);
}

bool bad_eq(eq value); // error: U is not supplied
```

`make` is a normal capability, but it is not dynamically usable because its
receiver type appears only in the return type:

```rust
interface<T, U> Result<T, Error> make(U value);
Mutex<i64> total = must(make(0));
must(make<Mutex<i64>>(0)); // required without expected type
```

Interface aliases use `+` and `-` to form narrowed views:

```rust
interface streaming = read - seek;
interface readable_seekable = read + seek;
```

`read - seek` masks out `seek` from the view. It does not require the concrete
type to lack `seek`.

Generic constraints may use `!capability` as a global hard rejection:

```rust
i64 copy_stream<T: read + write + !seek>(*T stream, *u8 out, usize len);
```

Semantic analysis is whole-program:

1. Collection phase: collect every `impl` from every imported file.
2. Resolution phase: evaluate constraints against the complete impl table.

`impl` declarations are not exported names. Once a file is part of the imported
program, all of its impls participate in coherence and constraint checks.

If any imported file implements `seek` for `T`, then `T: !seek` fails.

## 11. Structural Metaprogramming

Structural metaprogramming is exposed through `/std/meta`. The public surface is
ordinary Ciel: generic structs, enums, interfaces, impls, `switch`, and function
calls. The compiler only recognizes a small set of canonical `/std/meta` names
and lowers them during semantic analysis.

`/std/meta` provides product and sum vocabulary:

```rust
import /std/meta as meta;

meta::HNil
meta::HCons<meta::FieldRef<i64>, meta::HNil>
meta::Coproduct<meta::VariantRef<meta::HNil>, meta::CoNil>
```

`meta::RefRepr<T>` is a borrowed structural view. For a visible struct it
normalizes to an `HCons` list of `FieldRef<FieldType>` values in declaration
order:

```rust
struct Packet {
    i64 id;
    bool ok;
}

meta::RefRepr<Packet>
// meta::HCons<
//     meta::FieldRef<i64>,
//     meta::HCons<meta::FieldRef<bool>, meta::HNil>
// >
```

`meta::Repr<T>` is an owned structural value with `Field<FieldType>` instead of
`FieldRef<FieldType>`.

For a visible enum it normalizes to a `Coproduct` list in variant declaration
order. Each branch is a `VariantRef<PayloadProduct>` for `RefRepr<T>` and a
`Variant<PayloadProduct>` for `Repr<T>`. Positional payloads use
`PayloadRef<P>` or `Payload<P>` inside an `HCons` product:

```rust
enum Token {
    Number(i64),
    End,
}

meta::RefRepr<Token>
// meta::Coproduct<
//     meta::VariantRef<meta::HCons<meta::PayloadRef<i64>, meta::HNil>>,
//     meta::Coproduct<meta::VariantRef<meta::HNil>, meta::CoNil>
// >
```

Fixed-size arrays are structural only through owned representation. `Repr<[0]T>`
normalizes to `ArrayNil`, `Repr<[1]T>` through `Repr<[16]T>` normalize to
`ArrayChunk1<T>` through `ArrayChunk16<T>`, and larger arrays normalize to a
balanced `ArrayCat<L, R>` tree of bounded chunks. Nested fixed arrays are
expanded recursively in owned leaves, including struct fields, enum payloads,
and closure captures. Borrowed representation leaves arrays as borrowed array
leaves such as `FieldRef<[N]T>` inside named products and sums. A root
`RefRepr<[N]T>` has no field or payload wrapper, so it normalizes to the same
bounded `ArrayChunk` / `ArrayCat` shape with non-null element pointer leaves.

Array representation expansion is budgeted. Very large static arrays are bulk
storage rather than record-shaped structural data; using `meta::Repr<[N]T>` past
the budget is rejected and should be replaced by an explicit wrapper policy or
an owned buffer type.

For a concrete closure instance, structural representation exposes the captured
environment as an `HCons` product in capture order. Captures are named
`capture#0`, `capture#1`, and so on in the value-level metadata. Erased closure
signature types do not expose captures.

The compiler-lowered functions are:

```rust
meta::RefRepr<T> as_ref_repr<T>(*const T value);
meta::Repr<T> into_repr<T>(*const T value);
T from_repr<T>(meta::Repr<T> value);
```

`as_ref_repr` creates read-only pointers to visible fields, enum payloads, or
closure captures. Its result has the same lifetime and actor-local behavior as
those pointers. For enums, projection switches on the active variant and returns
the corresponding `Coproduct` branch. `into_repr` copies from a read-only source
pointer into the owned representation. `from_repr` reconstructs a struct, enum,
or concrete closure instance from the owned representation by structural
position.

Policies remain library code. A type opts into a policy by projecting itself
and delegating to ordinary generic impls:

```rust
interface<T> u64 hash(*const T value, u64 seed);
interface hashable = hash;

impl hash(*const Packet value, u64 seed) {
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return hash(&repr, seed);
}
```

The core mechanism remains explicit projection plus ordinary policy code. A
future declaration-level convenience may auto-emit those wrapper impls, but it
does not change the SOP representation.

`Message` uses this mechanism for structural user data. Ordinary structs and
enums do not automatically implement `Message`; their owned representation can:

```rust
import /std/meta as meta;

struct Packet {
    i64 id;
    bool ok;
}

type PacketMessage = meta::Repr<Packet>;
```

`/std/message` implements `clone_message` for owned SOP nodes such as `HNil`,
`HCons`, `Field`, `CoNil`, `Coproduct`, `Variant`, and `Payload`. If a field or
payload leaf lacks `Message`, ordinary generic constraint checking rejects the
representation. Code that wants the original nominal type itself to cross an
actor or channel boundary must write an explicit `clone_message(*const T)`
policy.

Owned representation recursively expands structs, enums, concrete closures, and
fixed-size arrays where no nominal policy boundary exists. A nested named field
or payload that already has an ordinary `clone_message` impl remains a leaf and
is cloned through that impl by the SOP policy. The top-level `meta::Repr<T>`
still reflects `T` itself, so this rule does not make `T` directly implement
`Message` and does not hide its fields when code explicitly asks for
`meta::Repr<T>`. Concrete closure instances are not opaque policy leaves; their
standard-library `clone_message` impl reflects captures through `meta::Repr<C>`.

Structural metaprogramming is not a text macro system. It does not generate
Ciel source, paste tokens, or run before name resolution. The order is:

1. resolve imports and identify canonical `/std/meta` declarations
2. normalize `RefRepr<T>` and `Repr<T>` while lowering types in semantic
   analysis
3. type-check generic constraints and impl calls against the normalized types
4. lower `as_ref_repr`, `into_repr`, and `from_repr`
5. run ordinary monomorphization, escape analysis, and C code generation

## 12. Enums and Pattern Matching

Enum variants live in the single namespace. Variant constructors are ordinary
names. Since overloading is forbidden, two variants with the same name cannot
be visible in the same lexical scope.

Unit variants are written without parentheses:

```rust
enum DigitError {
    DigitNonDecimal,
}

return Err(DigitNonDecimal);
```

Payload variants are ordinary constructor calls:

```rust
enum ConfigError {
    MissingPort,
    InvalidPort(i64),
}

return Err(InvalidPort(raw_port));
```

`switch` over an enum must be exhaustive unless it has `default:`. `default:`
is the only top-level fallback; `case _:` is invalid. Nested enum patterns are
matched recursively. Pattern bindings use copy semantics and are scoped to
their case body. Pattern bindings use the same `BindingName` rule as locals and
parameters: `name` is immutable and `@name` is mutable.

```rust
enum Inner {
    A(i64),
    B,
}

enum Outer {
    Wrap(Inner, Inner),
    Empty,
}

i64 pick(Outer value) {
    switch (value) {
        case Wrap(A(x), _):
            return x;
        case Wrap(B, A(y)):
            return y;
        case Wrap(B, B):
            return 0;
        case Empty:
            return 0;
    }
}
```

```rust
switch (event) {
    case Click(pos):
        pos.x = 1; // error: pos is an immutable binding
    case Drag(@pos):
        pos.x = 1; // ok
}
```

## 13. Error Handling and Panic

The `?` operator works only on the `Result<T, E>` type exported by
`/std/result`, or aliases of that type. The surrounding function must return
`Result<U, E>` with exactly the same error type `E`, or `Result<U, Error>` where
`Error` is the standard error type exported by `/std/error` and `E` implements
the standard `ErrorTrait` formatting capability. In the latter case, `?` boxes
the concrete error through `error_box`. No general implicit conversion graph is
searched.

`must` and `expect` are standard-library generic functions. They are not
special syntax. On error, they call a runtime panic function.

Panic is immediate process termination with exit status `0` and an optional
diagnostic. It does not unwind and does not run `defer` handlers. `defer` is
guaranteed only for normal control-flow exits.

## 14. Defer

`defer` registers a single direct function call. Suffixes after that call, such
as `?`, are invalid in a `defer` statement. Its arguments are evaluated when
the `defer` statement executes. Deferred calls run in strict LIFO order when
the current block exits through normal control flow:

```text
fallthrough, return, ?, break, continue
```

The scope is block-level. A `defer` inside a loop body runs at the end of that
iteration's block, not at the end of the function.

If one control-flow action exits multiple nested blocks, deferred calls run from
the innermost exited block outward.

When a loop body exits through `continue`, its block-level deferred calls run
before the loop step expression.

The return value of a deferred call is ignored.

## 15. Escape Analysis

Local values do not expose stack/heap placement. The compiler chooses storage
and may promote escaping values to the GC heap.

Escape analysis is conservative. If unsure, promote. A local value is promoted
when its address reaches:

```text
return, global storage, heap object, unknown C code, thread entry data
```

Each Ciel function gets an escape summary. Whole-program compilation iterates
summaries until stable. Dynamic interface calls are conservative. Unknown
`extern "C"` pointer parameters escape by default unless explicitly marked
`noescape`; such annotations are trusted C contracts.

Captured closure values are treated as fields of a generated environment
object. If a closure escapes, each captured value escapes according to the
environment's escape destination. Capturing a slice keeps the slice backing
storage alive when the closure escapes. Capturing a pointer copies the pointer;
it does not make the pointed-to object safe to share across actors.

If a pointer to a local value is passed as thread entry data or stored in data
reachable by another thread, the value is promoted even if user code later
joins that thread. Join-sensitive analysis is a future optimization, not a
language guarantee.

Escape analysis decides storage placement. Actor isolation and `Message`
capability checks are the concurrency safety proof; promotion alone does not
make a local object shareable across actors.

## 16. Concurrency and Actors

Ciel's concurrency model is actor-first. Asynchronous work is expressed through
actor mailboxes, channels, and synchronized handles provided by the standard
library and runtime.

The model has four parts:

1. an actor is an isolated execution domain with private mutable state
2. actor code processes one message at a time
3. sending a message constructs an independent receiver value through a
   `Message` capability
4. shared identity is represented by explicit synchronized handles such as
   actor handles, channels, atomics, and selected standard-library services

Ordinary pointers and slices are actor-local. They may be used freely inside
the actor that owns the pointed-to data. Cross-actor APIs accept message values
or synchronized handles, not borrowed interior pointers into another actor.

An actor handle is a shareable reference to a mailbox:

```rust
struct Actor<M> {
    *void handle;
}
```

Actor state remains encapsulated by the actor runtime. It is initialized when
the actor starts and is updated by the actor's handler. Actor state is never
exposed as a cross-actor `*S`.

Actors are spawned with an initial state value and a retained-capability
closure handler:

```rust
Result<Actor<M>, Error> spawn_actor<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
```

Messages are checked through `Message`, actor state is handled inside the actor
loop, and safe code cannot send a borrowed pointer to another actor's mutable
state. Actor-handler closures capture by value. Converting a concrete closure
or Ciel ABI `fn` into the handler type retains the `Message` witness used by
the actor runtime to clone the handler across the actor boundary.

`Message` is an explicit conversion capability:

```rust
interface<T> Result<T, Error> clone_message(*const T value);
interface Message = clone_message;
```

`clone_message` constructs the value that will be owned by the receiver. It may
copy fields, allocate fresh backing storage, serialize and decode, duplicate a
resource handle, intern immutable data, or report an error.

Cross-domain standard-library APIs are ordinary functions that require
`Message` and call `clone_message` explicitly:

```rust
Result<void, Error> send<M: Message>(*const Actor<M> actor, M value);
```

Conceptually, `send` clones before storing into another actor's mailbox:

```rust
Result<void, Error> send<T: Message>(*const Actor<T> actor, T value) {
    T copy = clone_message(&value)?;
    enqueue(actor, copy);
    return Ok;
}
```

The sender keeps its original value. The receiver receives the result of
`clone_message`, with independent mutable identity:

```rust
Buffer @buf = make_buffer();
*Buffer p = &buf;
send(actor, buf);        // send calls clone_message(&value)
append(p, "local only"); // mutates only the sender's buffer
```

`spawn_actor` follows the same rule:

```rust
Result<Actor<M>, Error> spawn_actor<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
) {
    S state = clone_message(&initial_state)?;
    Result<S, Error> |(S, M): Message| actor_handler = clone_message(&handler)?;
    return runtime_spawn_actor(state, actor_handler);
}
```

Closure messageability is a property of the concrete closure type's generated
environment, not of the erased callable signature alone:

```rust
i64 x = 1;
spawn_actor(0, |s, msg| s + msg + x); // ok

i64 local = 1;
*i64 ptr = &local;
spawn_actor(0, |s, msg| s + *ptr); // compile error
```

The compiler treats every closure literal as a unique concrete type. A concrete
closure type implements `Message` only when every captured field implements
`Message`; a noncapturing closure implements `Message` through an empty
environment. A plain erased closure signature such as
`Result<S, Error> |(S, M)|` does not by itself prove `Message`, because two
closures with that signature can capture different values. A retained signature
such as `Result<S, Error> |(S, M): Message|` carries the witness explicitly.

`Message` is implemented per concrete type. `/std/message` provides ordinary
impls for primitive values, `Error`, `Result<T, E>`, and owned `/std/meta` SOP
nodes. Standard-library handle modules provide their own impls for actor
handles, channels, mutexes, atomics, and other synchronized handles.

Compiler-derived `Message` no longer applies to user structs or enums. Programs
that want structural behavior use the owned representation at the boundary:

```rust
import /std/meta as meta;

struct Event {
    i64 value;
    bool ok;
}

type EventMessage = meta::Repr<Event>;

Result<void, Error> send_event(*const Channel<EventMessage> channel, Event event) {
    return channel_send(channel, meta::into_repr(&event));
}
```

An explicit user-defined impl is still the way to make the original nominal
type itself a message type:

```rust
impl clone_message(*const Event value) {
    return Ok(*value);
}
```

Fixed-size arrays, Ciel ABI `fn` values, and concrete closure instances are not
compiler-known `Message` leaves. The compiler only normalizes fixed-size arrays
inside `meta::Repr`, provides compiler-owned marker facts for Ciel ABI function
values and concrete closure instances, and emits `into_repr` / `from_repr`
code. `/std/message` owns the ordinary impls: function values clone by copying
the Ciel ABI function pointer, concrete closures clone by converting their
capture environment through `meta::Repr<C>`, and array representation nodes
clone through `ArrayNil`, `ArrayChunk1` through `ArrayChunk16`, and
`ArrayCat<L, R>`.

Capability-erased closure values do not add an actor-specific exception: they
retain whichever capability witnesses the source type already proved. Raw
pointers, slices, dynamic interface values, plain erased closure signatures,
extern C function pointers, and opaque C handles do not implement `Message`
without an explicit policy.

The current actor runtime is backed by pthreads. `spawn_actor` clones the
initial state and handler, creates a runtime mailbox, and starts a worker thread
attached to the GC. `send` clones the payload before enqueueing it. `join`
closes the mailbox, drains queued messages, waits for the worker, and returns a
standard boxed `code_error(...)` error on runtime failure.

Resource wrappers define their own policy. `/std/io::File` is actor-local by
default. A wrapper that crosses actors implements `Message` by explicitly
duplicating, reconnecting, or otherwise constructing an independent receiver
value.

Shared mutable identity is represented through synchronized handle types:

```rust
struct Channel<T> { *void handle; }
struct Atomic<T> { *void handle; }
struct Actor<M> { *void handle; }
```

Their safe APIs expose operations:

```rust
Result<void, Error> channel_send<T: Message>(*const Channel<T> ch, T value);
Result<T, Error> channel_recv<T: Message>(*const Channel<T> ch);

Result<T, Error> atomic_load<T: AtomicValue>(*const Atomic<T> value, MemoryOrder order);
Result<void, Error> atomic_store<T: AtomicValue>(
    *const Atomic<T> value,
    T next,
    MemoryOrder order
);
```

Handles can implement `Message` when copying the handle is safe and
intentional. The implementation is responsible for synchronization and
lifetime rooting.

Mutexes are a low-level library feature. The safe mutex API uses value
replacement:

```rust
struct Mutex<T> {
    *void handle;
}

struct Update<T, R> {
    T value;
    R result;
}

interface<F, T, R> Result<Update<T, R>, Error> update_value(*const F f, T value);

Result<R, Error> mutex_update<T, F, R>(*const Mutex<T> mutex, *const F f);
```

`mutex_update` takes the current value, calls `update_value`, stores the
replacement value, unlocks, and returns the result. Implementations may
optimize the storage path internally, but the safe API exposes value
replacement rather than a borrowed interior pointer.

The actor model uses interfaces for capability classification:

```rust
interface<T> Result<T, Error> clone_message(*const T value);
interface<T> bool share_handle_marker(*const T value);
interface<T> bool thread_local_marker(*const T value);

interface Message = clone_message;
interface ShareHandle = share_handle_marker;
interface ThreadLocal = thread_local_marker;
```

Examples:

```rust
Result<void, Error> send<T: Message>(*const Actor<T> actor, T value);
Result<void, Error> accept_handle<T: ShareHandle>(T handle);
void local_resource<T: ThreadLocal>(*const T value);
```

Negative constraints remain useful for APIs that require a type to stay
actor-local:

```rust
void bind_local<T: !Message>(*const T value);
```

C interop is a trusted boundary. C wrappers decide which C-backed values are
messageable, shareable handles, or actor-local resources. Opaque C handles
start as `ThreadLocal`; wrappers implement `Message` by explicitly
duplicating, reconnecting, or otherwise constructing an independent receiver
value; wrappers implement `ShareHandle` only when operations are internally
synchronized or immutable.

The compiler recognizes canonical `/std/message` interface names only for
generic constraint lookup, retained closure witnesses, coherence checks, and
code generation of calls to selected ordinary impls. It does not synthesize
`clone_message` implementations, infer `Message`, `ShareHandle`, or
`ThreadLocal` policy from type structure, or emit policy-specific fallback
diagnostics. Structural message behavior is proved through the ordinary
`/std/message` impls for owned `/std/meta` SOP nodes; failures surface as the
normal missing `clone_message` or `Message` constraint.

The compiler work is intentionally small and generic. `T: Message` is checked by
the existing interface-constraint machinery. Monomorphized code calls ordinary
`clone_message` functions where the standard library writes those calls.
Whole-program coherence rejects duplicate concrete `clone_message` impls and
ambiguous generic marker impls.

Concrete closure value layouts used by the C backend expose the call entry and
environment pointer. `clone_message` for a concrete closure is ordinary
`/std/message` code: it reflects the concrete closure into `meta::Repr<C>`,
clones that representation through the SOP impls, then reconstructs the
closure. Retained closure signature values use their stored capability witness
when the source value already proved `Message`. Erased closure signature values
are callable values, but they do not carry enough static type information to
satisfy `Message` by default.

Runtime-backed generic APIs need code generation help only where C cannot
express monomorphized Ciel types directly. `/std/meta` exposes
`type_size<T>()` and `type_align<T>()`; the compiler lowers those helpers to C
`sizeof(T)` and `CIEL_ALIGNOF(T)`. Standard-library modules such as
`/std/channel` and `/std/sync` pass that metadata to thin runtime hooks from
ordinary Ciel code. Actor spawning additionally generates dispatch thunks that
let the runtime call concrete handlers as `Result<S, Error>(S, M)` and store the
next actor state. The safety check remains ordinary `Message` conversion, not
an actor-only type-system rule.

For safe Ciel code, actor-local mutable data is reachable only from its actor,
cross-actor APIs call `clone_message`, conversion returns a receiver-owned value
or fails, and shared handles expose synchronized operations instead of interior
mutable pointers. This guarantee depends on correct compiler checks, correct
standard-library implementations, and trusted C wrappers honoring their declared
policies.

## 17. Standard Library Boundary

The compiler treats the standard library as ordinary Ciel source except for the
generic `/std/meta` helpers and runtime hooks explicitly named in this
specification. `Error`, `must`, `expect`, `Message`, actor handles, channels,
atomics, and synchronization handles are library surface, not syntax. The `?`
operator is syntax, and it recognizes the `Result` type exported by
`/std/result` when it is visible through a direct import or a re-export such as
`/std/lib`.

There is no prelude. Every standard-library module must be imported explicitly:

```rust
import /std/result;
import /std/panic;
import /std/io;
import /std/message;
import /std/actor;
```

`/std/lib` is the standard facade module. It re-exports `/std/error`,
`/std/result`, `/std/panic`, `/std/c`, and `/std/io`. It is still imported
explicitly like any other file. The concurrency modules are imported directly.

String literals have compiler support because each occurrence emits
program-lifetime static NUL-terminated bytes and constructs a `[]const char`
slice:

```rust
[]const char name = "ciel";
usize n = name.len;
*const char raw = name.ptr;
```

The core standard library is organized around small implementation modules and
stable facade modules:

```rust
// /std/error
export import /std/result/core;
export import /std/error/core;
export import /std/error/basic;
export import /std/error/context;
```

```rust
// /std/error/core
export interface<T> []const char format_error(*const T error);
export interface ErrorTrait = format_error;

export struct Error {
    ErrorTrait value;
    []const char context;
    ?*const Error source;
}

export Error error_box(ErrorTrait error);
export Error error_with_context(Error source, []const char context);
export []const char error_message(*const Error error);
```

```rust
// /std/error/basic
export struct TextError {
    []const char text;
}

export struct CodeError {
    i64 code;
}

export Error text_error([]const char text);
export Error code_error(i64 code);
```

```rust
// /std/error/context
export Result<T, Error> error_context<T, E: ErrorTrait>(
    Result<T, E> result,
    []const char context,
);
export Result<void, Error> error_context_void<E: ErrorTrait>(
    Result<void, E> result,
    []const char context,
);
```

```rust
// /std/result/core
export enum Result<T, E> {
    Ok(T),
    Err(E),
}
```

```rust
// /std/result
export import /std/result/core;
export import /std/error;

export T must<T, E>(Result<T, E> value);
export T expect<T, E>(Result<T, E> value, []const char message);
```

```rust
// /std/format
export import /std/format/number;
```

```rust
// /std/panic
extern "C" {
    noescape never ciel_panic(*const char message, usize len);
}

export never panic([]const char message) {
    ciel_panic(message.ptr, message.len);
}
```

```rust
// /std/c
#c_include "stddef.h"
#c_include "stdint.h"

export extern "C" {
    type c_int = "int";
    type c_long = "long";
    type c_size_t = "size_t";
    type c_ptrdiff_t = "ptrdiff_t";
    type c_intptr_t = "intptr_t";
    type c_uintptr_t = "uintptr_t";
}

export type c_string = *char;
export type const_c_string = *const char;
```

```rust
// /std/io
export import /std/result;
import /std/c as c;

export struct Fd {
    c::c_int raw;
}

export enum OpenMode {
    Read,
    Write,
    Append,
}

export interface<T> []const char to_string(*const T value);
export interface printable = to_string;

export Fd stdin();
export Fd stdout();
export Fd stderr();
export Fd from_raw_fd(c::c_int raw);
export c::c_int raw_fd(Fd fd);

export Error last_error();

export Result<Fd, Error> open([]const char path, OpenMode mode);
export Result<Fd, Error> open_read([]const char path);
export Result<Fd, Error> create([]const char path);
export Result<Fd, Error> append([]const char path);
export Result<void, Error> close(Fd fd);

export Result<usize, Error> read(Fd fd, []u8 out);
export Result<usize, Error> write(Fd fd, []const u8 data);
export Result<void, Error> write_all(Fd fd, []const u8 data);
export Result<void, Error> write_text(Fd fd, []const char text);

export Result<void, Error> write_value<T: printable>(Fd fd, T value);
export Result<void, Error> print_value<T: printable>(T value);
export Result<void, Error> println_value<T: printable>(T value);
export Result<void, Error> eprint_value<T: printable>(T value);
export Result<void, Error> eprintln_value<T: printable>(T value);

export Result<void, Error> write_format(Fd fd, []const char fmt, []printable values);
export Result<void, Error> print([]const char fmt, []printable values);
export Result<void, Error> println([]const char fmt, []printable values);
export Result<void, Error> eprint([]const char fmt, []printable values);
export Result<void, Error> eprintln([]const char fmt, []printable values);
```

`/std/io` is POSIX-limited in this compiler slice. It uses file descriptor
numbers directly for `stdin`, `stdout`, and `stderr`; `read`, `write`, and
`close` are direct POSIX calls. Opening files uses tiny runtime hooks so the C
backend can use target C macros such as `O_CREAT` without hard-coding platform
flag values in Ciel source. `open` copies the `[]const char` path into a
NUL-terminated GC allocation before calling the host `open`. Printable values
are values that implement `to_string`; printing functions convert values to
`[]const char` first, then write the resulting slice to the selected descriptor.
Formatted printing uses `{}` placeholders and a `[]printable` slice, so callers
can pass heterogeneous printable values through dynamic interface erasure:

```rust
print("{} = {}", ["answer", 42 as usize]);
```

```rust
// /std/message
import /std/result;
import /std/meta as meta;

export interface<T> Result<T, Error> clone_message(*const T value);
export interface<T> bool share_handle_marker(*const T value);
export interface<T> bool thread_local_marker(*const T value);

export interface Message = clone_message;
export interface ShareHandle = share_handle_marker;
export interface ThreadLocal = thread_local_marker;
```

```rust
// /std/meta
export usize type_size<T>();
export usize type_align<T>();

export struct RefRepr<T> {}
export struct Repr<T> {}

export interface<T> bool ciel_fn_value_marker(*const T value);
export interface CielFnValue = ciel_fn_value_marker;

export interface<T> bool closure_value_marker(*const T value);
export interface ClosureValue = closure_value_marker;

export struct FieldRef<T> {
    []const char name;
    *const T value;
}

export struct Field<T> {
    []const char name;
    T value;
}

export struct PayloadRef<T> {
    usize index;
    *const T value;
}

export struct Payload<T> {
    usize index;
    T value;
}

export struct VariantRef<P> {
    []const char name;
    P payload;
}

export struct Variant<P> {
    []const char name;
    P payload;
}

export RefRepr<T> as_ref_repr<T>(*const T value);
export Repr<T> into_repr<T>(*const T value);
export T from_repr<T>(Repr<T> value);
```

```rust
// /std/actor
import /std/result;
import /std/message;

export struct Actor<M> {
    *void handle;
}

export Result<Actor<M>, Error> spawn_actor<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
export Result<void, Error> send<T: Message>(*const Actor<T> actor, T value);
```

```rust
// /std/channel
import /std/result;
import /std/message;
import /std/meta;

export struct Channel<T> {
    *void handle;
}

export Result<Channel<T>, Error> make_channel<T: Message>();
export Result<void, Error> channel_send<T: Message>(*const Channel<T> ch, T value);
export Result<T, Error> channel_recv<T: Message>(*const Channel<T> ch);
export Result<void, Error> channel_close<T: Message>(*const Channel<T> ch);
```

```rust
// /std/atomic
export enum MemoryOrder {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}

export struct Atomic<T> {
    *void handle;
}

export struct CompareExchange<T> {
    bool exchanged;
    T previous;
}

export interface<T> bool atomic_value_marker(*const T value);
export interface AtomicValue = atomic_value_marker;

export interface<T> bool atomic_integer_marker(*const T value);
export interface AtomicInteger = atomic_integer_marker;

export Result<Atomic<T>, Error> make_atomic<T: AtomicValue>(T initial);
export Result<T, Error> atomic_load<T: AtomicValue>(
    *const Atomic<T> atomic,
    MemoryOrder order
);
export Result<void, Error> atomic_store<T: AtomicValue>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
);
export Result<T, Error> atomic_exchange<T: AtomicValue>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
);
export Result<CompareExchange<T>, Error> atomic_compare_exchange<T: AtomicValue>(
    *const Atomic<T> atomic,
    T expected,
    T desired,
    MemoryOrder success,
    MemoryOrder failure
);
export Result<T, Error> atomic_fetch_add<T: AtomicInteger>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
);
export Result<T, Error> atomic_fetch_sub<T: AtomicInteger>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
);
```

```rust
// /std/sync
import /std/result;
import /std/meta;

export struct Mutex<T> {
    *void handle;
}

export struct Update<T, R> {
    T value;
    R result;
}

export interface<F, T, R> Result<Update<T, R>, Error> update_value(
    *const F f,
    T value
);

export Result<R, Error> mutex_update<T, F, R>(*const Mutex<T> mutex, *const F f);
```

```rust
// /std/lib
export import /std/error;
export import /std/result;
export import /std/panic;
export import /std/c;
export import /std/io;
export import /std/meta;
export import /std/actor;
export import /std/channel;
export import /std/sync;
export import /std/atomic;
```

These modules are standard library API. They are not compiler intrinsics except
where this specification names `/std/meta` type metadata helpers or a runtime
hook.

## 18. C Interop and ABI

`extern "C"` declarations are C ABI declarations. C APIs require explicit
pointer nullability and view mutability: users write `*T`, `*const T`, `?*T`,
and `?*const T`. Standalone `const T` is not a Ciel source type. Ciel specifies
`extern "C"` as its C ABI spelling. C ABI callable types are named C ABI
functions and `extern "C" ... fn(...)` function-pointer types. Closure values
use the Ciel ABI.

A top-level `extern "C" T name(...);` declares an external C symbol. A
top-level `export extern "C" T name(...) { ... }` defines a C ABI symbol
implemented in Ciel. `export extern "C" { ... }` re-exports imported C
declarations to Ciel importers; it does not define the C symbols. `noescape`
is allowed only on imported `extern "C"` function declarations inside an
extern block. `extern "C"` functions may return `never`; Ciel lowers that C ABI
return type as `void` while treating calls as non-fallthrough.

A top-level `export extern "C" type name = "C spelling";` declares a C
spelling type. Inside an `extern "C"` block the ABI is inherited, so
`type name = "C spelling";` has the same meaning. The spelling string is
emitted as the C declaration spelling for that type. This is how `/std/c`
exposes prefixed public types such as `c_int`, `c_long`, `c_size_t`, and
`c_ssize_t` without assuming that they are identical to Ciel fixed-width
primitives.

Inside an `extern "C"` block, the block ABI applies to declared functions and
to nested function types in those declarations or type aliases unless a nested
function type has an explicit ABI.

```rust
extern "C" {
    opaque struct FILE;

    ?*FILE fopen(*const char filename, *const char mode);
    i32 fclose(*FILE stream);
    i32 fputs(*const char str, *FILE stream);
    void free(?*void ptr);
}
```

Ciel models only caller-visible pointer mutability at the C boundary:

```text
*T         => T *
*const T   => const T *
?*T        => T *
?*const T  => const T *
```

C top-level `const` on a by-value parameter or on the pointer value itself is
not part of the caller-visible Ciel function type. Pointee `const` is
preserved because it controls whether the callee may write through the
argument:

```c
void f(const int value);      // Ciel: void f(i32 value)
void g(char * const buffer);  // Ciel: void g(*char buffer)
void h(const char * const s); // Ciel: void h(*const char s)
```

Only pointee `const` is preserved because it changes what the callee may write
through the argument:

```c
void takes_mut(char *buffer);      // Ciel: void takes_mut(*char buffer)
void takes_ro(const char *buffer); // Ciel: void takes_ro(*const char buffer)
```

Calls obey the Ciel declaration exactly. A writable view may weaken to a
read-only C parameter, but a read-only view cannot satisfy a writable C
parameter:

```rust
extern "C" {
    void read_only(*const char s);
    void may_write(*char s);
}

[]const char text = "hello";
read_only(text.ptr); // ok
may_write(text.ptr); // error
```

Generated C may normalize top-level `const` spelling at ABI boundaries after
Ciel type checking, but it must preserve pointee `const` in prototypes and must
not create a source-level conversion from `*const T` to `*T` or from
`[]const T` to `[]T`.

If a legacy C API accepts `void *` for data it only reads, the binding should
use a C shim or a corrected declaration that exposes `*const void` to Ciel.
The ordinary Ciel call path must not insert a `*const T` to `*T` cast. For rare
C declarations where exact spelling matters but no Ciel semantics are needed,
users should keep using C spelling aliases:

```rust
extern "C" type CHandle = "const struct CHandle";
```

For exported Ciel functions, generated prototypes preserve pointee `const`:

```rust
export extern "C" void inspect(*const Packet packet) { ... }
export extern "C" void mutate(*Packet packet) { ... }
```

```c
void inspect(const Packet *packet);
void mutate(Packet *packet);
```

Top-level `const` on C parameters may appear in a user-written C header, but
Ciel does not need to reproduce it in generated definitions. Pointee `const`
must match; otherwise the C and Ciel declarations describe different write
permissions.

```rust
import /std/c as c;

#c_include "unistd.h"

extern "C" {
    c::c_ssize_t write(c::c_int fd, *const void buf, c::c_size_t count);
}
```

Function type ABI is explicit:

```rust
i32 fn(i64)                    // Ciel ABI
extern "C" i32 fn(*void, *void) // C ABI
```

The Ciel internal ABI may lower large returns and arguments using hidden
pointers. Any declaration marked `extern "C"` or `export extern "C"` must obey
the target platform C ABI as written. By-value `void` parameters are invalid in
`extern "C"` declarations; an empty C parameter list is written by omitting
parameters.

Generated Ciel libraries expose a small host ABI:

```rust
extern "C" {
    opaque struct CielRoot;

    void ciel_runtime_init();
    i32 ciel_thread_attach();
    void ciel_thread_detach();

    *CielRoot ciel_root_pin(*void value);
    *void ciel_root_get(*CielRoot root);
    void ciel_root_unpin(*CielRoot root);
}
```

`ciel_runtime_init` initializes the GC and enables external thread
registration. It is idempotent. Generated executables call it before user
`main`. Shared libraries also emit an internal constructor or target-equivalent
initializer:

```c
__attribute__((constructor))
static void ciel_internal_init(void) {
    ciel_runtime_init();
}
```

Host-created threads must call `ciel_thread_attach` before calling Ciel or
holding Ciel GC pointers, and must detach only after they no longer hold such
pointers. C `malloc` memory is not scanned by the GC; C code that stores Ciel
GC pointers must use `CielRoot` or another explicit root mechanism.

`ciel_thread_attach` returns `0` on success and a nonzero value on failure.

## 19. Debug Information

A debug build emits target debug information through the generated C compiler.
The Ciel compiler:

1. preserves generated C files when requested
2. passes the target compiler's debug flag such as `-g`
3. emits `#line` directives mapping generated C back to Ciel source files
4. uses deterministic mangled names for generated C symbols
5. keeps a source-location table for runtime diagnostics such as panic messages

The minimum debug contract is source-line mapping, readable panic locations,
and deterministic generated names.

## 20. C Backend Lowering

Ciel keeps source-level value semantics. The generated C ABI for internal Ciel
functions may avoid large copies:

```rust
BigResult parse_big(*const char text);
void consume_big(BigResult value);
```

may lower internally to:

```c
void parse_big(BigResult *out, const char *text);
void consume_big(const BigResult *value);
```

Closure values lower to generated environment layouts plus call thunks. The
compiler may allocate an environment on the stack when it proves the closure
does not escape; otherwise the environment is GC-managed. Noncapturing
closures used as `fn` values lower directly to generated helper functions
without an environment pointer.

Generated C first emits requested `#c_include` directives and runtime includes.
Then it is printed in dependency-safe phases:

1. typedefs and struct/enum forward declarations
2. struct/enum layout definitions
3. function prototypes
4. function bodies

The generated C does not depend on source declaration order.
