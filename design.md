# Ciel Specification

This document is the normative specification for Ciel. Ciel compiles whole
programs to a single generated C translation unit, then invokes the target
system C compiler. The runtime uses BDWGC/libgc and a libdispatch-backed actor
and async I/O runtime on supported targets.

## 1. Language Model

Ciel is a whole-program, ahead-of-time compiled language with C interop as a
core goal. The source program is resolved as a closed set of imported files,
checked, lowered to one generated C translation unit, and then compiled by the
target C compiler.

Ciel is garbage-collected. Local values do not expose stack or heap placement;
the compiler chooses storage and promotes values to the GC heap when required
for safety. Safe Ciel prevents dangling local-address use, null dereference of
non-null pointers, unchecked enum pattern omissions, and unsafe C ABI mismatch
inside Ciel declarations. Safe concurrency is async/await-first and
actor-backed: ordinary mutable objects are task-local or actor-local,
cross-domain communication uses explicit or hidden `Message` obligations, and
shared identity is exposed only through synchronized handles.
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
true type unsafe void while
```

`fn` is not reserved. It is a contextual token recognized only while parsing a
function-pointer type suffix.

`async`, `await`, `select`, and `biased select` are contextual syntax. They are
recognized only in async function declarations, async closure expressions,
await expressions, and select expressions. They remain ordinary identifiers in
module paths and other positions, so `/std/async` is a valid import path.

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

### Package Manifests

The compiler understands versioned `ciel.toml` manifests. Manifest version `1`
declares package identity, project entries, explicit public Ciel exports, and
optional package-owned CMake native targets:

```toml
manifest_version = 1

[package]
name = "sqlite"
kind = "library"
root = "."

[ciel.exports]
"/sqlite" = "sqlite.ciel"

[[native.cmake]]
path = "CMakeLists.txt"
target = "ciel_lib_sqlite"
when = { os = ["linux", "macos"] }
```

`package.kind` is one of `project`, `stdlib`, or `library`. `project` marks an
entry project rooted at `package.root`. A project manifest must contain a
`[project.entries]` table mapping entry names to `.ciel` source files relative
to `package.root`; `project.default` may name the entry used when the CLI does
not pass `--entry`. Project entries are compile entry points, not import
exports, and project modules should be loaded through relative imports. Project
manifests do not use `[ciel.exports]`. Manifest version `1` does not define test
metadata or workspace membership. `package.root` is relative to the manifest file
and defaults to `"."`. In `stdlib` and `library` manifests, `[ciel.exports]`
maps absolute import paths to `.ciel` source files relative to `package.root`.
Export paths and manifest paths are validated; paths must not escape the package
root.

Standard-library manifests are loaded from compiler standard-library search
roots. Project builds load the entry project manifest from
`--manifest-path <path/to/ciel.toml>` or, when no input file is passed, by
searching the current directory and its parents for `ciel.toml`. Passing an
input file compiles that source directly; project metadata is loaded only when a
project manifest is selected explicitly or discovered for an entry build. User
package roots are passed explicitly with repeated `--package-root <root>`
arguments. User package-root scanning accepts only library manifests.

Absolute imports first resolve through standard-library package exports, then
through user package-root exports, and finally through the legacy standard-path
file fallback. This preserves `/std/...` ownership by compiler-shipped packages
while allowing repository-local library packages such as `/sqlite`.

Executable and shared-library builds link generated C through a generated
top-level CMake project. The build plan includes the fixed runtime CMake
target, imported entry-project targets, imported standard-library package
targets, and imported user package targets. CMake targets loaded from user
package roots are not executed unless the driver is invoked with
`--allow-native-build`; entry-project targets do not require that third-party
allow flag. The driver passes the selected Ciel build profile to CMake as both
`CMAKE_BUILD_TYPE`/`--config` and `CIEL_BUILD_PROFILE`.

## 5. Names and Scopes

Ciel has a single namespace for values, functions, types, enum variants,
interfaces, and aliases. Function overloading is forbidden. Two visible
bare declarations with the same name are an error only when a bare use is
ambiguous. Names inside aliased imports are not bare declarations.

Lexical declarations shadow outer declarations from their declaration point
forward. Each block introduces a new lexical scope. This includes local
declarations shadowing imported symbols.

Variables declared in a `for` initializer are scoped to that `for` statement.

```ciel
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
Type            ::= [ "unsafe" ] [ AbiSpec ] PrefixType { CallableSuffix }
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

```ciel
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

```ciel
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

```ciel
_ handler = |State<i64> state, Command<i64> command| handle(state, command);
Actor<_> actor = must(spawn_actor_cloned<State<i64>, Command<i64>>(initial, handler));
Result<Actor<_>, Error> pending =
    spawn_actor_cloned<State<i64>, Command<i64>>(initial, handler);
[]_ values = [1, 2, 3];
[3]_ fixed = [1, 2, 3];
```

Every hole is solved from the declaration initializer while type checking that
declaration. The solved concrete type is stored on the local before later
assignments, monomorphization, and code generation. Holes do not infer from
later uses:

```ciel
_ value = 1;  // i64
value = 2;    // ok
value = 2.0;  // error: expected i64

_ ptr = null;  // error: null needs an expected nullable pointer type
?*i64 ok = null;
```

Partial annotations provide context, but expressions that already require an
expected type still require one:

```ciel
_ point = { x: 1, y: 2 }; // error: struct literal needs a struct type
_ empty = [];             // error: empty array literal has no element type

Point point = { x: 1, y: 2 };
[]i64 empty = [];
```

A fully typed closure can infer a concrete compiler-created closure type. An
untyped closure still needs an expected callable type, and `_` alone does not
infer block-bodied closure return types:

```ciel
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

```ciel
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

```ciel
*i32 fn(i64)       // function returning *i32
?*i32 fn(i64)      // function returning ?*i32
?*(i32 fn(i64))    // nullable reference to a function type
```

Repeated `fn` suffixes construct functions returning functions:

```ciel
i32 fn(i64) fn(*void) // takes *void, returns i32 fn(i64)
```

Complex function types can always be written directly. Aliases give them
stable names:

```ciel
void fn(i32) fn(i32, void fn(i32)) signal;

type SignalHandler = void fn(i32);
SignalHandler fn(i32, SignalHandler) signal2;
```

If no `extern "C"` ABI is written and no enclosing `extern "C"` block applies,
a function type uses the Ciel ABI. The Ciel ABI is an implementation detail.
`extern "C" T fn(...)` uses the target platform C ABI.
`unsafe T fn(...)` is a function-pointer type whose calls require
`unsafe { ... }`.

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

```ciel
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

```ciel
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

```ciel
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

StructDecl          ::= [ "unsafe" ] "struct" Identifier [ GenericParamList ] StructBody
StructBody          ::= "{" { FieldDecl } "}"
FieldDecl           ::= Type Identifier ";"

EnumDecl            ::= "enum" Identifier [ GenericParamList ] EnumBody
EnumBody            ::= "{" [ VariantDecl { "," VariantDecl } [ "," ] ] "}"
VariantDecl         ::= Identifier [ "(" TypeList ")" ]

InterfaceDecl       ::= [ "unsafe" ] "interface" GenericParamList
                        InterfaceSignature ";"
InterfaceSignature  ::= Type Identifier "(" [ ParamList ] ")"
InterfaceAliasDecl  ::= "interface" Identifier "=" InterfaceExpr ";"

InterfaceExpr       ::= InterfaceTerm { ( "+" | "-" ) InterfaceTerm }
InterfaceTerm       ::= [ "!" ] Identifier [ TypeArgList ]

ImplDecl            ::= [ "unsafe" ] "impl" [ GenericParamList ] Identifier [ TypeArgList ]
                        "(" [ ParamList ] ")" Block

FunctionDecl        ::= [ "unsafe" ] [ AbiSpec ] [ "async" ] FunctionSignature
                        ( Block | ";" )
FunctionSignature   ::= Type Identifier [ GenericParamList ]
                        "(" [ ParamList ] ")"

GenericParamList    ::= "<" GenericParam { "," GenericParam } [ "," ] ">"
GenericParam        ::= Identifier [ ":" ConstraintExpr ]
ConstraintExpr      ::= ConstraintTerm { ( "+" | "-" ) ConstraintTerm }
ConstraintTerm      ::= [ "!" ] Identifier [ TypeArgList ]

ParamList           ::= Param { "," Param } [ "," ]
Param               ::= Type BindingName
BindingName         ::= [ "@" ] Identifier

ExternBlock         ::= [ "unsafe" ] "extern" StringLiteral "{" { ExternItem } "}"
ExternItem          ::= OpaqueStructDecl
                     | [ "noescape" ] FunctionSignature ";"
                     | TypeAliasDecl
                     | CSpellingTypeDecl
OpaqueStructDecl    ::= "opaque" "struct" Identifier ";"
```

Local variables and function parameters are declared with type syntax.
`BindingName` controls whether the binding may be assigned again:

```ciel
i64 value = 1;      // immutable binding
i64 @count = 0;     // mutable binding

void step(i64 input, i64 @state) {
    state = state + input;
}
```

A binding without `@` is immutable after initialization. A binding with `@`
may be assigned repeatedly. `@` belongs to the binding name, not to the type;
a mutable binding may hold a read-only pointer or slice view.

```ciel
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

```ciel
i64 x;
if (cond) {
    x = 1;
} else {
    x = 2;
}

x = 3; // error: x is already initialized
```

Partial writes cannot initialize an immutable aggregate:

```ciel
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

```ciel
_ value = make_value(); // ok
_ value;                // error
_ @value;               // error
```

Struct declarations do not define default field values. A struct value is
created by a named-field struct literal, by copying another value, by a
function return, or by C interop according to the declared ABI.
An `unsafe struct` is still copied and passed like an ordinary struct, but
constructing it with a struct literal or projecting one of its fields requires
`unsafe { ... }`.

At most one function body may exist for a given fully qualified name. A
non-`extern` function declaration ending in `;` is a prototype and must match
the eventual body exactly. `extern "C"` declarations do not require a Ciel body.

An `async` function is declared by writing `async` before the ordinary return
type:

```ciel
async Result<Bytes, Error> read_frame(AsyncTcpStream stream) {
    Bytes header = await read_exact(stream, 8)?;
    usize len = decode_len(header)?;
    return await read_exact(stream, len);
}
```

The written return type is the value produced when the function is awaited.
Calling an async function creates a first-class future whose concrete type is
compiler-generated and opaque. That generated type implements the standard
`Future<Out>`/`Awaitable<Out>` surface for the function's written output type.
Async functions may be declared or prototyped like ordinary Ciel functions, but
they cannot use a C ABI; exporting or importing an async `extern "C"` function
is rejected.

`unsafe` on a function makes calls to that function require an unsafe block.
The function body is still ordinary checked code; unsafe operations inside it
must appear in `unsafe { ... }`.

Imported C functions must be declared through an unsafe C boundary, normally an
`unsafe extern "C"` block. C spelling type aliases and opaque declarations may
remain in safe `extern "C"` blocks because they do not create callable unsafe
operations.

Recursive layout is checked through storage edges. A struct field or enum
payload stored by value continues layout expansion. A fixed-size array
continues layout expansion through its element type. Pointer edges stop layout
expansion: `*T`, `*const T`, `?*T`, and `?*const T` may refer back to the
containing aggregate. Slice, function, closure, and dynamic interface values
are finite handles or descriptors and do not expand the pointed-to or callable
shape for aggregate layout.

If layout reaches the same concrete struct or enum instance again through only
by-value storage edges, the program is rejected:

```ciel
struct Node {
    i64 data;
    ?*Node next; // ok: pointer edge cuts the cycle
}

struct BadNode {
    i64 data;
    BadNode next; // error
}

enum List {
    Cons(i64, List), // error
    Nil,
}
```

Generic aggregate layout depends only on substituted storage types. An unused
type parameter does not force expansion:

```ciel
struct Wrapper<T> {
    i64 tag;
}

struct Outer {
    Wrapper<Outer> inner; // ok: Wrapper<T> stores no T
}
```

If substitution leaves a by-value cycle, the concrete instance is rejected:

```ciel
struct Box<T> {
    T value;
}

struct Outer {
    Box<Outer> inner; // error
}
```

Layout validity is separate from policy surfaces such as `Message` and
structural metaprogramming. A recursive pointer graph has finite in-memory
layout, but it does not automatically become a safe cross-actor clone format.

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
                 | AwaitExpr
                 | SelectExpr
                 | UnsafeBlockExpr
                 | "(" Expr ")"

UnsafeBlockExpr ::= "unsafe" "{" { Statement } [ Expr ] "}"

AwaitExpr       ::= "await" PostfixExpr
SelectExpr      ::= [ "biased" ] "select" "{" SelectArm { SelectArm } "}"
SelectArm       ::= "case" Identifier "=" Expr ":" Expr [ ";" ]

QualifiedName   ::= Identifier "::" Identifier { "::" Identifier }
Literal         ::= IntegerLiteral | FloatLiteral | CharLiteral
                 | StringLiteral | BoolLiteral | NullLiteral

StructLiteral   ::= "{" [ FieldInit { "," FieldInit } [ "," ] ] "}"
FieldInit       ::= Identifier ":" Expr
ArrayLiteral    ::= "[" [ Expr { "," Expr } [ "," ] | Expr ";" [ IntegerLiteral ] ] "]"

ClosureExpr     ::= [ "async" ] ClosureIntro ClosureBody
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

```ciel
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

`unsafe { ... }` is an expression block. It permits unsafe operations inside the
block, creates a nested local scope, and evaluates to the final expression when
one is present or to `void` otherwise. Ordinary type checking, control-flow
checking, escape analysis, and interface constraints still apply inside the
block.

Closure parameters may write either `BindingName` or `Type BindingName`. If a
parameter type is omitted, it must be supplied by an expected callable type.
Expected callable types come from assignment, parameter passing, return
context, or an explicit `as` type annotation. If no expected callable type
exists, every closure parameter must write its type. Closure parameters use the
same `@` mutability rule as function parameters:

```ciel
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

```ciel
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

An async closure is written by prefixing a closure literal with `async`:

```ciel
async || work()
async |usize value| {
    return compute(value);
}
```

An async closure body uses the same async rules as an async function body. A
direct async closure passed to `async::spawn` is checked by the compiler as a
task boundary: result values and captured values that cross into the spawned
task must satisfy the hidden `Message` obligations described in Section 16. The
closure does not need to be manually retained as a `: Message` closure unless
the surrounding API requires an ordinary messageable closure value.

A call suffix may call a function item, function-pointer value, or closure
value. Closure arguments are evaluated in source order, then the closure's
generated call function is invoked with its environment and the arguments.

Calling an async function or async closure produces a future value immediately;
it does not run the body to completion at the call site. `await future` is valid
only inside an async body or inside compiler-recognized async bridges such as
`async::block_on`. The operand must implement `Awaitable<Out>`, and the await
expression has type `Out`. If `Out` is `Result<T, Error>`, ordinary `?`
propagation composes after the await:

```ciel
Bytes bytes = await socket_read(stream)?;
```

The full execution, task, cancellation, and frame-safety rules for async
functions, futures, and `await` are specified in Section 16.

`select` constructs a future that races a flat set of future expressions:

```ciel
usize result = await select {
    case bytes = reader::read_buffered(reader, 4096): handle(bytes);
    case slept = async_time::sleep_ms(100): timeout(slept);
};
```

Every arm future must produce the same selected result type through its arm
body. Fairness, cancellation-safety, timeout lowering, and selectable future
rules are specified in Section 16.

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

```ciel
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

```ciel
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

```ciel
i64 x = 1;
i64 @y = 2;

*const i64 px = &x;
*i64 py = &y;
```

Taking a writable pointer from a read-only lvalue is rejected, but taking a
read-only pointer from a writable lvalue is allowed by view weakening.

Parameters follow the same address-of rule as initialized locals. `T value` is
a read-only lvalue and `T @value` is a writable lvalue:

```ciel
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

```ciel
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

```ciel
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

Bitwise `&`, `|`, and `^` require integer operands of the same type after
literal inference and return that type. Shift `<<` and `>>` require an integer
left operand and an integer shift count; the result type is the left operand
type. `~` requires an integer operand and returns the same type. `bool`,
`char`, floats, pointers, closures, slices, structs, enums, and dynamic
interfaces are not bitwise operands without explicit casts to integer types.

```ciel
u32 mask = (1 as u32) << 5;
u8 nibble = (byte >> (4 as u8)) & (0x0f as u8);
i64 both = left ^ right;
```

Unsigned `>>` is a logical right shift. Signed `>>` is an arithmetic right
shift; supported C targets are required to provide two's-complement signed
integers with arithmetic signed right shift. Constant shift counts greater than
or equal to the left operand bit width are compile-time errors. Dynamic
out-of-range shift counts panic at runtime.

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

Pointer casts from a typed pointer to `*void` or `?*void` are safe type erasure.
Casts from `*void` or `?*void` back to a typed pointer are unsafe operations:

```ciel
i64 value = 1;
*void raw = &value as *void;
*i64 typed = unsafe { raw as *i64 };
```

The pointer and slice view constructors have only these implicit view
conversions:

```ciel
*T       -> *const T
*T       -> ?*T
*T       -> ?*const T
*const T -> ?*const T
?*T      -> ?*const T
[]T      -> []const T
```

Conversions that remove read-only view mutability are rejected, including under
`as`:

```ciel
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

```ciel
?*T p = get();
if (p != null) {
    use(p); // p is narrowed to *T inside this branch
}
```

`if (p == null) return;` narrows `p` to `*T` after the statement. Short-circuit
`&&` is supported:

```ciel
if (p != null && p->value > 0) {
    use(p);
}
```

Reassigning `p`, assigning through a pointer to `p`, or passing `&p` to code
that may write it invalidates the narrowing immediately. Fields must be copied
to locals before narrowing:

```ciel
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

An `unsafe interface` marks the implementation as a trusted safety contract.
Implementations of unsafe interfaces must write `unsafe impl`; `unsafe impl` is
rejected for safe interfaces. Using an unsafe interface alias as a generic
constraint remains safe at call sites because the obligation is discharged at
the implementation site.

Type arguments written on an `impl` also bind only non-receiver generic
parameters.

An `impl` may have its own generic parameter list. Those parameters are inferred
from the receiver and other interface arguments, then monomorphized like a
generic function:

```ciel
unsafe impl<T> clone_message(*const Actor<T> value) {
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

```ciel
interface<T> i64 measure(*const T value);
i64 call_measure(measure value);
```

```ciel
interface<T, U> bool eq(*const T value, U other);

bool check_eq(eq<i64> value, i64 target) {
    return eq(value, target);
}

bool bad_eq(eq value); // error: U is not supplied
```

`make` is a normal capability, but it is not dynamically usable because its
receiver type appears only in the return type:

```ciel
interface<T, U> Result<T, Error> make(U value);
Mutex<i64> total = must(make(0));
must(make<Mutex<i64>>(0)); // required without expected type
```

Interface aliases use `+` and `-` to form narrowed views:

```ciel
interface streaming = read - seek;
interface readable_seekable = read + seek;
```

`read - seek` masks out `seek` from the view. It does not require the concrete
type to lack `seek`.

Generic constraints may use `!capability` as a global hard rejection:

```ciel
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

```ciel
import /std/meta as meta;

meta::HNil
meta::HCons<meta::FieldRef<i64>, meta::HNil>
meta::Coproduct<meta::VariantRef<meta::HNil>, meta::CoNil>
```

`meta::RefRepr<T>` is a borrowed structural view. For a visible struct it
normalizes to an `HCons` list of `FieldRef<FieldType>` values in declaration
order:

```ciel
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

```ciel
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

```ciel
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

```ciel
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

```ciel
import /std/meta as meta;

struct Packet {
    i64 id;
    bool ok;
}

type PacketMessage = meta::Repr<Packet>;
```

`/std/message` implements `clone_message` for owned SOP nodes such as `HNil`,
`HCons`, `Field`, `CoNil`, `Coproduct`, `Variant`, and `Payload`. If a field or
payload leaf lacks `Message`, or has a capability forbidden by `Message`,
ordinary generic constraint checking rejects the representation. Code that wants
the original nominal type itself to cross an actor or channel boundary must write
an explicit `clone_message(*const T)` policy and must not mark the type with a
capability excluded by `Message`.

Owned representation recursively expands structs, enums, concrete closures, and
fixed-size arrays where no nominal policy boundary exists. A named field or
payload that already carries a nominal policy such as `clone_message`,
`ShareHandle`, or `ThreadLocal` remains a leaf. This preserves both positive and
negative capability facts through `meta::Repr<T>`. For example, a
`ThreadLocal` handle inside `meta::Repr<Event>` still blocks `Event` from
satisfying `Message`. Concrete closure instances are not opaque policy leaves;
their standard-library `clone_message` impl reflects captures through
`meta::Repr<C>`.

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

```ciel
enum DigitError {
    DigitNonDecimal,
}

return Err(DigitNonDecimal);
```

Payload variants are ordinary constructor calls:

```ciel
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

```ciel
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

```ciel
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

## 16. Async/Await

Ciel's ordinary asynchronous programming model is stackless async/await.
Programmers write async functions, futures, tasks, async channels, timeouts,
and `select`. The runtime is actor-backed, but ordinary async I/O does not
require users to name `Actor<M>`, build mailbox messages, or manually handle
operation-token completions.

### Async Functions and Futures

An async function or async closure call evaluates its callee, arguments, and
captures in ordinary source order, then constructs a future value. The call
does not block the current thread, does not run the async body to completion at
the call site, and does not create a concurrent task by itself.

The written return type of an async function is the value produced when its
future is awaited:

```ciel
async Result<Bytes, Error> read_frame(AsyncTcpStream stream) {
    Bytes header = await async_net::read(stream, 8)?;
    usize len = decode_len(header)?;
    return await async_net::read(stream, len);
}

_ future = read_frame(stream);
Bytes frame = await read_frame(stream)?;
```

The concrete future type generated for `read_frame` is opaque. It implements
`Awaitable<Result<Bytes, Error>>`, and may also implement `CancelSafe` or
`Abortable` when its body proves the corresponding contract. Users can store a
future in a local, pass it to `async::spawn`, pass it to `select`, pass it to a
generic future combinator, or await it. Users cannot name the generated frame
type, inspect its layout, or reach into another task's frame through the future.

Compiler-generated futures are single-consumer values. After a generated future
has completed, awaiting it again is invalid unless the particular future type
explicitly documents reusable await behavior. Dropping a future that has not
registered a pending operation is allowed. Dropping or cancelling a pending
future while the current task continues is allowed only through a path that has
proved `CancelSafe`; tearing down a pending future because its owning task is
terminating requires `Abortable`.

### Await

`await expr` is valid only inside an async body or inside a compiler-recognized
synchronous bridge such as `async::block_on`. The operand is evaluated exactly
once and must implement `Awaitable<Out>`. The expression has type `Out`, and
ordinary `?` propagation composes after the await:

```ciel
Bytes bytes = await async_net::read(stream, 16384)?;
```

Awaiting a ready future yields its value without parking the task. Awaiting a
pending future stores the current program counter, nested future state, and
every live frame-safe local in the task's async frame, registers a wakeup with
the runtime, and returns control to the scheduler. Suspension parks only the
current task, not the OS thread.

Resumption continues at the source point after the `await`. No native C stack
frame is preserved across suspension; all state needed after the suspension
lives in the generated async frame. Immediate completions resume through a
task-local trampoline so a chain of ready awaits cannot grow the native C stack
without bound or monopolize the executor.

`defer`, definite assignment, return-path analysis, and `?` keep their ordinary
meaning in async bodies. The async-specific rule is that initialized frame
fields and active cleanup actions must be tracked by program-counter state, so
normal return, `Err` return, panic, cancellation, and abort all run the correct
non-awaiting cleanup before the frame is released.

### Tasks

`async::spawn` starts an awaitable body as an independent task:

```ciel
Task<usize> task = async::spawn(async || compute_size(path))?;
usize size = await task?;
```

The body passed to `spawn` must be awaitable with output `Result<T, Error>` and
must be abortable. `Task<T>` is itself awaitable with output
`Result<T, Error>`: awaiting a task waits for normal completion, failure, or
cancellation of that task. Cancelling a wait on a task handle does not cancel
the running task; it unregisters the waiter. `async::cancel` requests task
termination through the task's abort path.

Spawning is a task-ownership boundary. Values captured by a directly spawned
async closure and the task result `T` must satisfy hidden `Message`
obligations, because they cross from one task owner to another. The source API
does not require users to write these bounds on ordinary calls to `spawn`; the
compiler attaches the obligations at the boundary, resolves structural
messageability when possible, and reports the failing captured value or result
field when proof fails.
When a hidden async boundary uses a structural `meta::Repr<T>` crossing path,
that fact is local to the boundary. It does not make the original nominal type
implement `Message` for explicit low-level actor APIs or for any API spelling a
public `T: Message` bound.

Values created inside the spawned async body are task-local. They do not need
to implement `Message` merely because they live across `await`; they only need
to satisfy async-frame safety. Moving an already existing non-`Message` value
from one task into a new task is not supported by the high-level safe spawn API.
Such a transfer requires an explicit synchronized handle, an owned message
representation, or a future unsafe ownership-transfer facility.

### Async Channels and Task Groups

Async tasks communicate through bounded async channels:

```ciel
ChannelPair<Bytes> ch = async::channel<Bytes>(1024)?;
Task<void> writer = async::spawn(async || write_loop(ch.receiver))?;
await async::send(ch.sender, payload)?;
await writer?;
```

`send` suspends when the channel is full, which provides backpressure. `try_send`
is the non-suspending fast path. `reserve` waits for capacity and returns a
permit; `permit_send` then commits a value synchronously. This split matters for
cancellation: waiting for capacity through `reserve` is cancellation-safe, but
dropping a pending `send(value)` may otherwise discard a value that was moved
into the send operation.

Channel payloads cross task ownership and therefore carry hidden `Message`
obligations at send and receive boundaries. Channel endpoint liveness is
deterministic: closing or destroying the last sender wakes receivers after the
buffer is drained, and closing or destroying the last receiver wakes blocked
senders and reservations with `channel_closed_error()`. Task cleanup must
release channel endpoints stored in async frames before the task is considered
finished; GC finalization is not a scheduling guarantee.

`select` handles a static set of futures known at compile time. Dynamic
concurrency uses task groups. `group_next` waits for the next task in the group
to finish and does not cancel the remaining tasks. `group_cancel_all` aborts
unfinished tasks through their task abort paths.

### Select and Timeout

`select` races a flat set of future expressions and produces one result:

```ciel
Event event = await select {
    case bytes = async_net::read_buffered(reader, 16384):
        Event::Bytes(bytes?)

    case command = async::recv(commands):
        Event::Command(command?)

    case tick = async_time::sleep_ms(5000):
        tick?;
        Event::Tick
};
```

The whole `select` expression is awaited. Arm expressions are futures, not
nested `await` expressions. Each arm binds the completed arm value and evaluates
an arm body whose result must be assignable to the common `select` result type.
`?` inside an arm propagates from the enclosing async function.

Every arm future must implement `SelectableFuture<ArmOut>`, which is
`Awaitable<ArmOut> + CancelSafe + Abortable`. The compiler and stdlib lower a
select expression to an internal select-set future that polls every arm once
before parking, so ready buffered data, completed tasks, channel messages, and
expired timers cannot be missed. Default `select` chooses fairly among all
ready arms; `biased select` is the explicit source-order priority form. Losing
futures are cancelled only after their `CancelSafe` contract permits it.

`async::timeout(future, ms)` is a convenience wrapper over the same model. It
races the operand with a timer and, on timeout, cancels only the waiting future.
It does not assume that an arbitrary protocol future can discard partial state.

### Cancel and Abort

Cancellation and abort are distinct operations:

1. Cancel abandons one pending future while the current task continues. This is
   what happens to losing futures in `select` and `timeout`.
2. Abort terminates the owning task's current suspended operation because the
   task is ending through cancellation, panic, or runtime teardown.

`CancelSafe` is a behavioral promise that cancelling a pending future cannot
lose user-visible data, corrupt protocol state, or hide a side effect in a
resource that remains usable. It is not derived merely because every awaited
operation inside a future is itself `CancelSafe`; multi-await protocol code can
consume state before a later suspension.

`Abortable` is a behavioral promise that the runtime can tear down the current
pending operation and run task cleanup in bounded time. An abort path may close
a socket, deregister a timer, poison a handle, or make a resource unusable, as
long as later aliases observe a defined error instead of unsynchronized state.

Raw TCP reads, reusable-buffer reads, and writes are abortable but not
cancellation-safe by default: abort may close the stream to release the task,
but a losing race must not silently discard bytes, lose an owned buffer, or hide
a partial write while the task continues. Buffered reads, timers, connect,
accept, task waits, channel receives, and channel reservations can be selectable
when their stdlib contracts preserve state.

External callbacks must never capture async frame pointers, task-state
pointers, or pointers into user frame storage. A runtime operation token owns
callback-visible result storage and contains routing data such as actor mailbox
id, task id, operation id, and generation. Callback completion enqueues a hidden
resume event. The actor-backed resume dispatcher validates the ids and
generation before resuming a task; stale events clean up operation-owned
storage and do not touch the released async frame.

### Frame Safety and Boundary Policy

Async-frame safety is separate from `Message`. `Message` is required when a
value crosses task ownership or enters a low-level actor mailbox:

1. values captured by a spawned task body;
2. task result values delivered through `Task<T>`;
3. async channel payloads;
4. task-group result payloads;
5. explicit low-level actor mailbox payloads.

Task-local values that remain inside one task do not need `Message`. If they
are live across an `await`, they must be safe to store in the private async
frame. Safe code allows owned scalars, structs, enums, arrays, owned runtime
handles documented as frame-safe, values satisfying `Message`, direct local
static read-only slices such as string literals, and compiler-generated
operation keys.

Safe code rejects the following values across `await`: raw pointers, nullable
raw pointers, mutable slices, borrowed read-only slices whose owner is not
syntactically static, thread-local handles, closures that capture forbidden
locals, and compound values whose transitive fields may contain those rejected
views or handles. In the first implementation, compound values containing slice
or reference-view fields are rejected across await unless the compiler has an
explicit built-in proof that the representation is owned and frame-safe.

```ciel
[]const u8 view = buffer[0..n];
await async_time::sleep_ms(1)?;
use(view); // error: borrowed slice crosses await

[]const char msg = "start processing";
await async_time::sleep_ms(1)?;
print(msg); // ok: string-literal storage is static and read-only
```

The frame-safety predicate is compiler-private. Ordinary users should fix the
reported local, move the data into an owned value such as `Bytes`, or construct
the non-message resource inside the task that owns it.

### Lowering and Execution Invariants

The compiler lowers each async function and async closure to an opaque
stackless future type. The generated frame stores a program counter, live
locals, nested future state, operation keys, initialized-field state, and
cleanup state. Spawning a task moves the future into actor-owned task storage;
awaiting a nested future keeps that nested state in the same task frame.

Each task is in one of the runtime states `Ready`, `Suspended`, `Cancelling`,
`Finished`, or `Failed`. The runtime never resumes two continuations of the
same task concurrently. Awaiting I/O suspends only the task. Task termination
runs deterministic cleanup before the task is considered finished. Hidden
resume events are not user-visible messages and cannot carry arbitrary user
payloads.

Safe async concurrency follows these invariants:

1. every async frame is owned by exactly one task;
2. task-local frame values are never exposed through task handles, channels, or
   resume events;
3. every value crossing task ownership is cloned, moved, or stored through a
   proven `Message` path;
4. task handles and channel endpoints are opaque synchronized handles, not
   pointers into async frames;
5. external callbacks route completions through runtime-owned operation tokens;
6. stale completions are discarded only when the relevant `CancelSafe` or
   `Abortable` contract permits dropping them.

## 17. Concurrency and Actors

Ciel's low-level actor model is the runtime isolation model behind
async/await and the explicit mailbox API for advanced code. Ordinary
asynchronous I/O should use the async/await surface in Section 16.

The actor model has four parts:

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

```ciel
struct Actor<M> {
    *void handle;
}
```

Actor state remains encapsulated by the actor runtime. It is initialized when
the actor starts and is updated by the actor's handler. Actor state is never
exposed as a cross-actor `*S`.

Actors can be spawned with cloned messageable state or with actor-owned state
constructed by an initializer. The clone-state API copies both the initial
state and the handler through `Message`:

```ciel
Result<Actor<M>, Error> spawn_actor_cloned<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
```

Messages are checked through `Message`, actor state is handled inside the actor
loop, and safe code cannot send a borrowed pointer to another actor's mutable
state. Actor-handler closures capture by value. Converting a concrete closure
or Ciel ABI `fn` into the handler type retains the `Message` witness used by
the actor runtime to clone the handler across the actor boundary.

The actor-owned-state API constructs `S` inside an initializer closure and
stores it directly in actor runtime storage:

```ciel
Result<Actor<M>, Error> spawn_actor_state<S, M: Message>(
    Result<S, Error> |(): Message| init,
    Result<void, Error> |(*S, Actor<M>, M): Message| handler
);
```

`S` does not need to implement `Message`. The initializer itself must be
`Message`, so it can capture only messageable seed values. Non-message actor
resources such as maps, async streams, frame readers, and queues are built
inside the initializer. The handler receives a writable pointer to actor-owned
state plus the actor's own handle for the current message; it mutates state in
place and returns `Result<void, Error>`.

`Message` is an explicit conversion capability:

```ciel
unsafe interface<T> Result<T, Error> clone_message(*const T value);
unsafe interface<T> bool share_handle_marker(*const T value);
unsafe interface<T> bool thread_local_marker(*const T value);

interface MessageInternal = clone_message;
interface ShareHandleInternal = share_handle_marker;
interface ThreadLocalInternal = thread_local_marker;

interface Message = MessageInternal + !ThreadLocalInternal;
interface ShareHandle = ShareHandleInternal + Message + !ThreadLocalInternal;
interface ThreadLocal = ThreadLocalInternal + !MessageInternal + !ShareHandleInternal;
```

`clone_message` constructs the value that will be owned by the receiver. It may
copy fields, allocate fresh backing storage, serialize and decode, duplicate a
resource handle, intern immutable data, or report an error. Implementing it is a
safety contract, so each implementation uses `unsafe impl`. Calling safe APIs
that require `T: Message` does not require an unsafe block.

Cross-domain standard-library APIs are ordinary functions that require
`Message` and call `clone_message` explicitly:

```ciel
Result<void, Error> send<M: Message>(*const Actor<M> actor, M value);
```

Conceptually, `send` clones before storing into another actor's mailbox:

```ciel
Result<void, Error> send<T: Message>(*const Actor<T> actor, T value) {
    T copy = clone_message(&value)?;
    enqueue(actor, copy);
    return Ok;
}
```

The sender keeps its original value. The receiver receives the result of
`clone_message`, with independent mutable identity:

```ciel
Buffer @buf = make_buffer();
*Buffer p = &buf;
send(actor, buf);        // send calls clone_message(&value)
append(p, "local only"); // mutates only the sender's buffer
```

`spawn_actor_cloned` follows the same rule:

```ciel
Result<Actor<M>, Error> spawn_actor_cloned<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
) {
    S state = clone_message(&initial_state)?;
    Result<S, Error> |(S, M): Message| actor_handler = clone_message(&handler)?;
    return runtime_spawn_actor(state, actor_handler);
}
```

`spawn_actor_state` does not clone `S`:

```ciel
Result<Actor<M>, Error> spawn_actor_state<S, M: Message>(
    Result<S, Error> |(): Message| init,
    Result<void, Error> |(*S, Actor<M>, M): Message| handler
) {
    S state = init()?;
    Result<void, Error> |(*S, Actor<M>, M): Message| actor_handler =
        clone_message(&handler)?;
    return runtime_spawn_actor_state(state, actor_handler);
}
```

Closure messageability is a property of the concrete closure type's generated
environment, not of the erased callable signature alone:

```ciel
i64 x = 1;
spawn_actor_cloned(0, |s, msg| s + msg + x); // ok

i64 local = 1;
*i64 ptr = &local;
spawn_actor_cloned(0, |s, msg| s + *ptr); // compile error
```

The compiler treats every closure literal as a unique concrete type. A concrete
closure type implements `Message` only when every captured field implements
`Message`; a noncapturing closure implements `Message` through an empty
environment. A plain erased closure signature such as
`Result<S, Error> |(S, M)|` does not by itself prove `Message`, because two
closures with that signature can capture different values. A retained signature
such as `Result<S, Error> |(S, M): Message|` carries the witness explicitly.

`Message` is implemented per concrete type. `/std/message` provides unsafe
impls for primitive values, `Error`, `Result<T, E>`, and owned `/std/meta` SOP
nodes. Standard-library handle modules provide their own unsafe impls for actor
handles, channels, mutexes, atomics, and other synchronized handles.

Layout-valid recursive pointer graphs do not gain `Message` automatically.
For example, `struct Node { i64 data; ?*Node next; }` has finite layout, but a
raw pointer graph is not a default cross-actor clone representation. Code must
use an owned representation or write an explicit `clone_message(*const T)`
policy for the nominal type.

Compiler-derived `Message` no longer applies to user structs or enums. Programs
that want structural behavior use the owned representation at the boundary:

```ciel
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

```ciel
unsafe impl clone_message(*const Event value) {
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

The actor runtime is backed by one serial dispatch queue per actor on supported
targets. `spawn_actor_cloned` clones the initial state and handler, creates a runtime
mailbox, a serial queue, and a group used to track accepted jobs. `send`
clones the payload before enqueueing it. `stop` closes the mailbox to new
sends while allowing accepted jobs to drain. `join` closes the mailbox, waits
for accepted jobs through the dispatch group, rejects self-join with an error,
and returns a standard boxed `code_error(...)` error on runtime failure.

Dispatch-managed callbacks are not implicit GC roots. Runtime callbacks that
touch Ciel values or generated code enter through counted callback scope
helpers that attach the current thread to BDWGC/libgc on first entry and detach
only when the outermost callback scope exits.

On supported targets, asynchronous file-descriptor operations use public
dispatch I/O APIs through runtime shims. Low-level operation-token APIs can
still notify explicit actor mailboxes for compatibility. High-level
async/await code wraps the same operation tokens in futures and resumes tasks
through runtime-owned task and operation routing state.

Resource wrappers define their own policy. `/std/io::File` is actor-local by
default. A wrapper that crosses actors implements `Message` by explicitly
duplicating, reconnecting, or otherwise constructing an independent receiver
value.

Shared mutable identity is represented through synchronized handle types:

```ciel
struct Channel<T> { *void handle; }
struct Atomic<T> { *void handle; }
struct Actor<M> { *void handle; }
```

Their safe APIs expose operations:

```ciel
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
lifetime rooting, and it is written as `unsafe impl`.

Mutexes are a low-level library feature. The safe mutex API uses value
replacement:

```ciel
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

```ciel
unsafe interface<T> Result<T, Error> clone_message(*const T value);
unsafe interface<T> bool share_handle_marker(*const T value);
unsafe interface<T> bool thread_local_marker(*const T value);

interface MessageInternal = clone_message;
interface ShareHandleInternal = share_handle_marker;
interface ThreadLocalInternal = thread_local_marker;

interface Message = MessageInternal + !ThreadLocalInternal;
interface ShareHandle = ShareHandleInternal + Message + !ThreadLocalInternal;
interface ThreadLocal = ThreadLocalInternal + !MessageInternal + !ShareHandleInternal;
```

The public aliases encode the standard capability relationships. `Message`
means an explicit `clone_message` witness and no thread-local marker.
`ShareHandle` means a share-handle marker, `Message`, and no thread-local
marker. `ThreadLocal` means a thread-local marker and neither a message clone
witness nor a share-handle marker.

Examples:

```ciel
Result<void, Error> send<T: Message>(*const Actor<T> actor, T value);
Result<void, Error> accept_handle<T: ShareHandle>(T handle);
void local_resource<T: ThreadLocal>(*const T value);
```

Negative constraints remain useful for APIs that require a type to stay
actor-local:

```ciel
void bind_local<T: !Message>(*const T value);
```

C interop is a trusted boundary. C wrappers decide which C-backed values are
messageable, shareable handles, or actor-local resources. Opaque C handles
start as `ThreadLocal`; because the public aliases make `ThreadLocal`
incompatible with `Message` and `ShareHandle`, those handles cannot cross actor
or channel boundaries through either the nominal type or `meta::Repr<T>`.
Wrappers implement `Message` by explicitly duplicating, reconnecting, or
otherwise constructing an independent receiver value; wrappers implement
`ShareHandle` only when operations are internally synchronized or immutable.

The compiler recognizes canonical `/std/message` interface names only for
generic constraint lookup, retained closure witnesses, coherence checks, and
code generation of calls to selected ordinary impls. It does not synthesize
`clone_message` implementations, infer `Message`, `ShareHandle`, or
`ThreadLocal` policy from type structure, or emit policy-specific fallback
diagnostics. Structural message behavior is proved through the ordinary
`/std/message` impls for owned `/std/meta` SOP nodes and through the same
positive/negative capability algebra used by every interface alias; failures
surface as normal missing or forbidden capability constraints.

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

## 18. Standard Library Boundary

The compiler treats the standard library as ordinary Ciel source except for the
generic `/std/meta` helpers and runtime hooks explicitly named in this
specification. `Error`, `must`, `expect`, `Message`, actor handles, channels,
atomics, and synchronization handles are library surface, not syntax. The `?`
operator is syntax, and it recognizes the `Result` type exported by
`/std/result` when it is visible through a direct import or a re-export such as
`/std/lib`.

There is no prelude. Every standard-library module must be imported explicitly:

```ciel
import /std/result;
import /std/panic;
import /std/io;
import /std/message;
import /std/actor;
```

`/std/lib` is the standard facade module. It re-exports `/std/error`,
`/std/result`, `/std/panic`, `/std/c`, `/std/io`, `/std/async_io`,
`/std/async_net`, `/std/async_time`, `/std/message`, `/std/meta`,
`/std/actor`, `/std/channel`, `/std/sync`, `/std/atomic`, `/std/codec`,
`/std/buf`, `/std/bytes`, `/std/text`, `/std/map`, `/std/shared_map`,
`/std/time`, `/std/env`, `/std/crypto`, and `/std/net`.
It is still imported explicitly like any other file.

String literals have compiler support because each occurrence emits
program-lifetime static NUL-terminated bytes and constructs a `[]const char`
slice:

```ciel
[]const char name = "ciel";
usize n = name.len;
*const char raw = name.ptr;
```

The core standard library is organized around small implementation modules and
stable facade modules:

```ciel
// /std/error
export import /std/result/core;
export import /std/error/core;
export import /std/error/basic;
export import /std/error/context;
```

```ciel
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

```ciel
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

```ciel
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

```ciel
// /std/result/core
export enum Result<T, E> {
    Ok(T),
    Err(E),
}
```

```ciel
// /std/result
export import /std/result/core;
export import /std/error;

export T must<T, E>(Result<T, E> value);
export T expect<T, E>(Result<T, E> value, []const char message);
```

```ciel
// /std/format
export import /std/format/number;
```

```ciel
// /std/format/number
export []const char u64_to_string(u64 value);
export []const char usize_to_string(usize value);
export []const char i64_to_string(i64 value);
```

```ciel
// /std/panic
unsafe extern "C" {
    noescape never ciel_panic(*const char message, usize len);
}

export never panic([]const char message) {
    unsafe {
        ciel_panic(message.ptr, message.len);
    };
}
```

`panic` prints a diagnostic to standard error and terminates the process with a
nonzero exit status. The current runtime uses exit code `101` for panic
termination.

```ciel
// /std/c
#c_include "stddef.h"
#c_include "stdint.h"

export extern "C" {
    type c_char = "char";
    type c_schar = "signed char";
    type c_uchar = "unsigned char";
    type c_short = "short";
    type c_ushort = "unsigned short";
    type c_int = "int";
    type c_uint = "unsigned int";
    type c_long = "long";
    type c_ulong = "unsigned long";
    type c_long_long = "long long";
    type c_ulong_long = "unsigned long long";
    type c_float = "float";
    type c_double = "double";

    type c_size_t = "size_t";
    type c_ptrdiff_t = "ptrdiff_t";
    type c_intptr_t = "intptr_t";
    type c_uintptr_t = "uintptr_t";
    type c_intmax_t = "intmax_t";
    type c_uintmax_t = "uintmax_t";
}

#if !is_target_os("windows")
#c_include "sys/types.h"

export extern "C" {
    type c_ssize_t = "ssize_t";
}
#endif

export type c_string = *char;
export type const_c_string = *const char;
```

```ciel
// /std/io
export import /std/result;
import /std/message;

export enum OpenMode {
    Read,
    Write,
    Append,
}

struct File;

export interface<T> []const char to_string(*const T value);
export interface printable = to_string;

export Error last_error();

export Result<R, Error> with_open<R: Message>(
    []const char path,
    OpenMode mode,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_open_read<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_create<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_append<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<usize, Error> read(File file, []u8 out);
export Result<usize, Error> write(File file, []const u8 data);
export Result<usize, Error> write_text_once(File file, []const char text);
export Result<void, Error> write_all(File file, []const u8 data);
export Result<void, Error> write_text(File file, []const char text);

export []const char f32_to_string(f32 value);
export []const char f64_to_string(f64 value);

export Result<void, Error> write_value<T: printable>(File file, T value);
export Result<void, Error> write_format(File file, []const char fmt, []printable values);
export Result<void, Error> print_value<T: printable>(T value);
export Result<void, Error> println_value<T: printable>(T value);
export Result<void, Error> eprint_value<T: printable>(T value);
export Result<void, Error> eprintln_value<T: printable>(T value);
export Result<void, Error> print([]const char fmt, []printable values);
export Result<void, Error> println([]const char fmt, []printable values);
export Result<void, Error> eprint([]const char fmt, []printable values);
export Result<void, Error> eprintln([]const char fmt, []printable values);
```

`/std/io` is a scoped blocking I/O API. Importers do not get a public copyable
descriptor value from this module. Instead, a private `File` token is passed by
value into a callback and is closed when that callback returns. The callback
result type is constrained as `R: Message`, so safe code cannot return the
private token through the ordinary result path.

The runtime stores real descriptors in a generation-checked slot table. The
private `File` token contains a slot index and generation. Every blocking I/O
operation validates that the slot is live, the generation matches, and the slot
state is open before touching the OS descriptor. This prevents stale escaped
tokens from touching a reused descriptor number.

`stdout`, `stderr`, and formatting helpers use borrowed slot-table entries for
the process standard streams. Printable values are values that implement
`to_string`; printing functions convert values to `[]const char` first, then
write through a scoped `File`.

Low-level raw descriptor interop lives in `/std/os/fd`, not `/std/io`:

```ciel
// /std/os/fd
import /std/c as c;

export unsafe struct RawFd {
    c::c_int raw;
}

export unsafe RawFd from_raw_fd(c::c_int raw);
export unsafe c::c_int raw_fd(RawFd fd);
export RawFd stdin();
export RawFd stdout();
export RawFd stderr();
```

`RawFd` is a low-level interop type. It is actor-local by default and requires
unsafe operations for adoption and extraction.

```ciel
// /std/io_posix
import /std/c as c;

#c_include "unistd.h"

export unsafe extern "C" {
    c::c_ssize_t read(c::c_int fd, *void buf, c::c_size_t count);
    c::c_ssize_t write(c::c_int fd, *const void buf, c::c_size_t count);
    c::c_int close(c::c_int fd);
}
```

`/std/io_posix` exposes raw POSIX `read`, `write`, and `close` declarations for
low-level interop code. It is unsafe and platform-specific; ordinary file code
should use `/std/io` or `/std/async_io`.

Formatted printing uses `{}` placeholders and a `[]printable` slice, so callers
can pass heterogeneous printable values through dynamic interface erasure:

```ciel
print("{} = {}", ["answer", 42 as usize]);
```

```ciel
// /std/message
import /std/error;
import /std/result;
import /std/meta as meta;

export unsafe interface<T> Result<T, Error> clone_message(*const T value);
export unsafe interface<T> bool share_handle_marker(*const T value);
export unsafe interface<T> bool thread_local_marker(*const T value);

export interface MessageInternal = clone_message;
export interface ShareHandleInternal = share_handle_marker;
export interface ThreadLocalInternal = thread_local_marker;

export interface Message = MessageInternal + !ThreadLocalInternal;
export interface ShareHandle = ShareHandleInternal + Message + !ThreadLocalInternal;
export interface ThreadLocal = ThreadLocalInternal + !MessageInternal + !ShareHandleInternal;
```

```ciel
// /std/meta
export usize type_size<T>();
export usize type_align<T>();

export struct Type<T> {}

export struct RefRepr<T> {}
export struct Repr<T> {}

export interface<T> bool ciel_fn_value_marker(*const T value);
export interface CielFnValue = ciel_fn_value_marker;

export interface<T> bool closure_value_marker(*const T value);
export interface ClosureValue = closure_value_marker;

export struct HNil {}

export struct HCons<H, T> {
    H head;
    T tail;
}

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

export enum CoNil {}

export enum Coproduct<H, T> {
    This(H),
    Next(T),
}

export struct VariantRef<P> {
    []const char name;
    P payload;
}

export struct Variant<P> {
    []const char name;
    P payload;
}

export struct ArrayNil {}

export struct ArrayChunk1<T> { T item0; }
export struct ArrayChunk2<T> { T item0; T item1; }
export struct ArrayChunk3<T> { T item0; T item1; T item2; }
export struct ArrayChunk4<T> { T item0; T item1; T item2; T item3; }
export struct ArrayChunk5<T> { T item0; T item1; T item2; T item3; T item4; }
export struct ArrayChunk6<T> { T item0; T item1; T item2; T item3; T item4; T item5; }
export struct ArrayChunk7<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; }
export struct ArrayChunk8<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; }
export struct ArrayChunk9<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; }
export struct ArrayChunk10<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; }
export struct ArrayChunk11<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; }
export struct ArrayChunk12<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; T item11; }
export struct ArrayChunk13<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; T item11; T item12; }
export struct ArrayChunk14<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; T item11; T item12; T item13; }
export struct ArrayChunk15<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; T item11; T item12; T item13; T item14; }
export struct ArrayChunk16<T> { T item0; T item1; T item2; T item3; T item4; T item5; T item6; T item7; T item8; T item9; T item10; T item11; T item12; T item13; T item14; T item15; }

export struct ArrayCat<L, R> {
    L left;
    R right;
}

export RefRepr<T> as_ref_repr<T>(*const T value);
export Repr<T> into_repr<T>(*const T value);
export T from_repr<T>(Repr<T> value);
```

```ciel
// /std/actor
import /std/result;
import /std/message;

export struct Actor<M> {
    *void handle;
}

export Result<Actor<M>, Error> spawn_actor_cloned<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
export Result<Actor<M>, Error> spawn_actor_state<S, M: Message>(
    Result<S, Error> |(): Message| init,
    Result<void, Error> |(*S, Actor<M>, M): Message| handler
);
export Result<void, Error> send<T: Message>(*const Actor<T> actor, T value);
export Result<void, Error> stop<T: Message>(*const Actor<T> actor);
export Result<void, Error> join<T: Message>(*const Actor<T> actor);
```

```ciel
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

```ciel
// /std/atomic
export import /std/error;
export import /std/message;
export import /std/result;

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

export unsafe interface<T> bool atomic_value_marker(*const T value);
export interface AtomicValue = atomic_value_marker;

export unsafe interface<T> bool atomic_integer_marker(*const T value);
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

```ciel
// /std/sync
import /std/result;
import /std/message;
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

export Result<Mutex<T>, Error> make_mutex<T: Message>(T initial);
export Result<R, Error> mutex_update<T: Message, F, R>(
    *const Mutex<T> mutex,
    *const F f
);
export Result<R, Error> mutex_with<T, R: Message>(
    *const Mutex<T> mutex,
    Result<R, Error> |(*T)| body
);
```

```ciel
// /std/lib
export import /std/error;
export import /std/result;
export import /std/panic;
export import /std/c;
export import /std/io;
export import /std/async_io;
export import /std/async_net;
export import /std/async_time;
export import /std/message;
export import /std/meta;
export import /std/actor;
export import /std/channel;
export import /std/sync;
export import /std/atomic;
export import /std/codec;
export import /std/buf;
export import /std/bytes;
export import /std/text;
export import /std/map;
export import /std/shared_map;
export import /std/time;
export import /std/env;
export import /std/crypto;
export import /std/net;

export Result<void, Error> sleep_ms(u64 ms);
```

```ciel
// /std/codec
import /std/result;
import /std/meta as meta;

export interface<T> usize encoded_len(T value);
export interface<T> Result<void, Error> put_be([]u8 out, T value);
export interface<T> Result<void, Error> put_le([]u8 out, T value);
export interface<T> Result<T, Error> get_be(meta::Type<T> tag, []const u8 data);
export interface<T> Result<T, Error> get_le(meta::Type<T> tag, []const u8 data);

export Result<[]u8, Error> encode_be<T: encoded_len + put_be>(T value);
export Result<[]u8, Error> encode_le<T: encoded_len + put_le>(T value);
```

```ciel
// /std/buf
import /std/result;

export unsafe struct ByteBuf {
    []u8 storage;
    usize len;
}

export Result<ByteBuf, Error> byte_buf_new(usize capacity);
export usize byte_buf_len(*const ByteBuf buf);
export void byte_buf_clear(*ByteBuf buf);
export []const u8 byte_buf_slice(*const ByteBuf buf);
export []u8 byte_buf_mut_slice(*ByteBuf buf);
export Result<void, Error> byte_buf_reserve(*ByteBuf buf, usize additional);
export Result<void, Error> byte_buf_push_slice(*ByteBuf buf, []const u8 data);
export Result<[]u8, Error> byte_buf_spare_mut_slice(*ByteBuf buf, usize additional);
export Result<void, Error> byte_buf_commit_tail(*ByteBuf buf, usize additional);
export Result<void, Error> byte_buf_discard_prefix(*ByteBuf buf, usize count);
```

`/std/buf` provides a GC-backed growable byte buffer. `ByteBuf` is an unsafe
struct so safe application code cannot construct invalid internal descriptors;
callers use `byte_buf_new` and the exported operations. Slice-returning
functions expose views into the buffer's initialized prefix. `byte_buf_clear`
sets the initialized length to zero without releasing capacity, and
`byte_buf_reserve` grows while preserving existing bytes.
`byte_buf_spare_mut_slice` and `byte_buf_commit_tail` support staged appends:
callers reserve writable tail space, fill it through the returned slice, then
commit the number of bytes actually initialized. This pattern is used by frame
readers that copy async `Bytes` into reusable buffers.
`byte_buf_discard_prefix` removes an initialized prefix and shifts the
remaining bytes down, which supports frame parsers that retain partial input
between async reads.

```ciel
// /std/map
import /std/result;
import /std/message;

export interface<T> u64 hash_key(*const T value, u64 seed);
export interface<T> bool key_eq(*const T left, *const T right);
export interface map_key = hash_key + key_eq;

export unsafe struct HashMap<K, V> {
    *void buckets;
    usize capacity;
    usize len;
    u64 seed;
}

export enum InsertResult<V> {
    Inserted,
    Replaced(V),
}

export enum RemoveResult<V> {
    Removed(V),
    Missing,
}

export enum GetResult<V> {
    MapFound(V),
    MapMissing,
}

export enum PopResult<K, V> {
    Popped(K, V),
    Empty,
}

export Result<HashMap<K, V>, Error> hash_map_new<K: map_key, V>();
export usize hash_map_len<K: map_key, V>(*const HashMap<K, V> map);
export void hash_map_clear<K: map_key, V>(*HashMap<K, V> map);
export Result<bool, Error> hash_map_contains_key<K: map_key, V>(
    *const HashMap<K, V> map,
    K key
);
export Result<GetResult<V>, Error> hash_map_get<K: map_key, V: Message>(
    *const HashMap<K, V> map,
    K key
);
export Result<InsertResult<V>, Error> hash_map_insert<K: map_key, V>(
    *HashMap<K, V> map,
    K key,
    V value
);
export Result<RemoveResult<V>, Error> hash_map_remove<K: map_key, V>(
    *HashMap<K, V> map,
    K key
);
export Result<PopResult<K, V>, Error> hash_map_pop_any<K: map_key, V>(
    *HashMap<K, V> map
);
export Result<R, Error> hash_map_with<K: map_key, V, R: Message>(
    *HashMap<K, V> map,
    K key,
    Result<R, Error> |(*V)| body
);
```

Typical call sites write the key/value types at construction and rely on
generic inference from the typed map receiver afterward:

```ciel
_ @table = must(hash_map_new<u32, i64>());
must(hash_map_insert(&table, 7 as u32, 10));
usize count = hash_map_len(&table);
```

`HashMap<K, V>` itself is the type witness for operations that take the map;
ordinary map operations do not need separate `meta::Type<T>` tag values.
`hash_map_get` returns a cloned value and therefore requires `V: Message`;
`hash_map_with` is the scoped mutable-access API for values that should not be
cloned. `hash_map_pop_any` removes one arbitrary entry, which is useful for
draining actor-local work queues and for implementing synchronized facades.

`/std/map` provides an actor-local mutable hash table. It uses separate
chaining with GC-backed nodes and a runtime-allocated bucket array. `HashMap`
does not implement `Message`; code should send keys, values, snapshots, or
explicit messages rather than live map storage. Primitive key policies cover
`bool`, `char`, signed integer types, unsigned integer types, and `usize`.
Structural policies cover `/std/meta` product and sum nodes used by
`meta::RefRepr<T>` and `meta::Repr<T>`, so visible structs and enums can opt in
with explicit `hash_key` and `key_eq` wrappers that delegate to the structural
representation.

```ciel
// /std/shared_map
import /std/result;
import /std/map as map;
import /std/message;
import /std/sync;

unsafe struct SharedMapState<K, V> {
    map::HashMap<K, V> inner;
}

export struct SharedMap<K, V> {
    sync::Mutex<SharedMapState<K, V>> state;
}

export enum SharedMapGet<V> {
    SharedMapFound(V),
    SharedMapMissing,
}

export enum SharedMapPop<K, V> {
    SharedMapItem(K, V),
    SharedMapEmpty,
}

export interface shared_map_key = map::map_key + Message;

export Result<SharedMap<K, V>, Error> shared_map_new<K: shared_map_key, V: Message>();
export Result<map::InsertResult<V>, Error> shared_map_insert<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key,
    V value
);
export Result<SharedMapGet<V>, Error> shared_map_get<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key
);
export Result<SharedMapGet<V>, Error> shared_map_remove<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key
);
export Result<SharedMapPop<K, V>, Error> shared_map_pop_any<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared
);
export Result<usize, Error> shared_map_len<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared
);
```

`/std/shared_map` wraps an actor-local `HashMap` in a shareable `Mutex` handle.
Keys must be both `map_key` and `Message`, and values must be `Message`, because
operations clone values across the synchronized boundary. It is intended for
registries and routing tables shared by async tasks or actors, while
`/std/map` remains the cheaper actor-local storage primitive.

```ciel
// /std/time
import /std/result;

export Result<u64, Error> monotonic_ms();
export Result<void, Error> sleep_ms(u64 ms);
```

`/std/time` provides blocking wall-clock-independent timing helpers. The
monotonic clock reports milliseconds from an unspecified steady epoch and must
not go backwards during one process run. `sleep_ms` blocks the current OS worker
thread until the requested duration has elapsed or a platform error is reported;
it is intended for simple backoff, tests, and blocking utility code. Async
tasks and actor continuations that must stay non-blocking should use
`/std/async_time::sleep_ms` or the lower-level async timer operation-token API.

```ciel
// /std/env
import /std/result;

export Result<usize, Error> args_len();
export Result<[]const char, Error> arg(usize index);
```

`/std/env` exposes process command-line arguments as stable read-only character
slices. Index `0` is the host-provided executable argument. `arg` returns a
standard `Error` when the index is outside the current `args_len`. Environment
variables, working-directory access, process spawning, and path search are
reserved for later modules.

```ciel
// /std/crypto
import /std/result;

export unsafe struct SystemRng {
    *void handle;
}

export type Rng = SystemRng;

export unsafe struct Hash {
    *void handle;
}

export unsafe struct Mac {
    *void handle;
}

export enum HashAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

export enum MacAlgorithm {
    HmacSha256,
    HmacSha384,
    HmacSha512,
}

export []const char hash_algorithm_name(HashAlgorithm algorithm);
export []const char mac_algorithm_name(MacAlgorithm algorithm);

export Result<void, Error> random_bytes([]u8 out);
export Result<SystemRng, Error> system_rng();
export Result<void, Error> rng_random_bytes(SystemRng rng, []u8 out);

export Result<usize, Error> hash_once(
    []const char algorithm,
    []const u8 data,
    []u8 out
);

export Result<usize, Error> hash_once_alg(
    HashAlgorithm algorithm,
    []const u8 data,
    []u8 out
);

export Result<Hash, Error> hash_new([]const char algorithm);
export Result<Hash, Error> hash_new_alg(HashAlgorithm algorithm);
export Result<void, Error> hash_update(Hash hash, []const u8 data);
export Result<usize, Error> hash_finish(Hash hash, []u8 out);
export Result<void, Error> hash_clear(Hash hash);

export Result<usize, Error> mac_once(
    []const char algorithm,
    []const u8 key,
    []const u8 data,
    []u8 out
);

export Result<usize, Error> mac_once_alg(
    MacAlgorithm algorithm,
    []const u8 key,
    []const u8 data,
    []u8 out
);

export Result<Mac, Error> mac_new([]const char algorithm, []const u8 key);
export Result<Mac, Error> mac_new_alg(MacAlgorithm algorithm, []const u8 key);
export Result<void, Error> mac_update(Mac mac, []const u8 data);
export Result<usize, Error> mac_finish(Mac mac, []u8 out);
export Result<void, Error> mac_clear(Mac mac);

export bool constant_time_eq([]const u8 left, []const u8 right);
```

`/std/crypto` exposes backend-neutral cryptographic operations backed by Botan's
C FFI in the first runtime. `random_bytes` uses the system CSPRNG directly.
`SystemRng` is an explicit reusable CSPRNG handle. One-shot and streaming hash
and MAC APIs write into caller-provided output buffers and return the number of
bytes written. A too-small output buffer returns a standard `Error`.

The recommended algorithm names are `SHA-256`, `SHA-384`, `SHA-512`,
`HMAC(SHA-256)`, `HMAC(SHA-384)`, and `HMAC(SHA-512)`. Application code should
prefer the enum-based `*_alg` helpers for those common algorithms. The
string-based entry points are still available for backend-neutral protocol
surfaces and compatibility with older peers; after rejecting empty names,
embedded NUL bytes, and overly long algorithm names, the runtime passes the
algorithm name through to Botan. HMAC keys shorter than 16 bytes are rejected.
When Botan reports an error, `/std/crypto` surfaces Botan's error description as
a standard text error.

`SystemRng` implements `Message` as a shareable handle because Botan's system
RNG is thread-safe. `Hash` and `Mac` are unsafe runtime-backed handle structs
and do not implement `Message`; application code should pass byte slices or
completed digest/MAC values across actor boundaries instead of live streaming
crypto contexts. `hash_clear` and `mac_clear` release their runtime handles;
later use of the cleared value returns an error.

```ciel
// /std/net
import /std/result;

export enum AddressFamily {
    Ip4,
    Ip6,
}

export unsafe struct SocketAddr {
    *void handle;
}

export unsafe struct TcpListener {
    u32 slot;
    u32 generation;
}

export unsafe struct TcpStream {
    u32 slot;
    u32 generation;
}

export Result<SocketAddr, Error> parse_addr([]const char text);
export Result<SocketAddr, Error> resolve_tcp([]const char host, u16 port);
export Result<AddressFamily, Error> addr_family(SocketAddr addr);
export Result<u16, Error> addr_port(SocketAddr addr);
export Result<usize, Error> addr_write(SocketAddr addr, []char out);
export Result<[]const char, Error> addr_to_string(SocketAddr addr);

export Result<TcpListener, Error> tcp_listen(SocketAddr addr);
export Result<TcpStream, Error> tcp_accept(TcpListener listener);
export Result<TcpStream, Error> tcp_connect(SocketAddr addr);
export Result<TcpStream, Error> tcp_connect_host([]const char host, u16 port);
export Result<usize, Error> tcp_read(TcpStream stream, []u8 out);
export Result<usize, Error> tcp_write(TcpStream stream, []const u8 data);
export Result<void, Error> tcp_write_all(TcpStream stream, []const u8 data);
export Result<void, Error> tcp_shutdown_read(TcpStream stream);
export Result<void, Error> tcp_shutdown_write(TcpStream stream);
export Result<void, Error> tcp_shutdown(TcpStream stream);
export Result<void, Error> tcp_close(TcpStream stream);
export Result<void, Error> listener_close(TcpListener listener);
export Result<SocketAddr, Error> listener_addr(TcpListener listener);
export Result<SocketAddr, Error> stream_local_addr(TcpStream stream);
export Result<SocketAddr, Error> stream_peer_addr(TcpStream stream);

export Result<R, Error> with_tcp_connect<R: Message>(
    SocketAddr addr,
    Result<R, Error> |(TcpStream)| body
);

export Result<R, Error> with_tcp_connect_host<R: Message>(
    []const char host,
    u16 port,
    Result<R, Error> |(TcpStream)| body
);

export Result<R, Error> with_tcp_listen<R: Message>(
    SocketAddr addr,
    Result<R, Error> |(TcpListener)| body
);
```

`/std/net` provides a blocking TCP socket layer over the platform socket API.
It does not introduce a third-party networking dependency. `parse_addr` parses
numeric IPv4 and bracketed numeric IPv6 endpoints such as `127.0.0.1:8080` and
`[::1]:8080`; it does not perform DNS. Domain-name lookup is explicit through
`resolve_tcp`, and `tcp_connect_host` resolves a host name and tries the
returned TCP addresses until one connects.

`SocketAddr` is an immutable runtime-backed address value and implements
`Message` as a shareable handle. `TcpListener` and `TcpStream` are
runtime-backed descriptor handles and are actor-local blocking resources. The
runtime stores real descriptors in a generation-checked slot table, so stale
copies of a closed listener or stream cannot accidentally operate on a reused
descriptor. The scoped `with_tcp_*` helpers follow the `/std/io` pattern and
close the opened resource on normal and error returns from the body.

```ciel
// /std/async/bytes
export import /std/result;

export struct Bytes {
    *void handle;
}

export Result<Bytes, Error> bytes_copy([]const u8 data);
export Result<Bytes, Error> bytes_copy_chars([]const char text);
export Result<Bytes, Error> bytes_concat([]const u8 left, []const u8 right);
export Result<Bytes, Error> bytes_prepend([]const u8 prefix, Bytes bytes);
export Result<Bytes, Error> bytes_slice(Bytes bytes, usize offset, usize len);
export usize bytes_len(Bytes bytes);
export usize bytes_capacity(Bytes bytes);
export Result<usize, Error> bytes_copy_to(Bytes bytes, []u8 out);
export Result<usize, Error> bytes_copy_to_chars(Bytes bytes, []char out);
```

`/std/async/bytes` is the implementation module for immutable runtime-backed
owned byte buffers. `Bytes` implements `Message` as a shareable handle. The
copy helpers allocate new immutable handles from slices; `bytes_slice` creates
a subrange handle; `bytes_copy_to` and `bytes_copy_to_chars` copy into caller
provided mutable buffers. `bytes_capacity` exposes backing capacity for APIs
such as async TCP `read_into`, where a returned buffer can be reused.

```ciel
// /std/bytes
export import /std/async/bytes;

export Result<Bytes, Error> bytes_empty();
export Result<Bytes, Error> bytes_from_text([]const char text);
export Result<[]u8, Error> bytes_to_slice(Bytes bytes);
export Result<Bytes, Error> bytes_append(Bytes left, Bytes right);
```

`Bytes` is the general immutable owned byte buffer used by async file and TCP
APIs. It is a runtime-backed handle, implements `Message` through explicit
standard-library policy, and can be copied into slices when mutable inspection
is needed. `/std/bytes` is the general facade; `/std/async/bytes` remains the
implementation module exported by older async modules.

```ciel
// /std/text
export import /std/result;
import /std/bytes as bytes;

export struct Text {
    bytes::Bytes bytes;
}

export Result<Text, Error> text_empty();
export Result<Text, Error> text_copy([]const char text);
export usize text_len(Text text);
export Result<bytes::Bytes, Error> text_to_bytes(Text text);
export Result<[]char, Error> text_to_chars(Text text);
export Result<[]const char, Error> text_to_slice(Text text);
```

`/std/text` wraps immutable owned bytes as text-oriented data. It does not yet
perform Unicode normalization or validation beyond preserving byte contents.
`Text` implements `Message` as a shareable handle, so it is suitable for actor
and async-task payloads. Conversion helpers copy the contents out when mutable
or slice inspection is needed.

```ciel
// /std/async
export import /std/async/core;

export struct Future<T> {
    *void handle;
}

export unsafe interface<A, Out> *void awaitable_future(*const A awaitable);
export interface Awaitable<Out> = awaitable_future<Out>;

export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;
export interface SelectableFuture<Out> = Awaitable<Out> + CancelSafe + Abortable;

export T block_on<T, A: Awaitable<T> + Abortable>(A future);
export Future<Result<Out, Error>> future_from_op<Op, Out>(Op op);

export Error timeout_error();
export Error channel_closed_error();

export struct Task<T> {
    *void handle;
}

export Result<Task<T>, Error> spawn<T, A: Awaitable<Result<T, Error>> + Abortable>(
    A body
);
export Result<void, Error> cancel<T>(*const Task<T> task);
export Result<bool, Error> is_finished<T>(*const Task<T> task);

export struct Sender<T> {
    *void handle;
}

export struct Receiver<T> {
    *void handle;
}

export struct SendPermit<T> {
    *void handle;
}

export struct ChannelPair<T> {
    Sender<T> sender;
    Receiver<T> receiver;
}

export Result<ChannelPair<T>, Error> channel<T>(usize capacity);
export async Result<void, Error> send<T>(Sender<T> sender, T value);
export Result<void, Error> try_send<T>(Sender<T> sender, T value);
export async Result<SendPermit<T>, Error> reserve<T>(Sender<T> sender);
export Result<void, Error> permit_send<T>(SendPermit<T> permit, T value);
export async Result<T, Error> recv<T>(Receiver<T> receiver);
export Result<void, Error> close<T>(Sender<T> sender);
export Result<void, Error> close_receiver<T>(Receiver<T> receiver);

export struct TaskGroup<T> {
    *void handle;
}

export Result<TaskGroup<T>, Error> task_group<T>();
export Result<void, Error> group_add<T>(*const TaskGroup<T> group, Task<T> task);
export async Result<T, Error> group_next<T>(*const TaskGroup<T> group);
export Result<void, Error> group_cancel_all<T>(*const TaskGroup<T> group);
export Result<void, Error> group_close<T>(*const TaskGroup<T> group);

export async Result<Out, Error> timeout<Out, A: SelectableFuture<Out>>(
    A future,
    u64 ms
);
```

`/std/async` is the user-facing async/await surface. `Future<T>` is a
runtime-backed future handle; compiler-generated async functions and closures
also implement `Awaitable<T>` without exposing their generated frame type.
`block_on` is the synchronous bridge for `main`, tests, and embedding hosts; it
starts a future on the task runtime and blocks the current thread until the
future returns. Async bodies should use `await` instead of nested `block_on`.

`Task<T>` is an awaitable handle to a spawned task. `spawn` starts an awaitable
body whose output is `Result<T, Error>`. The compiler attaches hidden
`Message` obligations to spawned-task captures and to `T`, because those values
cross task ownership. `cancel` aborts the task's current suspended operation
and runs deterministic cleanup; awaiting a cancelled task produces the stable
runtime cancellation error. `is_finished` reports whether the task has reached
a terminal state.

Async channels are bounded. `send` waits for capacity, `try_send` reports full
or closed without suspension, `reserve` waits for capacity and returns a
permit, and `permit_send` commits a value into a reserved slot. Sender and
receiver lifetimes wake the opposite side: the last sender wakes receivers, and
the last receiver wakes senders and outstanding reservations with
`channel_closed_error()`. Channel payloads carry hidden `Message` obligations
at send and receive boundaries.

Task groups support dynamic concurrency. `group_next` returns completed task
results in completion order without cancelling unfinished tasks. `group_cancel_all`
aborts unfinished tasks through `Abortable`, and `group_close` releases the
remaining group handle state.

`timeout` races a selectable future with a timer. Timing out cancels only the
waiter future; it does not assume that an arbitrary underlying protocol can
discard partial state. The operand therefore must satisfy
`SelectableFuture<Out>`, which is `Awaitable<Out> + CancelSafe + Abortable`.

```ciel
// /std/async/internal/adapter
import /std/actor as actor;
import /std/c as c;
import /std/message;
import /std/result;

export interface<Op, M> Result<void, Error> notify_done(
    Op op,
    actor::Actor<M> actor_handle,
    M message
);

export interface<Op, Out> Result<Out, Error> finish(Op op);
export unsafe interface<Op> *void raw_operation(*const Op op);
export unsafe interface<Op, Out> c::c_int poll_done(Op op, *Out out);
```

The internal adapter namespace describes runtime operation tokens. `notify_done`
and `finish` support low-level actor completion tests and direct operation
integration.
`raw_operation` and `poll_done` are used by `future_from_op` to wrap a one-shot
runtime operation as a future. Normal application code should call awaitable
stdlib functions such as `async_io::read_bytes`, `async_net::read`, or
`async_time::sleep_ms` instead of implementing operation adapters directly.

```ciel
// /std/async_io
export import /std/result;
export import /std/async/bytes;
import /std/actor as actor;
import /std/io;
import /std/message;
import /std/os/fd as os_fd;

export struct AsyncFd {
    *void handle;
}

export struct AsyncRead {
    *void handle;
}

export struct AsyncWrite {
    *void handle;
}

export Result<AsyncFd, Error> open_async([]const char path, io::OpenMode mode);
export Result<AsyncFd, Error> open_async_read([]const char path);
export Result<AsyncFd, Error> create_async([]const char path);
export Result<AsyncFd, Error> append_async([]const char path);
export unsafe Result<AsyncFd, Error> async_from_raw_fd(os_fd::RawFd fd);
export Result<void, Error> close_async(AsyncFd fd);

export Result<AsyncRead, Error> read_bytes_async(AsyncFd fd, usize max_len);
export Result<AsyncWrite, Error> write_bytes_async(AsyncFd fd, Bytes data);
export async Result<Bytes, Error> read_bytes(AsyncFd fd, usize max_len);
export async Result<usize, Error> write_bytes(AsyncFd fd, Bytes data);

export Result<void, Error> notify_read_done<M: Message>(
    *const AsyncRead op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<void, Error> notify_write_done<M: Message>(
    *const AsyncWrite op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<Bytes, Error> finish_read(AsyncRead op);
export Result<usize, Error> finish_write(AsyncWrite op);
export Result<void, Error> cancel_read(AsyncRead op);
export Result<void, Error> cancel_write(AsyncWrite op);
```

`/std/async_io` provides awaitable file-descriptor operations. The high-level
`read_bytes` and `write_bytes` functions are async functions and are the normal
API. The `*_async`, `notify_*`, `finish_*`, and `cancel_*` operation-token
functions are low-level hooks for direct actor-completion integration. Raw fd
reads and writes are `Abortable` but not `CancelSafe` by default because
cancellation may hide offset changes or partial writes.

```ciel
// /std/async_net
export import /std/result;
export import /std/async/bytes;
import /std/actor as actor;
import /std/message;
import /std/net;

export struct AsyncTcpListener {
    *void handle;
}

export struct AsyncTcpStream {
    *void handle;
}

export struct AsyncTcpReadHalf {
    *void handle;
}

export struct AsyncTcpWriteHalf {
    *void handle;
}

export struct AsyncTcpSplit {
    AsyncTcpReadHalf read;
    AsyncTcpWriteHalf write;
}

export struct BufferedStreamReader {
    *void handle;
}

export struct AsyncAccept {
    *void handle;
}

export struct AsyncConnect {
    *void handle;
}

export struct AsyncTcpRead {
    *void handle;
}

export struct AsyncTcpWrite {
    *void handle;
}

export struct AsyncBufferedRead {
    *void handle;
}

export struct ReadIntoResult {
    Bytes bytes;
    usize read;
}

export Result<AsyncTcpListener, Error> listen_async(net::SocketAddr addr);
export Result<net::SocketAddr, Error> listener_addr(AsyncTcpListener listener);
export Result<void, Error> close_listener(AsyncTcpListener listener);
export Result<AsyncAccept, Error> accept_async(AsyncTcpListener listener);
export Result<AsyncConnect, Error> connect_async(net::SocketAddr addr);
export async Result<AsyncTcpStream, Error> accept(AsyncTcpListener listener);
export async Result<AsyncTcpStream, Error> connect(net::SocketAddr addr);
export async Result<AsyncTcpStream, Error> connect_timeout(net::SocketAddr addr, u64 ms);

export Result<void, Error> close_stream(AsyncTcpStream stream);
export Result<AsyncTcpSplit, Error> split(AsyncTcpStream stream);
export Result<void, Error> shutdown_read(AsyncTcpStream stream);
export Result<void, Error> shutdown_read_half(AsyncTcpReadHalf half);
export Result<void, Error> shutdown_write(AsyncTcpStream stream);
export Result<void, Error> shutdown_write_half(AsyncTcpWriteHalf half);
export Result<net::SocketAddr, Error> stream_local_addr(AsyncTcpStream stream);
export Result<net::SocketAddr, Error> stream_peer_addr(AsyncTcpStream stream);

export Result<AsyncTcpRead, Error> read_bytes(AsyncTcpStream stream, usize max_len);
export Result<AsyncTcpRead, Error> read_into_async(AsyncTcpStream stream, Bytes buffer);
export Result<AsyncTcpWrite, Error> write_bytes(AsyncTcpStream stream, Bytes data);
export Result<AsyncTcpWrite, Error> write_half_bytes(AsyncTcpWriteHalf half, Bytes data);
export async Result<Bytes, Error> read(AsyncTcpStream stream, usize max_len);
export async Result<ReadIntoResult, Error> read_into(AsyncTcpStream stream, Bytes buffer);
export async Result<usize, Error> write(AsyncTcpStream stream, Bytes data);
export async Result<usize, Error> write_half(AsyncTcpWriteHalf half, Bytes data);
export async Result<void, Error> write_all(AsyncTcpStream stream, Bytes data);
export async Result<void, Error> write_all_half(AsyncTcpWriteHalf half, Bytes data);

export Result<BufferedStreamReader, Error> buffered_reader(
    AsyncTcpReadHalf half,
    usize capacity
);
export Result<AsyncTcpReadHalf, Error> into_read_half(BufferedStreamReader reader);
export Result<AsyncBufferedRead, Error> read_buffered_async(
    BufferedStreamReader reader,
    usize max_len
);
export Result<AsyncBufferedRead, Error> read_exact_buffered_async(
    BufferedStreamReader reader,
    usize len
);
export async Result<Bytes, Error> read_buffered(
    BufferedStreamReader reader,
    usize max_len
);
export async Result<Bytes, Error> read_exact_buffered(
    BufferedStreamReader reader,
    usize len
);

export Result<void, Error> notify_accept_done<M: Message>(
    *const AsyncAccept op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<void, Error> notify_connect_done<M: Message>(
    *const AsyncConnect op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<void, Error> notify_read_done<M: Message>(
    *const AsyncTcpRead op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<void, Error> notify_write_done<M: Message>(
    *const AsyncTcpWrite op,
    *const actor::Actor<M> actor_handle,
    M message
);

export Result<AsyncTcpStream, Error> finish_accept(AsyncAccept op);
export Result<AsyncTcpStream, Error> finish_connect(AsyncConnect op);
export Result<Bytes, Error> finish_read(AsyncTcpRead op);
export Result<usize, Error> finish_write(AsyncTcpWrite op);
export Result<void, Error> cancel_accept(AsyncAccept op);
export Result<void, Error> cancel_connect(AsyncConnect op);
export Result<void, Error> cancel_read(AsyncTcpRead op);
export Result<void, Error> cancel_write(AsyncTcpWrite op);
export Result<void, Error> cancel_buffered_read(AsyncBufferedRead op);
```

`/std/async_net` provides awaitable TCP operations over nonblocking runtime
handles. `accept` and `connect` are `CancelSafe + Abortable`, so they can be
used directly with `timeout` and `select`. `read` returns zero-length `Bytes`
for EOF. `read_into` moves an owned `Bytes` buffer into the future and returns
the same owned buffer with the number of bytes read so hot loops can reuse
capacity without keeping a mutable slice live across await.

Raw TCP `read`, `read_into`, `write`, and `write_all` are `Abortable` but not
`CancelSafe`; they are rejected by `SelectableFuture` bounds. Task abort may
close or poison the stream to release a stuck operation, but a losing
`select`/`timeout` cannot keep using the same stream after possibly discarding
bytes, losing an owned buffer, or observing partial writes.

The `*_async`, `notify_*`, `finish_*`, and `cancel_*` functions are low-level
operation-token hooks for actor completion tests and direct operation
integration. Normal async application code should prefer `accept`, `connect`,
`read`, `write`, and the buffered reader helpers.

Selectable stream reads use `BufferedStreamReader`. The reader owns the read
half and a private buffer. `read_buffered` is `CancelSafe + Abortable` because
cancellation preserves already-read bytes inside that private buffer and abort
releases the pending read. The reader serializes or rejects overlapping reads.
It polls its user-space buffer before registering socket readiness, so a
previous read that drained the fd into the private buffer can make a later
`select` arm ready immediately.

```ciel
// /std/async_time
export import /std/result;
import /std/actor as actor;
import /std/message;

export struct AsyncSleep {
    *void handle;
}

export Result<AsyncSleep, Error> sleep_ms_async(u64 ms);
export async Result<void, Error> sleep_ms(u64 ms);
export Result<void, Error> notify_sleep_done<M: Message>(
    *const AsyncSleep op,
    *const actor::Actor<M> actor_handle,
    M message
);
export Result<void, Error> finish_sleep(AsyncSleep op);
export Result<void, Error> cancel_sleep(AsyncSleep op);
```

`/std/async_time` provides monotonic awaitable timers. `sleep_ms` is the normal
async timer API and is `CancelSafe + Abortable`. `sleep_ms_async`,
`notify_sleep_done`, `finish_sleep`, and `cancel_sleep` are low-level
operation-token hooks for direct actor-completion integration. Timer policy is
deliberately narrow:
protocol-specific heartbeat, missed-pong, retry, and deadline behavior belongs
in application code or in helpers such as `async::timeout`.

These modules are standard library API. They are not compiler intrinsics except
where this specification names `/std/meta` type metadata helpers or a runtime
hook.

## 19. C Interop and ABI

`extern "C"` declarations are C ABI declarations. C APIs require explicit
pointer nullability and view mutability: users write `*T`, `*const T`, `?*T`,
and `?*const T`. Standalone `const T` is not a Ciel source type. Ciel specifies
`extern "C"` as its C ABI spelling. C ABI callable types are named C ABI
functions and `extern "C" ... fn(...)` function-pointer types. Closure values
use the Ciel ABI.

Imported C functions are unsafe functions. They must be declared with an unsafe
C boundary, either as `unsafe extern "C" T name(...);` or inside an
`unsafe extern "C"` block. A top-level `export extern "C" T name(...) { ... }`
defines a C ABI symbol implemented in Ciel; it is not unsafe to call from Ciel
unless the declaration itself is marked `unsafe`. `export unsafe extern "C" {
... }` re-exports imported C declarations to Ciel importers; it does not define
the C symbols. `noescape` is allowed only on imported C function declarations,
so it appears in an unsafe C boundary. `extern "C"` functions may return
`never`; Ciel lowers that C ABI return type as `void` while treating calls as
non-fallthrough.
When an unsafe function is stored as a value, its function-pointer type keeps
the unsafe bit. Assigning it to an ordinary function-pointer type is rejected;
calling it through an unsafe function value requires `unsafe { ... }`.

A top-level `export extern "C" type name = "C spelling";` declares a C
spelling type. Inside an `extern "C"` block the ABI is inherited, so
`type name = "C spelling";` has the same meaning. The spelling string is
emitted as the C declaration spelling for that type. This is how `/std/c`
exposes prefixed public types such as `c_int`, `c_long`, `c_size_t`, and
`c_ssize_t` without assuming that they are identical to Ciel fixed-width
primitives.

Inside an `extern "C"` block, the block ABI applies to declared functions and to
nested function types in those declarations or type aliases unless a nested
function type has an explicit ABI. A safe `extern "C"` block may contain C
spelling type aliases and opaque declarations, but imported functions require
`unsafe extern "C"`.

```ciel
unsafe extern "C" {
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

Calls to imported C functions require `unsafe { ... }` and obey the Ciel
declaration exactly. A writable view may weaken to a read-only C parameter, but
a read-only view cannot satisfy a writable C parameter:

```ciel
unsafe extern "C" {
    void read_only(*const char s);
    void may_write(*char s);
}

[]const char text = "hello";
unsafe { read_only(text.ptr) }; // ok
unsafe { may_write(text.ptr) }; // error
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

```ciel
extern "C" type CHandle = "const struct CHandle";
```

For exported Ciel functions, generated prototypes preserve pointee `const`:

```ciel
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

```ciel
import /std/c as c;

#c_include "unistd.h"

unsafe extern "C" {
    c::c_ssize_t write(c::c_int fd, *const void buf, c::c_size_t count);
}
```

Function type ABI is explicit:

```ciel
i32 fn(i64)                    // Ciel ABI
extern "C" i32 fn(*void, *void) // C ABI
```

The Ciel internal ABI may lower large returns and arguments using hidden
pointers. Any declaration marked `extern "C"` or `export extern "C"` must obey
the target platform C ABI as written. By-value `void` parameters are invalid in
`extern "C"` declarations; an empty C parameter list is written by omitting
parameters.

Generated Ciel libraries expose a small host ABI:

```ciel
unsafe extern "C" {
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

## 20. Debug Information

A debug build emits target debug information through the generated C compiler.
The Ciel compiler:

1. preserves generated C files when requested
2. passes the target compiler's debug flag such as `-g`
3. emits `#line` directives mapping generated C back to Ciel source files
4. uses deterministic mangled names for generated C symbols
5. keeps a source-location table for runtime diagnostics such as panic messages

The minimum debug contract is source-line mapping, readable panic locations,
and deterministic generated names.

## 21. C Backend Lowering

Ciel keeps source-level value semantics. The generated C ABI for internal Ciel
functions may avoid large copies:

```ciel
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
