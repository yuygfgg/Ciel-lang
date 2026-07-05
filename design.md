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
source-level operation. When allocating GC-managed storage, the compiler may
also decide whether the storage needs GC scanning from the concrete runtime
layout.

The language uses value semantics for structs, enums, and fixed-size arrays.
Assignment is shallow field-wise or element-wise copy. Memory is GC-managed.
Non-memory resources use resource-affine wrapper values backed by runtime
resource owners; explicit close, lexical cleanup, scoped owners, and owner close
are part of one resource-management model.

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
`resource` is also contextual syntax. It is recognized as a modifier before
`struct` declarations and before generic parameter names; elsewhere it remains
an ordinary identifier. `derive` and `derivable` are contextual item-start
syntax. They remain ordinary identifiers outside item declarations.

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
                 | DerivableImplDecl
                 | DeriveDecl
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

Ciel has a single namespace for values, functions, types, interfaces, and
aliases. Function overloading is forbidden. Two visible bare declarations with
the same name are an error only when a bare use is ambiguous. Names inside
aliased imports are not bare declarations. Enum variants live under their enum
type and have a canonical qualified name such as `Result::Ok` or
`pkg::Result::Ok`.

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

StructDecl          ::= [ "resource" ] [ "unsafe" ] "struct" Identifier
                        [ GenericParamList ] StructBody
StructBody          ::= "{" { FieldDecl } "}"
FieldDecl           ::= Type Identifier ";"

EnumDecl            ::= "enum" Identifier [ GenericParamList ] EnumBody
EnumBody            ::= "{" [ VariantDecl { "," VariantDecl } [ "," ] ] "}"
VariantDecl         ::= Identifier [ "(" TypeList ")" ]

InterfaceDecl       ::= [ "unsafe" ] "interface" InterfaceGenericParamList
                        InterfaceSignature [ ReceiverSelector ] ";"
InterfaceSignature  ::= FunctionReturnType Identifier "(" [ ParamList ] ")"
InterfaceAliasDecl  ::= "interface" Identifier [ GenericParamList ]
                        "=" InterfaceExpr ";"

InterfaceExpr       ::= InterfaceTerm { ( "+" | "-" ) InterfaceTerm }
InterfaceTerm       ::= [ "!" ] Identifier [ TypeArgList ]

ImplDecl            ::= [ "unsafe" ] "impl" [ GenericParamList ] Identifier [ TypeArgList ]
                        "(" [ ParamList ] ")" Block
DerivableImplDecl   ::= [ "unsafe" ] "derivable" ImplDecl
DeriveDecl          ::= [ "unsafe" ] "derive" [ GenericParamList ]
                        Identifier TypeArgList ";"

FunctionDecl        ::= [ "unsafe" ] [ AbiSpec ] [ "async" ] FunctionSignature
                        [ ReceiverSelector ] ( Block | ";" )
FunctionSignature   ::= FunctionReturnType Identifier [ GenericParamList ]
                        "(" [ ParamList ] ")"
FunctionReturnType  ::= Type | OpaqueReturnType
OpaqueReturnType    ::= "_" ":" ConstraintExpr
ReceiverSelector    ::= "=" ( "." Identifier | Identifier "." Identifier )

GenericParamList    ::= "<" GenericParam { "," GenericParam } [ "," ] ">"
InterfaceGenericParamList
                    ::= "<" GenericParam { "," GenericParam }
                        [ "->" GenericParam { "," GenericParam } ]
                        [ "," ] ">"
GenericParam        ::= [ "resource" ] Identifier [ ":" ConstraintExpr ]
ConstraintExpr      ::= ConstraintTerm { ( "+" | "-" ) ConstraintTerm }
ConstraintTerm      ::= [ "!" ] Identifier [ ConstraintArgList ]
ConstraintArgList   ::= "<" ConstraintArg { "," ConstraintArg } [ "," ] ">"
ConstraintArg       ::= Type
                     | Identifier "=" "_" [ ":" ConstraintExpr ]

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

`derive` declarations and `derivable impl` templates are not exportable items.
They affect the current imported program by adding impls at explicit derive
sites.

`derivable impl` has the same signature shape as an ordinary impl, but it is an
inert template until a `derive` declaration requests a matching interface term.
The optional `unsafe` before `derivable` means instantiating the template
requires `unsafe derive`. The optional `unsafe` inside `ImplDecl` keeps its
ordinary meaning: the generated impl implements an unsafe interface.

In the first version, a public derive target must have exactly one type
argument, the receiver type:

```ciel
derive message::Message<Event>;
unsafe derive message::share_handle_marker<Handle>;
derive<T: message::Message> message::Message<Envelope<T>>;
```

The optional `GenericParamList` after `derive` introduces generic parameters
scoped to that derive declaration. These parameters are binders, not interface
arguments; they may appear in the receiver type supplied as the derive target
and may carry the same resource qualifiers and constraints as ordinary generic
parameters. For example, `derive<T: message::Message>
message::Message<Envelope<T>>;` requests a generic generated impl for every
`Envelope<T>` whose `T` satisfies the declared constraint.

A generic derive declaration is accepted only if the generated impl body is
well-typed under the constraints written on the derive declaration. If the
template body needs `T: message::Message`, then `derive<T>
message::Message<Envelope<T>>;` is rejected instead of registering an
under-constrained generic impl template for later capability solving.
Likewise, derived impls must satisfy the same receiver safety rules as ordinary
impls: affine or resource receiver types cannot derive message cloning or share
handle marker interfaces that would bypass resource ownership policy.

Interfaces with additional policy parameters expose one-argument interface
aliases for derivation. For example, a JSON module can publish
`interface Wire = Encode + Decode;` and users write `derive json::Wire<T>;`
instead of deriving the low-level multi-argument wire interfaces directly.

For each fully applied interface term, excluding the receiver type supplied by
the derive declaration, the imported program may contain at most one
`derivable impl` template. A type may either derive an interface term or provide
an explicit impl for that same term, but not both.

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

Struct and enum assignment is shallow field-wise copy for non-affine concrete
types. Fixed-size array assignment is element-wise copy when the element type is
non-affine. Slice assignment copies only the slice view. Assignment evaluates
the right-hand side first, then stores into the left-hand-side lvalue.
Returning a struct, enum, or array value uses the same value semantics at the
Ciel level; backend lowering may avoid physical copies.

A concrete type is resource-affine when it is declared `resource struct`, when
it is the canonical `/std/resource::Handle`, when it is a compiler-generated
future, or when it structurally contains a resource-affine field, enum payload,
array element, or closure capture. Resource-affine values are move-only and
are cleaned up by generated structural cleanup code when their live owner slot
goes out of scope. The full non-memory resource model is specified in
[Non-Memory Resource Management](#18-non-memory-resource-management).

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

This is local type-hole syntax. The same `_` token also appears in inferred
capability type bindings and opaque return types, but those forms are legal
only in their own grammar positions and have declaration-level scope.

Struct declarations do not define default field values. A struct value is
created by a named-field struct literal, by copying another value, by a
function return, or by C interop according to the declared ABI.
An `unsafe struct` is still copied and passed like an ordinary struct, but
constructing it with a struct literal or projecting one of its fields requires
`unsafe { ... }`.

A `resource struct` declares an owning resource wrapper. It is resource-affine.
A concrete `resource struct` must contain an owning resource field, unless it
is the canonical `/std/resource::Handle` leaf. A concrete non-resource struct
cannot store a resource-affine field. The `unsafe` modifier is independent:
`resource unsafe struct` is used when the wrapper representation itself has
unsafe invariants, such as a raw runtime handle field.

Generic parameters may be written as `resource T` to require a resource-affine
type argument. Ordinary `T` parameters can still be instantiated with resources
when the generic body is affine-correct; the `resource` marker is for APIs that
must expose a resource-only operation boundary in their signature.

Generic parameter bounds may bind inferred capability types with `Name = _`.
That syntax introduces hidden generic parameters for the declaration; the
central rules are in
[Inferred Capability Types](#inferred-capability-types).

At most one function body may exist for a given fully qualified name. A
non-`extern` function declaration ending in `;` is a prototype and must match
the eventual body exactly. `extern "C"` declarations do not require a Ciel body.

An `async` function is declared by writing `async` before the ordinary return
type:

```ciel
async Result<bytes::Bytes, async_net::AsyncNetError> read_frame(
    *const async_net::AsyncTcpStream stream
) {
    bytes::Bytes header = await async_net::read(stream, 8)?;
    usize len = decode_len(header)?;
    return await async_net::read(stream, len);
}
```

The written return type is the value produced when the function is awaited.
Calling an async function creates a first-class future whose concrete type is
compiler-generated and opaque. That generated type implements the standard
`Future<Out>` and `Awaitable<Out>` surface for the function's written output
type, with `Awaitable` determining `Out` from the future receiver.
Async functions may be declared or prototyped like ordinary Ciel functions, but
they cannot use a C ABI; exporting or importing an async `extern "C"` function
is rejected.

A Ciel function may hide its concrete source return type by writing
`_: ConstraintExpr` as the return type:

```ciel
_: Iterator<i64> range(i64 start, i64 end) {
    return Range{ start: start, end: end };
}
```

This is an opaque static return, not a dynamic interface value. The function
body chooses one concrete return type for each concrete generic instance, and
every normal return path must return that same concrete type. The concrete type
must satisfy the written positive and negative capability bounds after generic
substitution. Callers see only the opaque source type and the written bounds;
code generation lowers the value to the selected concrete type.

The opaque identity is keyed by the defining function and the concrete explicit
and hidden generic arguments. Two calls to the same opaque-returning function
with the same canonical arguments have the same source type. Opaque returns from
different functions are distinct even if their bodies return the same concrete
type. A value of opaque return type can satisfy static constraints proven by its
written bounds and can be coerced to an expected dynamic interface value through
the ordinary dynamic erasure path.

Opaque return types are rejected on `extern "C"` and exported C ABI functions,
interface declarations, and impl declarations. They cannot be named from outside
the defining function, and a bare `_` return type without a constraint remains
invalid.

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
                 | ReceiverSelectorSuffix
                 | ArrowSuffix
                 | IndexSuffix
                 | SliceSuffix
                 | TrySuffix
CallSuffix      ::= [ TypeArgList ] "(" [ ArgList ] ")"
FieldSuffix     ::= "." Identifier
ReceiverSelectorSuffix ::= "." QualifiedName
ArrowSuffix     ::= "->" Identifier
IndexSuffix     ::= "[" Expr "]"
SliceSuffix     ::= "[" [ Expr ] ".." [ Expr ] "]"
TrySuffix       ::= "?"

CallExpr        ::= PostfixExpr
ArgList         ::= Expr { "," Expr } [ "," ]

PrimaryExpr     ::= Identifier
                 | QualifiedName
                 | GenericItemExpr
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
GenericItemExpr ::= ( Identifier | QualifiedName ) TypeArgList
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
functions, types, enum variants selected by enum-qualified or expected-type
lookup, and interface names are resolved directly and are not captured.

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
task must satisfy the hidden `Message` obligations described in
[Async/Await](#16-asyncawait). The closure does not need to be manually retained
as a `: Message` closure unless the surrounding API requires an ordinary
messageable closure value.

A call suffix may call a function item, function-pointer value, or closure
value. Closure arguments are evaluated in source order, then the closure's
generated call function is invoked with its environment and the arguments.

A generic function item expression produces a concrete function item value from
a generic function template, for example `compare<Item>`. It is valid only for a
generic function item or qualified generic function item, not for locals,
closures, dynamic interface calls, method values, or arbitrary expressions. The
result has the substituted function-pointer type. When the type argument list
is followed by an argument list, the expression is a generic call such as
`f<T>(args)` instead.

Receiver selectors let a function or interface declaration expose one
receiver-call spelling while keeping the semantic model as an ordinary call.
The declaration syntax is `= .name` for the first parameter or
`= parameter.name` for a named receiver parameter:

```ciel
export usize hash_map_len<K: map_key, V>(*const HashMap<K, V> map) = .len;
export bool contains_entry<K, V>(K key, *const HashMap<K, V> map) = map.contains;
```

The selector name does not enter the bare function namespace and does not
create a method value. `map.len()` can desugar to `hash_map_len(&map)`, but
`len(map)` is still ordinary name lookup. `map.len` without a call remains
field access.

For a selector declared as `R f(P0 p0, ..., PI pI, ..., PN pN) = pI.name`,
`receiver.name(a0, ...)` desugars to an ordinary call to `f`. The receiver
expression fills `pI`; explicit receiver-call arguments fill every other
parameter slot in declaration order. For a non-first receiver parameter,
evaluation order follows the equivalent ordinary call after desugaring.

Only the receiver expression may be adapted during selector desugaring. If the
receiver expression is assignable to the receiver parameter as written, it is
passed as written. Otherwise, when the receiver parameter is a pointer view of
`T` and the receiver expression is an addressable `T`, the compiler may insert
`&receiver`. Writable pointer receivers require a writable receiver lvalue;
read-only pointer receivers require only an addressable receiver. Nullable
pointer widening and read-only view weakening then follow the ordinary
assignability rules. Non-receiver arguments are checked exactly like ordinary
call arguments.

Selector resolution chooses a desugaring target from visible callable
declarations with the requested selector name. It filters candidates only by
the declared receiver parameter and the receiver expression type; remaining
arguments, return type, and generic constraints do not participate in selector
choice. If exactly one candidate matches, the desugared function or interface
call is type-checked through the existing call path. If none match, the call is
an error; if more than one matches, the call is ambiguous.

Selectors follow the visibility of their callable declaration. Selectors from
unaliased imports are available to unqualified receiver calls. Selectors from
aliased imports stay behind the alias and are called with a qualified selector:

```ciel
import /std/map as map;

table.map::insert(key, value)?;
```

`obj.name(args)` first tries the existing callable-field interpretation. If
that field call type-checks, it wins. Qualified receiver calls such as
`obj.map::insert(args)` are selector calls only.

Declarations in the same module conflict when they expose the same selector
name for overlapping receiver type patterns. Pointer view differences such as
`T`, `*T`, `*const T`, `?*T`, and `?*const T` are not overload dimensions for
the same receiver root. Interface selectors use the existing interface-call
semantics after desugaring; selectors do not make a type implement an
interface, and `impl` declarations do not attach selectors.
Extern block function items cannot attach selectors directly; a Ciel wrapper
can expose a selector while preserving the unsafe C boundary.

Calling an async function or async closure produces a future value immediately;
it does not run the body to completion at the call site. `await future` is valid
only inside an async body or inside compiler-recognized async bridges such as
`async::block_on`. The operand must implement `Awaitable`, whose determined
output is named by the compiler as `Out`; the await expression has type `Out`.
If `Out` is `Result<T, E>`, ordinary `?` propagation composes after the
await:

```ciel
bytes::Bytes bytes = await async_net::read(&stream, 16384)?;
```

The full execution, task, cancellation, and frame-safety rules for async
functions, futures, and `await` are specified in
[Async/Await](#16-asyncawait).

`select` constructs a future that races a flat set of future expressions:

```ciel
usize result = await select {
    case bytes = reader::read_buffered(reader, 4096): handle(bytes);
    case slept = async_time::sleep_ms(100): timeout(slept);
};
```

Every arm future must produce the same selected result type through its arm
body. Fairness, cancellation-safety, timeout lowering, and selectable future
rules are specified in [Async/Await](#16-asyncawait).

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

An interface generic parameter list may contain one `->`. Parameters before the
arrow determine parameters after the arrow:

```ciel
interface<I -> Item> Next<Item> next(*I iter) = .next;
interface Iterator<Item> = next<Item>;
```

The arrow does not change the receiver rule: the first generic parameter is
still the receiver type, and written type arguments on impls, constraints,
aliases, and dynamic interface types still bind only the non-receiver
parameters. Determination is a uniqueness rule over the whole program. For a
concrete determinant tuple, there may be at most one concrete tuple of
determined parameters. These impls conflict:

```ciel
impl next<i64>(*Range iter) { ... }
impl next<u8>(*Range iter) { ... } // error
```

The determinant side may contain more than the receiver:

```ciel
interface<F, In -> Out> Out map_call(*F f, In value);
interface Mapper<In, Out> = map_call<In, Out>;
```

Generic impls are checked conservatively. If two generic impls may overlap on
the determinant side and could produce different determined parameters, the
program is rejected unless the existing coherence machinery can prove the
determinant sets are disjoint. Duplicate impls with the same determined
parameters are still rejected by the ordinary duplicate-impl rule; determined
parameters are not overloads.

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

Dynamic interface types require fixed non-receiver arguments and cannot contain
`Name = _` hidden bindings. Hidden bindings are compile-time generic
parameters, not dynamic interface payload.

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

### Inferred Capability Types

An inferred capability type is a hidden generic parameter introduced by a
positive static capability constraint using `Name = _`:

```ciel
struct Peekable<I: Iterator<Item = _>> {
    I inner;
    Item cached;
}
```

The identifier before `=` is the hidden generic parameter name in the
surrounding declaration. It is not a named argument for the constrained
interface or alias. Constraint argument lists remain positional:
`Iterator<Cached = _>` binds a hidden type named `Cached` in the same argument
slot that `Iterator<Item>` would occupy. Interface parameter names describe
slots; they do not have to be repeated at use sites.

The hidden parameter is in scope anywhere an explicit generic parameter of the
same declaration would be in scope: fields, enum payloads, type alias targets,
function signatures, impl bodies, and ordinary type references. Hidden
parameters are not part of source-level arity. `Peekable<Range>` is the source
spelling; the compiler's canonical instance includes the solved hidden
arguments so layout and monomorphization remain concrete.

When two constraints should infer independent types from the same interface
slot, they use distinct hidden names:

```ciel
struct Zip<A: Iterator<AItem = _>, B: Iterator<BItem = _>> {
    A iter_a;
    B iter_b;
    AItem a_val;
    BItem b_val;
}
```

When two constraints must share one inferred type, the first constraint binds
the hidden name and later constraints refer to that name as an ordinary type:

```ciel
struct SameItemZip<A: Iterator<Item = _>, B: Iterator<Item>> {
    A iter_a;
    B iter_b;
    Item cached;
}
```

A binding may carry an additional constraint on the hidden type:

```ciel
struct Flatten<I: Iterator<Inner = _: Iterator<Item = _>>> {
    I inner;
    Inner current;
}
```

The hidden name introduced by each binding must be unique in the declaration
and must not duplicate an explicit generic parameter. Repeating the same hidden
binding, such as `A: Iterator<Item = _>, B: Iterator<Item = _>`, is an error
because it would introduce `Item` twice. Later uses write `Item` as an ordinary
type name.

Named constraint bindings are valid only in positive static capability
constraints that introduce or check a generic environment, including nested
constraints attached to another hidden binding. They are rejected in negative
or removed constraint terms, dynamic interface types, interface alias
declarations, impl target argument lists, explicit call or type argument lists,
retained closure types, casts, opaque return constraints, and other ordinary
type contexts. Opaque return constraints may refer to hidden names that were
already bound by the function's generic parameter list; they may not introduce
new bindings.

Hidden parameters are solved only from positive determined capability facts.
During declaration checking, each hidden parameter must be derivable from
explicit parameters and already-derived hidden parameters by applying
determined interfaces. During instantiation, the compiler solves hidden
canonical arguments from concrete explicit arguments, the current generic
constraint environment, opaque return bounds when relevant, concrete impls,
and generic impls whose own constraints are satisfied. If a hidden parameter is
unsolved or ambiguous, instantiation is rejected.

```ciel
struct MapIter<I: Iterator<In = _>, F: Mapper<In, Out = _>> {
    I inner;
    F f;
    Out cached;
}
```

Here `In` is determined from `I: Iterator<In>`, and `Out` is determined from
`F, In: Mapper<In, Out>`. The names `In` and `Out` are hidden type names local
to `MapIter`; they are not required to match the generic parameter names
written by `Iterator` or `Mapper`. A constraint on an interface without
determined parameters cannot bind a hidden type:

```ciel
interface<T, U> bool related(*T value, U other);

struct Bad<T: related<U = _>> { // error
    U value;
}
```

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
`FieldRef<FieldType>`. Owned representation is a by-value copy shape and is
available only for non-resource-affine values. A resource-affine `T`, including
arrays, closures, structs, or enums that contain resource-affine values, cannot
be represented by `meta::Repr<T>`.

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
meta::Schema<T> schema<T>();
```

`as_ref_repr` creates read-only pointers to visible fields, enum payloads, or
closure captures. Its result has the same lifetime and actor-local behavior as
those pointers. For enums, projection switches on the active variant and returns
the corresponding `Coproduct` branch. If this projection reads the fields of an
`unsafe struct`, the call must appear in an `unsafe` block.

`into_repr` copies from a read-only source pointer into the owned
representation. It is rejected for resource-affine source types because copying
from `*const T` cannot transfer affine ownership. If owned structural expansion
reads the fields of an `unsafe struct`, the call must appear in an `unsafe`
block.

`from_repr` reconstructs a struct, enum, or concrete closure instance from the
owned representation by structural position. It is rejected for resource-affine
target types because owned representation values are copyable ordinary values.
If reconstruction builds an `unsafe struct` through structural expansion, the
call must appear in an `unsafe` block.

`meta::Schema<T>` is an instance-free structural schema marker. It normalizes to
ordinary `/std/meta` schema nodes without requiring an existing `T` value.
`schema<T>()` lowers to an ordinary schema value containing static field names,
variant names, payload indices, and `meta::Type` witnesses for both the source
type and the owned representation slot type.

For a visible struct, `Schema<T>` is an `HCons` list of `FieldSchema<FieldType,
FieldReprSlot>` nodes in declaration order:

```ciel
struct Packet {
    i64 id;
    bool ok;
}

meta::Schema<Packet>
// meta::HCons<
//     meta::FieldSchema<i64, i64>,
//     meta::HCons<meta::FieldSchema<bool, bool>, meta::HNil>
// >
```

The runtime schema value stores `"id"` and `"ok"` in the corresponding
`FieldSchema.name` fields. For a nested visible field, `FieldReprSlot` is the
same owned slot type that `meta::Repr<T>` uses. A field of visible type `Inner`
therefore has source type `Inner` and a representation slot equal to the
normalized type denoted by `meta::Repr<Inner>`, not the nominal `Inner` value.

For a visible enum, `Schema<T>` is an `HCons` list of
`VariantSchema<PayloadSchemaProduct>` nodes. The schema lists every variant,
while `Repr<T>` remains a `Coproduct` containing only the active variant:

```ciel
enum Token {
    Number(i64),
    End,
}

meta::Schema<Token>
// meta::HCons<
//     meta::VariantSchema<
//         meta::HCons<meta::PayloadSchema<i64, i64>, meta::HNil>
//     >,
//     meta::HCons<meta::VariantSchema<meta::HNil>, meta::HNil>
// >
```

`PayloadSchema<T, R>.index` is the zero-based payload position inside its
variant. Concrete closure schemas expose captures as `FieldSchema<T, R>` entries
named `capture#0`, `capture#1`, and so on. Fixed-size array schemas use the same
bounded `ArrayNil`, `ArrayChunk1` through `ArrayChunk16`, and balanced
`ArrayCat<L, R>` tree shape as owned array representation, but the leaves are
`ElementSchema<T, R>` values. Schema expansion uses the same static-array budget
as `meta::Repr<T>`.

Schema reflection enables generic deserialization. A decoder can obtain
`meta::Schema<T>`, walk parsed input against field and variant names, construct
`meta::Repr<T>`, and then call `meta::from_repr<T>`. Format-specific choices
such as JSON null handling, field rename/default policy, unknown-field behavior,
and variable-length container decoding are library policy rather than core
schema semantics.

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

The core mechanism remains explicit projection plus ordinary policy code.
Declaration-level derive templates can auto-emit those wrapper impls at explicit
`derive` sites, but they do not change the SOP representation.

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
the original nominal type itself to cross an actor or channel boundary derives
or writes an explicit `clone_message(*const T)` policy and must not mark the
type with a capability excluded by `Message`.

Owned representation recursively expands structs, enums, concrete closures, and
fixed-size arrays where no nominal policy boundary exists. A named field or
payload that already carries a nominal non-resource policy such as
`clone_message`, `ShareHandle`, or `ThreadLocal` remains a leaf. Resource-affine
types are not owned policy leaves; they are rejected instead of copied into
`meta::Repr<T>`. This preserves both positive and negative capability facts
through `meta::Repr<T>`. For example, a `ThreadLocal` handle inside
`meta::Repr<Event>` still blocks `Event` from satisfying `Message`. Concrete
closure instances are not opaque policy leaves; their standard-library
`clone_message` impl reflects captures through `meta::Repr<C>`.

Type normalization still preserves nominal resource-affine boundaries, including
compiler futures and resource-backed async values, so generic impl identity does
not change merely because a type argument mentions `meta::Repr<T>`. This
normalization boundary does not make the resource type an owned representation
leaf; attempting to build or consume `meta::Repr<T>` for that resource-affine
type remains rejected.

Structural metaprogramming is not a text macro system. It does not generate
Ciel source, paste tokens, or run before name resolution. The order is:

1. resolve imports and identify canonical `/std/meta` declarations
2. normalize `RefRepr<T>`, `Repr<T>`, and `Schema<T>` while lowering types in
   semantic analysis
3. type-check generic constraints and impl calls against the normalized types
4. lower `as_ref_repr`, `into_repr`, `from_repr`, and `schema`
5. run ordinary monomorphization, escape analysis, and C code generation

## 12. Enums and Pattern Matching

Enum variants are scoped under their enum type. Two variants in the same enum
cannot have the same name, but different enums may reuse variant names. The
canonical constructor and pattern name is `Enum::Variant`; for an enum from an
aliased import, write `alias::Enum::Variant`.

Bare variant names are convenience syntax. A bare variant resolves when exactly
one visible variant has that name, or when the expected enum type selects one
candidate. Return expressions, annotated local initializers, function arguments,
variant payloads, and `switch` case patterns provide expected types. If no
expected enum type is available and more than one visible enum has that variant
name, the program must use the qualified form.

Unit variants are written without parentheses:

```ciel
enum DigitError {
    DigitNonDecimal,
}

return Err(DigitError::DigitNonDecimal);
```

Inside an expression with expected type `DigitError`, `DigitNonDecimal` is also
accepted.

Payload variants are ordinary constructor calls:

```ciel
enum ConfigError {
    MissingPort,
    InvalidPort(i64),
}

return Err(ConfigError::InvalidPort(raw_port));
```

Inside an expression with expected type `ConfigError`, `InvalidPort(raw_port)`
is also accepted.

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

Panic is immediate process termination with a nonzero exit status and an
optional diagnostic. The current runtime uses exit code `101` for panic
termination. It does not unwind and does not run `defer` handlers. `defer` is
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
async Result<bytes::Bytes, async_net::AsyncNetError> read_frame(
    *const async_net::AsyncTcpStream stream
) {
    bytes::Bytes header = await async_net::read(stream, 8)?;
    usize len = decode_len(header)?;
    return await async_net::read(stream, len);
}

_ future = read_frame(&stream);
bytes::Bytes frame = await read_frame(&stream)?;
```

The concrete future type generated for `read_frame` is opaque. It implements
`Awaitable<Result<bytes::Bytes, async_net::AsyncNetError>>`, with that output
determined by the future receiver, and may also implement `CancelSafe` or
`Abortable` when its body proves the corresponding contract. Users can store a
future in a local, pass it to `async::spawn`, pass it to `select`, pass it to a
generic future combinator, or await it. Users cannot name the generated frame
type, inspect its layout, or reach into another task's frame through the future.

Compiler-generated futures are resource-affine, single-consumer values. Awaiting
or passing one by value consumes that future; awaiting it again is invalid.
Dropping a future that has not registered a pending operation is allowed.
Dropping or cancelling a pending future while the current task continues is
allowed only through a path that has proved `CancelSafe`; tearing down a
pending future because its owning task is terminating requires `Abortable`.

### Await

`await expr` is valid only inside an async body or inside a compiler-recognized
synchronous bridge such as `async::block_on`. The operand is evaluated exactly
once and must implement `Awaitable`; the compiler asks capability solving for
the determined output `Out`. The expression has type `Out`, and ordinary `?`
propagation composes after the await:

```ciel
bytes::Bytes bytes = await async_net::read(&stream, 16384)?;
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
ChannelPair<bytes::Bytes> ch = async::channel<bytes::Bytes>(1024)?;
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

Channel payloads cross task ownership and therefore the async channel API
requires `T: Message` on payload send, reservation, permit-send, and receive
operations. Channel endpoint liveness is
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

Every arm future must implement `SelectableFuture`, whose determined output is
the arm binding type `ArmOut`; this is the selectable view of `Awaitable`,
`CancelSafe`, and `Abortable`. The compiler and stdlib lower a select
expression to an internal select-set future that polls every arm once before
parking, so ready buffered data, completed tasks, channel messages, and expired
timers cannot be missed. Default `select` chooses fairly among all ready arms;
`biased select` is the explicit source-order priority form. Losing futures are
cancelled only after their `CancelSafe` contract permits it.

`async::timeout(future, ms)` is a convenience wrapper over the same model. It
races the operand with a timer and, on timeout, cancels only the waiting future.
It does not assume that an arbitrary protocol future can discard partial state.
Its operand must satisfy `SelectableFuture<Out = _>`, with `Out` determined
from the operand.

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
handles documented as async-frame opt-ins, direct local static read-only slices
such as string literals, and compiler-generated operation keys. `ShareHandle`
values opt into async-frame storage through the standard library's generic
marker implementation.

Safe code rejects the following values across `await`: raw pointers, nullable
raw pointers, mutable slices, borrowed read-only slices whose owner is not
syntactically static, thread-local handles, closures that capture forbidden
locals, and compound values whose transitive fields may contain those rejected
views or handles. In the first implementation, compound values containing slice
or reference-view fields are rejected across await unless the compiler has an
explicit canonical marker proof that the representation is owned and
frame-safe.

```ciel
[]const u8 view = buffer[0..n];
await async_time::sleep_ms(1)?;
use(view); // error: borrowed slice crosses await

[]const char msg = "start processing";
await async_time::sleep_ms(1)?;
print(msg); // ok: string-literal storage is static and read-only

[]const u8 magic = "PING";
await async_time::sleep_ms(1)?;
use_bytes(magic); // ok: the string literal is static byte storage
```

The compiler recognizes the canonical
`/std/message.async_frame_opt_in_marker` capability as an unsafe opt-in for
owned values whose structural fields are not directly visible to the
frame-safety walk. This marker is only a manual unsafe opt-in, not a public
async-frame predicate.
Implementing it asserts that storing the value in a suspended async frame is
valid, but it does not imply cross-thread shared mutation safety. Ordinary
users should fix the reported local, move the data into an owned value such as
`Bytes` or `ByteBuf`, or construct the non-message resource inside the task
that owns it.

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
asynchronous I/O should use the async/await surface in
[Async/Await](#16-asyncawait).

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
unsafe interface<T> bool async_frame_opt_in_marker(*const T value);

interface MessageInternal = clone_message;
interface ShareHandleInternal = share_handle_marker;
interface ThreadLocalInternal = thread_local_marker;

interface Message = MessageInternal + !ThreadLocalInternal;
interface ShareHandle = ShareHandleInternal + Message + !ThreadLocalInternal;
interface ThreadLocal = ThreadLocalInternal + !MessageInternal + !ShareHandleInternal;
```

The standard library provides derive templates for the structural nominal
`Message` policy and for explicit unsafe marker opt-ins:

```ciel
derive message::Message<Event>;
unsafe derive message::share_handle_marker<Handle>;
unsafe derive message::thread_local_marker<RawFd>;
unsafe derive message::async_frame_opt_in_marker<FrameLocalBuffer>;
```

`clone_message` constructs the value that will be owned by the receiver. It may
copy fields, allocate fresh backing storage, serialize and decode, duplicate a
shareable synchronized handle, intern immutable data, or report an error. It
must not duplicate affine resources. Implementing it is a safety contract, so
each implementation uses `unsafe impl`. Calling safe APIs that require
`T: Message` does not require an unsafe block.

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
that want structural nominal behavior derive `Message` explicitly:

```ciel
import /std/message as message;

struct Event {
    i64 value;
    bool ok;
}

derive message::Message<Event>;

Result<void, Error> send_event(*const Channel<Event> channel, Event event) {
    return channel_send(channel, event);
}
```

The owned representation remains the low-level structural boundary type when an
API intentionally exposes it:

```ciel
type EventMessage = meta::Repr<Event>;
```

An explicit user-defined impl is still the way to provide a custom nominal
message policy:

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

Each actor owns a child resource owner. `spawn_actor_state` runs its state
initializer inside that owner, actor dispatch installs it as the current owner,
and `join` closes the owner after accepted jobs drain. Resource handles opened
by actor-local state or handlers therefore become stale after actor join.

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
Result<void, ChannelError> channel_send<T: Message>(*const Channel<T> ch, T value);
Result<T, ChannelError> channel_recv<T: Message>(*const Channel<T> ch);

Result<T, AtomicError> atomic_load<T: AtomicValue>(*const Atomic<T> value, MemoryOrder order);
Result<void, AtomicError> atomic_store<T: AtomicValue>(
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

enum SyncWithError<E> {
    Sync(SyncError),
    Body(E),
}

interface<F, T -> R, E> Result<Update<T, R>, E> update_value(*const F f, T value);

Result<R, SyncWithError<E>> mutex_update<
    T,
    F: update_value<T, R = _, E = _: ErrorTrait>
>(
    *const Mutex<T> mutex,
    *const F f
);
```

`mutex_update` takes the current value, calls `update_value`, stores the
replacement value, unlocks, and returns the result. The updater and protected
value type determine the result type `R` and body error type `E`, so callers do
not pass those as separate source type arguments. Lock/runtime failures are
reported as `SyncWithError::Sync`; updater failures are reported as
`SyncWithError::Body(E)`. Implementations may optimize the storage path
internally, but the safe API exposes value replacement rather than a borrowed
interior pointer.

The actor model uses interfaces for capability classification:

```ciel
unsafe interface<T> Result<T, Error> clone_message(*const T value);
unsafe interface<T> bool share_handle_marker(*const T value);
unsafe interface<T> bool thread_local_marker(*const T value);
unsafe interface<T> bool async_frame_opt_in_marker(*const T value);

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
witness nor a share-handle marker. The standard library provides
`unsafe impl<T: ShareHandle> async_frame_opt_in_marker(*const T)` so
synchronized or immutable share handles opt into async-frame storage through
normal interface composition, not a user-facing alias.

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
`type_size<T>()`, `type_align<T>()`, and `type_needs_gc_scan<T>()`; the compiler
lowers those helpers to C `sizeof(T)`, `CIEL_ALIGNOF(T)`, and a boolean layout
fact describing whether values of `T` may contain GC-managed pointers.
Standard-library modules such as `/std/channel`, `/std/sync`, and
`/std/storage` pass that metadata to thin runtime hooks from ordinary Ciel code.
Actor spawning additionally generates dispatch thunks that let the runtime call
concrete handlers as `Result<S, Error>(S, M)` and store the next actor state.
The safety check remains ordinary `Message` conversion, not an actor-only
type-system rule.

For safe Ciel code, actor-local mutable data is reachable only from its actor,
cross-actor APIs call `clone_message`, conversion returns a receiver-owned value
or fails, and shared handles expose synchronized operations instead of interior
mutable pointers. This guarantee depends on correct compiler checks, correct
standard-library implementations, and trusted C wrappers honoring their declared
policies.

## 18. Non-Memory Resource Management

Non-memory resources use a two-layer model:

1. source-level resource-affine values prevent accidental copies, double
   close, and use after move;
2. runtime resource owners and registry entries provide deterministic cleanup,
   revocation, generation checks, quotas, and integration with tasks, actors,
   and async operation tokens.

Memory remains GC-managed. `resource` is not a manual-memory mode, a GC escape
hatch, or a user-defined finalizer mechanism. It is for host and runtime
capabilities whose lifetime must be represented separately from GC reachability:
files, sockets, native handles, async operation tokens, actor/task owners, and
similar runtime resources.

The public standard-library surface is `/std/resource`. Its core types are
`Handle`, `TransferToken`, `Limits`, and `ScopedError<E>`. Its core operations
are explicit close, transfer to parent/current owner, scoped owner creation,
and the unsafe in-place helpers used by resource wrapper internals. Concrete
modules such as `/std/io`, `/std/net`, `/std/async_io`, `/std/async_net`, and
`/std/async_time` expose typed `resource struct` wrappers over this substrate.

The runtime source of truth is a resource owner table. An owner contains
resource entries with an owner id, resource id, generation, kind, state, raw
host resource or runtime pointer, and close action. Owners form a hierarchy:
the process root owner, task and actor owners, and lexical owners created by
`resource::scoped` or `resource::scoped_async`. Opening or registering a
resource uses the current ambient owner. Closing an owner closes every live
entry still owned by that owner and then closes live child owners. Owner limits
track live resources, child owners, pending operations, and descriptors; a zero
limit field means the runtime default for that count.

Visible Ciel resource values are resource-affine wrapper values. The canonical
leaf is `/std/resource::Handle`, a revocable token containing owner id,
resource id, and generation. `resource struct` wrappers such as `/std/io::File`
and `/std/async_net::AsyncTcpStream` contain that leaf directly or through
other resource-affine fields. Copying a raw token does not copy or extend the
underlying resource. Closing an entry, moving it to another owner, or closing
its owner revokes old token copies; later operations validate the token against
the registry and fail through the module's ordinary error path if the token is
stale, closed, moved, wrong-kind, or over policy.

The type checker treats a concrete type as resource-affine when it is
`resource struct`, the canonical `Handle`, a compiler-generated future, or a
visible aggregate, array, or closure capture that contains a resource-affine
value. Resource-affine values are move-only. Moving a whole local, parameter,
enum payload, result payload, selected future output, or returned value
consumes the source slot; later reads are rejected. Safe code cannot copy a
resource-affine value, use an array repeat to duplicate it, move only a
resource subvalue out of an aggregate, replace a resource subfield in place,
erase it into a dynamic interface or boxed error, or send it through
clone-based task, actor, and channel boundaries. Unsafe code may use the raw
standard-library holes, but those holes must document and uphold uniqueness.

A by-value use of a resource-affine local is a source move. The compiler marks
the source slot as moved and rejects later reads, borrows, and cleanup-sensitive
uses:

```ciel
Result<void, Error> drive([]const char path) {
    io::File file = io::open_read(path)?;
    io::File moved = file;        // moves `file`

    [8]u8 @buffer = [0;];
    io::read(&file, buffer[..])?; // error: use of moved resource `file`

    io::close(moved)?;
    return Ok;
}
```

Aggregates containing resource-affine fields are themselves resource-affine.
Moving the whole aggregate is allowed; moving one resource field out of the
aggregate is rejected in safe code because it would leave the aggregate with a
partially-owned cleanup shape:

```ciel
resource struct Duplex {
    io::File read_side;
    io::File write_side;
}

Result<void, Error> split_bad(Duplex pair) {
    io::File read = pair.read_side; // error: cannot move resource subvalue
    io::close(read)?;
    return Ok;
}

Result<void, Error> move_whole(Duplex pair) {
    Duplex moved = pair; // ok: whole aggregate move
    return Ok;           // `moved` is cleaned up if still live
}
```

Resource-qualified generic parameters make an API require a resource-affine
type argument at the call boundary. Ordinary generic parameters may still be
instantiated with resource-affine types when the generic body does not copy or
otherwise violate affine rules.

```ciel
Result<void, Error> accept_resource<resource T>(T value) {
    return Ok; // `value` is cleaned up if still live
}

accept_resource<io::File>(io::open_read(path)?); // ok
accept_resource<i64>(1);                         // error
```

Generated code performs structural cleanup for live affine owner slots on
normal cleanup paths. A cleanup helper closes a live `Handle` through the
runtime registry and clears the token fields. It recursively cleans arrays,
struct fields, enum payloads, and closure captures; for generated futures,
cleanup aborts the future handle. Cleanup is structural and
compiler-generated. There is no public user-defined `drop` hook, and ordinary
GC-managed fields remain GC-managed.

`close` is the explicit early-release operation for resource wrappers. It
consumes the wrapper, closes the registry entry, revokes copied tokens, and
clears the unique owner slot. A close error reports the runtime failure but the
source resource value has still been consumed. Automatic cleanup is best
effort for error reporting; APIs that need a close error in the program result
should call the explicit close operation or use a scoped helper whose result
surface includes cleanup errors.

Typed close functions are ordinary consuming wrappers over the unsafe
raw-handle close helper. `/std/io::close` clears the unique local slot so later
generated cleanup cannot close the same handle again:

```ciel
*resource::Handle file_handle_mut(*File file) {
    return unsafe { &file->handle };
}

export Result<void, IoError> close(File @file) {
    switch (unsafe { resource::close_handle_in_place(file_handle_mut(&file)) }) {
        case Ok:
            return Ok;
        case Err(error):
            return Err(resource_error(error));
    }
}
```

`resource::scoped` and `resource::scoped_with_limits` create a child owner,
make it current for the callback body, run a `Result<R, E>` body, transfer the
whole body result to the parent owner when that result is resource-affine, and
then close the child owner. This transfer applies to resources in both `Ok(R)`
and `Err(E)` payloads. Resources not moved into the body result remain in the
child owner and are closed when the child owner closes. Setup, transfer, and
cleanup failures are returned as `ScopedError::Resource`; body failures are
returned as `ScopedError::Body(E)`.

`resource::scoped_async` and `resource::scoped_async_with_limits` run an async
body under a child owner across suspension points. They restore the previous
owner after constructing the future, restore the child owner around result
transfer, transfer the whole `Result<R, E>` before closing the child owner, and
close the child owner on completion or cancellation. The source-level private
hook used by `scoped_async` is a deliberate no-op fallback; code generation
recognizes the canonical hook and emits structural transfer code for affine
results.

A resource opened inside `resource::scoped` normally belongs to the child owner.
If the body returns it, code generation transfers the whole returned
`Result<R, E>` to the parent owner before closing the child owner:

```ciel
Result<io::File, resource::ScopedError<Error>> open_for_later([]const char path) {
    return resource::scoped<io::File, Error>(|| {
        io::File file = io::open_read(path)?;
        return Ok(file);
    });
}
```

The returned `File` remains usable by the caller. Any resource opened inside
the body and not moved into the returned result is closed when the scoped owner
closes:

```ciel
Result<void, resource::ScopedError<Error>> write_temp([]const char path) {
    return resource::scoped<void, Error>(|| {
        io::File file = io::create(path)?;
        io::write_text(&file, "temporary")?;
        return Ok; // `file` remains in the child owner and is closed here
    });
}
```

The transfer is applied to the whole result, not only the `Ok` branch. If `E`
is resource-affine, resources stored in `Err(E)` are also reattached to the
parent before the child owner closes.

`transfer_to_parent` is the explicit lifetime-extension operation. It moves the
underlying registry entry to the parent owner, returns a fresh token, and
revokes old token copies. Transfer fails without moving the source entry if the
handle is stale, the destination is closed or incompatible, or the destination
owner is over quota. `transfer_to_current` is the corresponding adoption
operation: it moves an open entry from an ancestor owner into the current owner,
or validates and returns it unchanged when it is already current.

The transfer interfaces operate on a borrowed wrapper and move the registry
entry, not the source variable. The returned wrapper contains the fresh token.
The old wrapper and any copied tokens become stale, so later operations on
those tokens return resource errors.

```ciel
Result<io::File, resource::ScopedError<Error>> promote([]const char path) {
    return resource::scoped<io::File, Error>(|| {
        io::File child_file = io::open_read(path)?;
        io::File parent_file = resource::transfer_to_parent(&child_file)?;

        // `child_file` still exists as a source value, but its token is stale.
        // Use `parent_file` from this point on.
        return Ok(parent_file);
    });
}
```

`transfer_to_current` is useful when a child owner needs to adopt an ancestor
resource so that the child owner will close it if it is not returned or
transferred back out:

```ciel
Result<void, resource::ScopedError<Error>> consume_in_child(io::File parent_file) {
    return resource::scoped<void, Error>(|| {
        io::File local = resource::transfer_to_current(&parent_file)?;
        [128]u8 @buffer = [0;];
        _ n = io::read(&local, buffer[..])?;
        return Ok; // `local` remains in the child owner and is closed here
    });
}
```

`TransferToken` is an affine wrapper for a handle that has already been moved
out of its source owner. It is not `Message` and cannot cross clone-based task,
actor, or channel boundaries. Safe code can move the token through lexical
returns and then call `claim_transfer_token`, which validates and adopts it
through `transfer_handle_to_current`. Stale tokens fail through ordinary
generation validation.

A typed wrapper can claim a token by adopting the raw handle into the current
owner and rebuilding the typed resource. This is still a wrapper-internal
operation because constructing the typed wrapper from `Handle` is unsafe:

```ciel
Result<File, resource::ResourceError> claim_file(resource::TransferToken token) {
    resource::Handle handle = resource::claim_transfer_token(token)?;
    return Ok(file_from_handle(handle));
}
```

The `*_in_place` helpers are unsafe standard-library holes. They require the
caller to pass the unique live owner slot and clear that slot exactly once.
They exist so resource wrapper internals can move, close, split, or rebuild raw
handles without exposing raw resource invariants to ordinary safe code. The
compiler recognizes canonical `/std/resource` owner hooks through
standard-library identity metadata, not by source path or by trusting a user
interface with the same spelling.

Typed resource wrappers implement transfer by copying the wrapper into a local
unsafe owner slot, transferring the raw handle in place, clearing that local
slot, and rebuilding a typed wrapper from the fresh handle:

```ciel
File file_from_handle(resource::Handle handle) {
    return unsafe { { handle: handle } };
}

impl transfer_to_parent(*const File value) {
    File @file = unsafe { *value };
    resource::Handle handle = unsafe {
        resource::transfer_handle_to_parent_in_place(file_handle_mut(&file))?
    };
    return Ok(file_from_handle(handle));
}

impl transfer_to_current(*const File value) {
    File @file = unsafe { *value };
    resource::Handle handle = unsafe {
        resource::transfer_handle_to_current_in_place(file_handle_mut(&file))?
    };
    return Ok(file_from_handle(handle));
}
```

This pattern is intentionally standard-library-internal. Safe user code should
use typed wrappers such as `io::close`, `resource::transfer_to_parent`, and
`resource::transfer_to_current`, not raw handle mutation.

## 19. Standard Library Boundary

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
`/std/async_net`, `/std/async_time`, `/std/message`, `/std/resource`,
`/std/meta`, `/std/actor`, `/std/channel`, `/std/sync`, `/std/atomic`,
`/std/codec`, `/std/ord`, `/std/math`, `/std/ascii`, `/std/parse`,
`/std/slice`, `/std/sort`, `/std/buf`, `/std/vec`, `/std/bytes`,
`/std/base64`, `/std/text`, `/std/map`, `/std/option`, `/std/wire`,
`/std/json`, `/std/iter`, `/std/shared_map`, `/std/time`, `/std/env`,
`/std/crypto`, and `/std/net`.
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
export Error error_with_context(Error source, []const char context) = .with_context;
export []const char error_message(*const Error error) = .message;
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

Any concrete type that implements `format_error` can be converted to
`/std/error.Error` in an expected-type context. The compiler inserts the
erasing conversion for `?`, `return Err(concrete)`, nested `Result` payloads,
function arguments, and local initializers. Ordinary source code should prefer
returning concrete error enums from reusable APIs and let the compiler erase
them only at application boundaries.

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

export T must<T, E>(Result<T, E> value) = .must;
export T expect<T, E>(Result<T, E> value, []const char message) = .expect;
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
export []const char u64_to_hex(u64 value);
export []const char u32_to_hex(u32 value);
export []const char u8_to_hex(u8 value);
```

The hexadecimal helpers return fixed-width lowercase ASCII without a `0x`
prefix: 16 digits for `u64`, 8 digits for `u32`, and 2 digits for `u8`.

```ciel
// /std/ord
import /std/meta as meta;

export interface<T> bool eq(*const T left, *const T right);
export interface<T> bool lt(*const T left, *const T right);
export interface ordered = eq + lt;

export interface<T> T abs(T value);
export interface<T> T max_value(meta::Type<T> tag);
export interface<T> T min_value(meta::Type<T> tag);

export T min<T: lt>(T a, T b);
export T max<T: lt>(T a, T b);
export T clamp<T: lt>(T value, T lo, T hi);
export bool ne<T: eq>(T a, T b);
export bool le<T: lt>(T a, T b);
export bool gt<T: lt>(T a, T b);
export bool ge<T: lt>(T a, T b);
```

`/std/ord` is the generic equality and ordering surface. The language does not
overload `==`, `<`, `>`, `<=`, or `>=` for generic type variables, so generic
code uses the ordinary interface functions `eq` and `lt`, plus the derived
helpers `ne`, `le`, `gt`, and `ge`. `eq` and `lt` are implemented for `bool`,
`char`, all integer types, `usize`, `f32`, and `f64`; floating-point
comparisons follow the built-in IEEE 754 operators, including NaN behavior.

`min`, `max`, `clamp`, and sorting require `lt`; binary search and equality
checks use `ordered` or `eq` as appropriate. `clamp` assumes the caller passes
`lo <= hi`. `abs` is implemented for signed integers and floating-point types.
Signed integer absolute value saturates at the corresponding positive maximum
when given the minimum value.

`max_value` and `min_value` are type-directed interfaces over
`meta::Type<T>`, called with `meta::type_tag<T>()`. They are implemented for
all integer and floating-point primitives. Floating-point bounds are finite
maximum and finite negative maximum values, not infinities.

```ciel
// /std/math
import /std/meta as meta;

#c_include "math.h"

export interface<T> T pi(meta::Type<T> tag);
export interface<T> T e(meta::Type<T> tag);
export interface<T> T tau(meta::Type<T> tag);
export interface<T> T ln2(meta::Type<T> tag);
export interface<T> T log2_e(meta::Type<T> tag);
export interface<T> T log10_e(meta::Type<T> tag);

export f64 sqrt_f64(f64 x);
export f64 cbrt_f64(f64 x);
export f64 sin_f64(f64 x);
export f64 cos_f64(f64 x);
export f64 tan_f64(f64 x);
export f64 asin_f64(f64 x);
export f64 acos_f64(f64 x);
export f64 atan_f64(f64 x);
export f64 atan2_f64(f64 y, f64 x);
export f64 sinh_f64(f64 x);
export f64 cosh_f64(f64 x);
export f64 tanh_f64(f64 x);
export f64 exp_f64(f64 x);
export f64 exp2_f64(f64 x);
export f64 log_f64(f64 x);
export f64 log2_f64(f64 x);
export f64 log10_f64(f64 x);
export f64 pow_f64(f64 base, f64 exp);
export f64 floor_f64(f64 x);
export f64 ceil_f64(f64 x);
export f64 round_f64(f64 x);
export f64 trunc_f64(f64 x);
export f64 fmod_f64(f64 x, f64 y);
export f64 hypot_f64(f64 x, f64 y);
export f64 fabs_f64(f64 x);

export f32 sqrt_f32(f32 x);
export f32 cbrt_f32(f32 x);
export f32 sin_f32(f32 x);
export f32 cos_f32(f32 x);
export f32 tan_f32(f32 x);
export f32 asin_f32(f32 x);
export f32 acos_f32(f32 x);
export f32 atan_f32(f32 x);
export f32 atan2_f32(f32 y, f32 x);
export f32 sinh_f32(f32 x);
export f32 cosh_f32(f32 x);
export f32 tanh_f32(f32 x);
export f32 exp_f32(f32 x);
export f32 exp2_f32(f32 x);
export f32 log_f32(f32 x);
export f32 log2_f32(f32 x);
export f32 log10_f32(f32 x);
export f32 pow_f32(f32 base, f32 exp);
export f32 floor_f32(f32 x);
export f32 ceil_f32(f32 x);
export f32 round_f32(f32 x);
export f32 trunc_f32(f32 x);
export f32 fmod_f32(f32 x, f32 y);
export f32 hypot_f32(f32 x, f32 y);
export f32 fabs_f32(f32 x);
```

`/std/math` is a safe Ciel wrapper over the platform C math library. The
wrappers return the C library result directly and preserve IEEE 754 NaN,
infinity, and domain-result behavior rather than converting math outcomes into
`Result`. Constants are exposed as type-directed interfaces, for example
`math::pi(meta::type_tag<f64>())`. Values computable from libm are defined in
terms of libm wrappers such as `acos(-1)` and `exp(1)`.

The build driver links generated executable and shared-library outputs with
`m` on Linux and macOS. This is a driver-level C link rule, not a special
compiler semantic for `/std/math`.

```ciel
// /std/ascii
export import /std/result;

export enum AsciiError {
    InvalidChar,
    InvalidDigitValue,
}

export bool char_is_digit(char c);
export bool char_is_alpha(char c);
export bool char_is_alnum(char c);
export bool char_is_whitespace(char c);
export bool char_is_upper(char c);
export bool char_is_lower(char c);
export bool char_is_hex_digit(char c);
export char char_to_upper(char c);
export char char_to_lower(char c);
export Result<u8, AsciiError> char_to_decimal_digit_value(char c);
export Result<u8, AsciiError> char_to_hex_digit_value(char c);
export Result<char, AsciiError> decimal_digit_value_to_char(u8 value);
export Result<char, AsciiError> hex_digit_value_to_char(u8 value);
```

`/std/ascii` classifies and converts byte-sized `char` values using ASCII
ranges only. It does not perform Unicode case mapping or decoding.

```ciel
// /std/parse
export import /std/result;
import /std/meta as meta;

export enum ParseError {
    Empty,
    InvalidChar(usize),
    Overflow,
    Underflow,
}

export interface<T> Result<T, ParseError> parse_number(
    meta::Type<T> tag,
    []const char text
);
```

`/std/parse` parses leading and trailing ASCII whitespace around decimal
numbers. Integer parsing accepts an optional `+` for signed and unsigned
targets and `-` only for signed targets. It reports `Overflow` and `Underflow`
against the selected result type's `/std/ord` bounds. Floating-point parsing
delegates to runtime C helpers, accepts decimal/exponent forms plus the C
special values such as `inf`, `infinity`, and `nan`, and reports range errors
as `Overflow` or `Underflow`. Implementations are provided for all integer
primitives, `usize`, `f32`, and `f64`.

```ciel
// /std/option
export enum Option<T> {
    None,
    Some(T),
}

export bool option_is_some<T>(Option<T> value);
export bool option_is_none<T>(Option<T> value);
export T option_unwrap_or<T>(Option<T> value, T fallback);
```

`/std/option` defines the canonical optional value type. `/std/lib` re-exports
it because optional values are baseline data modeling vocabulary. Format
libraries may attach their own policies to it; JSON maps `None` to `null` and
`Some(value)` to the value's ordinary JSON representation.

```ciel
// /std/base64
export import /std/result;
import /std/bytes as bytes;
import /std/text as text;

export enum Base64Error {
    InvalidLength,
    InvalidPadding,
    InvalidChar,
    Storage,
}

export Result<text::Text, Base64Error> encode([]const u8 data);
export Result<bytes::Bytes, Base64Error> decode([]const char text);
```

`/std/base64` implements the standard padded Base64 alphabet using `+`, `/`,
and `=` padding. It is reusable standard-library codec surface. 
JSON uses it for the default `Bytes` wire policy.

```ciel
// /std/wire
export import /std/result;
import /std/meta as meta;

export enum WireErrorKind {
    Eof,
    Syntax,
    TypeMismatch,
    MissingField,
    DuplicateField,
    UnknownField,
    UnknownVariant,
    InvalidValue,
    UnsupportedType,
    Storage,
}

export struct WireError {
    WireErrorKind kind;
    []const char path;
    []const char message;
    usize byte_offset;
    bool has_byte_offset;
}

export WireError wire_error(WireErrorKind kind, []const char message);
export WireError wire_error_at(WireErrorKind kind, usize byte_offset, []const char message);

export interface<T, W> Result<void, WireError> encode_value(*const T value, *W writer);
export interface<T, R> Result<T, WireError> decode_value(meta::Type<T> target, *R reader);
```

`/std/wire` is format-neutral. It defines the common error shape and the
opt-in encode/decode interfaces used by concrete formats. `WireError.path` is a
rendered logical path such as `$`, `$.field`, or `$[3]`; concrete format
helpers update it while walking structural fields, enum payloads, arrays, and
object values.

```ciel
// /std/json
export import /std/wire;
import /std/base64 as base64;
import /std/bytes as bytes;
import /std/map as map;
import /std/meta as meta;
import /std/option as option;
import /std/text as text;
import /std/vec as vec;
import /std/wire as wire;

export struct JsonNumber {
    text::Text raw;
}

export enum Value {
    Null,
    Bool(bool),
    Number(JsonNumber),
    String(text::Text),
    Array(vec::Vec<Value>),
    Object(vec::Vec<Value>),
}

export enum UnknownFieldPolicy {
    Deny,
    Ignore,
}

export struct Reader { []const char input; usize offset; }
export struct Writer { /* ByteBuf-backed writer state */ }

export Result<Value, wire::WireError> parse([]const char input);
export Result<text::Text, wire::WireError> stringify(*const Value value);

export Result<text::Text, wire::WireError> encode<T: wire::encode_value<Writer>>(*const T value);
export Result<T, wire::WireError> decode<T: wire::decode_value<Reader>>(
    meta::Type<T> target,
    []const char input
);

export interface<K> Result<text::Text, wire::WireError> encode_object_key(*const K key);
export interface<K> Result<K, wire::WireError> decode_object_key(
    meta::Type<K> target,
    text::Text key
);
export interface json_object_key = encode_object_key + decode_object_key;

export Reader reader([]const char input);
export Result<Writer, wire::WireError> writer_new();
export Result<text::Text, wire::WireError> writer_finish(Writer writer);

export Result<Value, wire::WireError> read_value(*Reader reader);
export Result<void, wire::WireError> write_value(*const Value value, *Writer writer);

export Result<void, wire::WireError> write_struct<S, R>(
    *const S schema,
    *const R repr,
    *Writer writer
);
export Result<text::Text, wire::WireError> encode_struct<S, R>(
    *const S schema,
    *const R repr
);
export Result<T, wire::WireError> decode_struct<T, S, R>(
    meta::Type<T> target,
    *const S schema,
    meta::Type<R> repr,
    []const char input
);
export Result<T, wire::WireError> decode_struct_value<T, S, R>(
    meta::Type<T> target,
    *const S schema,
    meta::Type<R> repr,
    *const Value value
);
export Result<T, wire::WireError> decode_struct_with_policy<T, S, R>(
    meta::Type<T> target,
    *const S schema,
    meta::Type<R> repr,
    []const char input,
    UnknownFieldPolicy unknown_fields
);

export Result<void, wire::WireError> write_enum<S, R>(
    *const S schema,
    *const R repr,
    *Writer writer
);
export Result<text::Text, wire::WireError> encode_enum<S, R>(
    *const S schema,
    *const R repr
);
export Result<T, wire::WireError> decode_enum<T, S, R>(
    meta::Type<T> target,
    *const S schema,
    meta::Type<R> repr,
    []const char input
);
export Result<T, wire::WireError> decode_enum_value<T, S, R>(
    meta::Type<T> target,
    *const S schema,
    meta::Type<R> repr,
    *const Value value
);
```

`/std/json` owns JSON parsing, stringification, reader/writer state, typed JSON
policies, and schema-guided structural helpers. The parser accepts all legal
JSON values into `json::Value`, including duplicate object names. Duplicate
handling is a typed policy decision: structural object decoding and map
decoding reject duplicate names where uniqueness is required.

`json::Value::Object` stores alternating key and value entries. Object keys in
the value tree are always `Value::String`; this representation preserves
duplicates and input order for low-level parsing and stringification tests. A
typed object decoder then decides whether duplicate names are an error.

`json::encode` and `json::decode` are the ordinary typed entry points:

```ciel
import /std/json as json;
import /std/meta as meta;
import /std/text as text;

i64 value = 42;
text::Text encoded = json::encode<i64>(&value)?;
// encoded == "42"

i64 decoded = json::decode(meta::type_tag<i64>(), "42")?;
```

`json::parse` and `json::stringify` expose the untyped JSON tree:

```ciel
json::Value value = json::parse("{\"ok\":true,\"ok\":false}")?;
text::Text text = json::stringify(&value)?;
```

This layer understands JSON syntax only. It does not reject duplicate object
names because duplicate names are valid JSON text.

The default typed policies are:

1. `json::Value` maps to the parsed JSON tree.
2. `bool` maps to JSON booleans.
3. signed and unsigned integer primitives plus `usize` map to JSON numbers with
   range checks.
4. `f32` and `f64` map to finite JSON numbers; non-finite values are rejected.
5. `char` maps to a one-byte JSON string.
6. `text::Text` maps to a JSON string.
7. `bytes::Bytes` maps to a padded Base64 JSON string through `/std/base64`.
8. `option::Option<T>` maps `None` to `null` and `Some(T)` to `T`.
9. `vec::Vec<T>` maps to a JSON array.
10. fixed arrays `[0]T` through `[16]T` map to exact-length JSON arrays.
11. `map::HashMap<K, V>` maps to a JSON object when `K: json_object_key` and
    `V` has a JSON value policy. Standard key policies are provided for
    `text::Text`, `char`, `bool`, integer primitives, and `usize`.

Concrete examples:

```ciel
import /std/bytes as bytes;
import /std/json as json;
import /std/map as map;
import /std/meta as meta;
import /std/option as option;
import /std/text as text;
import /std/vec as vec;

option::Option<i64> absent = option::None;
text::Text null_text = json::encode<option::Option<i64>>(&absent)?;
// null_text == "null"

bytes::Bytes payload = bytes::bytes_from_text("hi")?;
text::Text payload_text = json::encode<bytes::Bytes>(&payload)?;
// payload_text == "\"aGk=\""

char marker = 'Z';
text::Text marker_text = json::encode<char>(&marker)?;
// marker_text == "\"Z\""

vec::Vec<i64> values = json::decode(meta::type_tag<vec::Vec<i64>>(), "[1,2,3]")?;
```

Map keys are converted through `json_object_key`, then written as JSON object
member names:

```ciel
map::HashMap<i64, text::Text> table = map::hash_map_new<i64, text::Text>()?;
map::hash_map_insert<i64, text::Text>(&table, 7, text::text_copy("seven")?)?;

text::Text encoded = json::encode<map::HashMap<i64, text::Text>>(&table)?;
// encoded == "{\"7\":\"seven\"}" for this one-entry map.

map::HashMap<i64, text::Text> decoded = json::decode(
    meta::type_tag<map::HashMap<i64, text::Text>>(),
    "{\"7\":\"seven\"}"
)?;
```

Additional key types opt in by implementing both key interfaces:

```ciel
import /std/format as format;
import /std/parse as parse;

struct UserId {
    i64 raw;
}

impl json::encode_object_key(*const UserId key) {
    return text::text_copy(format::i64_to_string(key->raw));
}

impl json::decode_object_key(meta::Type<UserId> target, text::Text key) {
    i64 raw = parse::parse_number(meta::type_tag<i64>(), text::text_to_slice(key)?)?;
    return Ok({ raw: raw });
}
```

The type must also be usable as a `map::HashMap` key, so it needs the ordinary
`map::hash_key` and `map::key_eq` impls.

Visible structs and enums can opt into the default structural JSON policy in the
module where the type shape is visible:

```ciel
derive json::Wire<Packet>;
```

The derive declaration must live in a module where `Packet`'s fields are
visible. The JSON module cannot reflect imported private shape by itself.
Nested nominal types need their own wire policy when they appear inside
containers or other structural types:

```ciel
struct Inner {
    i64 id;
    text::Text label;
}

struct Packet {
    i64 sequence;
    [2]Inner items;
}
```

`Packet`'s structural helper can walk the array and field list, but each
`Inner` value is still a nominal leaf at the container policy boundary. A module
that owns `Inner` should use `derive json::Wire<Inner>;` or provide custom
`wire::encode_value` and `wire::decode_value` impls for `Inner`.

A caller may also use structural helpers directly without publishing a typed
wire impl:

```ciel
Packet packet = {
    sequence: 3,
    items: [inner(1, "a"), inner(2, "b")],
};

_ schema = meta::schema<Packet>();
_ repr = meta::as_ref_repr(&packet);
text::Text text = json::encode_struct(&schema, &repr)?;

Packet decoded = json::decode_struct(
    meta::type_tag<Packet>(),
    &schema,
    meta::type_tag<meta::Repr<Packet>>(),
    text::text_to_slice(text)?
)?;
```

Struct helpers encode source field names exactly by default. Decode denies
unknown fields:

```ciel
Result<Packet, wire::WireError> packet = json::decode_struct(
    meta::type_tag<Packet>(),
    &schema,
    meta::type_tag<meta::Repr<Packet>>(),
    "{\"sequence\":3,\"items\":[{\"id\":1,\"label\":\"a\"},{\"id\":2,\"label\":\"b\"}],\"extra\":true}"
);
// returns WireError { kind: UnknownField, path: "$", ... }
```

Callers that intentionally accept additive input fields use the policy-taking
helper:

```ciel
Packet packet = json::decode_struct_with_policy(
    meta::type_tag<Packet>(),
    &schema,
    meta::type_tag<meta::Repr<Packet>>(),
    "{\"sequence\":3,\"items\":[{\"id\":1,\"label\":\"a\"},{\"id\":2,\"label\":\"b\"}],\"extra\":true}",
    json::Ignore
)?;
```

Missing fields remain errors unless a hand-written wrapper or future metadata
policy supplies a default:

```text
{"items": []}
```

decoding as `Packet` reports `WireErrorKind::MissingField` at path
`$.sequence`.

Enums use one-object-member externally tagged JSON by default. Unit variants
encode as `{"Variant": null}` and payload variants encode as
`{"Variant": [payload0, ...]}`:

```ciel
enum Event {
    Ping,
    Data(i64, text::Text),
    Nested(Inner),
}

derive json::Wire<Event>;

Event ping = Event::Ping;
// json::encode<Event>(&ping)? == "{\"Ping\":null}"

Event data = Event::Data(11, text::text_copy("ok")?);
// json::encode<Event>(&data)? == "{\"Data\":[11,\"ok\"]}"
```

Errors carry logical paths when structural and container policies can attach
them:

```ciel
json::decode(meta::type_tag<vec::Vec<i64>>(), "[1,\"bad\"]");
// Err(WireError { kind: TypeMismatch, path: "$[1]", ... })

json::decode_struct(
    meta::type_tag<Packet>(),
    &schema,
    meta::type_tag<meta::Repr<Packet>>(),
    "{\"sequence\":\"bad\",\"items\":[]}"
);
// Err(WireError { kind: TypeMismatch, path: "$.sequence", ... })
```

The current path representation is a rendered string. It is deliberately
format-neutral in `/std/wire`; `/std/json` is responsible for constructing
field, index, variant, and payload segments while it decodes.

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
// /std/resource
export import /std/result;
import /std/async/core as async_core;

export resource unsafe struct Handle {
    u64 owner_id;
    u64 resource_id;
    u64 generation;
}

export resource unsafe struct TransferToken {
    Handle handle;
}

export struct Limits {
    usize max_resources;
    usize max_child_owners;
    usize max_pending_ops;
    usize max_descriptors;
}

export interface<T> Result<T, ResourceError> transfer_to_parent(*const T value);
export interface<T> Result<T, ResourceError> transfer_to_current(*const T value);

export enum ScopedError<E> {
    Resource(ResourceError),
    Body(E),
}

export Limits default_limits();
export unsafe Handle take_handle_in_place(*Handle handle);
export unsafe Result<void, ResourceError> close_handle_in_place(*Handle handle);
export Result<void, ResourceError> close(Handle @handle);
export unsafe Result<Handle, ResourceError> transfer_handle_to_parent_in_place(*Handle handle);
export Result<Handle, ResourceError> transfer_handle_to_parent(Handle @handle);
export unsafe Result<Handle, ResourceError> transfer_handle_to_current_in_place(*Handle handle);
export Result<Handle, ResourceError> transfer_handle_to_current(Handle @handle);
export Result<TransferToken, ResourceError> transfer_handle_to_parent_token(Handle handle);
export unsafe Result<Handle, ResourceError> claim_transfer_token_in_place(*TransferToken token);
export Result<Handle, ResourceError> claim_transfer_token(TransferToken @token);
export Result<R, ScopedError<E>> scoped<R, E: ErrorTrait>(Result<R, E> |()| body) = .scoped;
export Result<R, ScopedError<E>> scoped_with_limits<R, E: ErrorTrait>(
    Limits limits,
    Result<R, E> |()| body
) = .scoped_with_limits;
export async Result<R, ScopedError<E>> scoped_async<R, E: ErrorTrait>(
    async_core::Future<Result<R, E>> |()| body
);
export async Result<R, ScopedError<E>> scoped_async_with_limits<R, E: ErrorTrait>(
    Limits limits,
    async_core::Future<Result<R, E>> |()| body
);
```

The `/std/resource` APIs are the standard-library entry points for the
resource model specified in
[Non-Memory Resource Management](#18-non-memory-resource-management).

```ciel
// /std/io
export import /std/result;
import /std/resource as resource;

export enum OpenMode {
    Read,
    Write,
    Append,
}

export resource unsafe struct File {
    resource::Handle handle;
}

export interface<T> []const char to_string(*const T value);
export interface printable = to_string;

export enum IoWithError<E> {
    Io(IoError),
    ResourceCleanup(resource::ResourceError),
    Body(E),
}

export IoError last_error();
export Result<File, IoError> open_read([]const char path);
export Result<File, IoError> create([]const char path);
export Result<File, IoError> append([]const char path);
export Result<void, IoError> close(File @file);

export Result<R, IoWithError<E>> with_open<R, E: ErrorTrait>(
    []const char path,
    OpenMode mode,
    Result<R, E> |(File)| body
) = .with_open;

export Result<R, IoWithError<E>> with_open_read<R, E: ErrorTrait>(
    []const char path,
    Result<R, E> |(File)| body
) = .with_open_read;

export Result<R, IoWithError<E>> with_create<R, E: ErrorTrait>(
    []const char path,
    Result<R, E> |(File)| body
) = .with_create;

export Result<R, IoWithError<E>> with_append<R, E: ErrorTrait>(
    []const char path,
    Result<R, E> |(File)| body
) = .with_append;

export Result<usize, IoError> read(*const File file, []u8 out) = .read;
export Result<usize, IoError> write(*const File file, []const u8 data) = .write;
export Result<usize, IoError> write_text_once(*const File file, []const char text) = .write_text_once;
export Result<void, IoError> write_all(*const File file, []const u8 data) = .write_all;
export Result<void, IoError> write_text(*const File file, []const char text) = .write_text;

export []const char f32_to_string(f32 value);
export []const char f64_to_string(f64 value);

export Result<void, IoError> write_value<T: printable>(*const File file, T value) = .write_value;
export Result<void, IoError> write_format(*const File file, []const char fmt, []printable values) = .write_format;
export Result<void, IoError> print_value<T: printable>(T value);
export Result<void, IoError> println_value<T: printable>(T value);
export Result<void, IoError> eprint_value<T: printable>(T value);
export Result<void, IoError> eprintln_value<T: printable>(T value);
export Result<void, IoError> print([]const char fmt, []printable values);
export Result<void, IoError> println([]const char fmt, []printable values);
export Result<void, IoError> eprint([]const char fmt, []printable values);
export Result<void, IoError> eprintln([]const char fmt, []printable values);
```

`/std/io` is a blocking I/O API whose `File` values are resource-affine
wrappers over descriptors registered in the current resource owner. `open_read`,
`create`, and `append` register a descriptor and return a `File`; `close`
performs explicit early release through the common resource registry. The
scoped helpers are convenience wrappers over `resource::scoped`.

The scoped helpers return `IoWithError<E>` so open/setup failures, resource
cleanup failures, and callback body failures can be matched separately.

Every blocking I/O operation validates the file token before touching the OS
descriptor. `stdout`, `stderr`, and formatting helpers use borrowed registry
entries for process standard streams. Printable values are values that
implement `to_string`; printing functions convert values to `[]const char`
first, then write through a `File`.

`/std/io` provides `to_string` implementations for `[]const char`, `char`,
`bool`, all integer primitive widths, `usize`, `f32`, `f64`, and `/std/error`
`Error`. User code should prefer the `to_string` interface and `printable`
alias over calling decimal helper functions directly; fixed-width hexadecimal
formatting remains in `/std/format`.

Low-level raw descriptor interop lives in `/std/os/fd`:

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
export unsafe interface<T> bool async_frame_opt_in_marker(*const T value);

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
export bool type_needs_gc_scan<T>();

export struct Type<T> {}

export Type<T> type_tag<T>();

export struct RefRepr<T> {}
export struct Repr<T> {}
export struct Schema<T> {}

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

export struct FieldSchema<T, R> {
    []const char name;
    Type<T> source_ty;
    Type<R> repr_ty;
}

export struct PayloadRef<T> {
    usize index;
    *const T value;
}

export struct Payload<T> {
    usize index;
    T value;
}

export struct PayloadSchema<T, R> {
    usize index;
    Type<T> source_ty;
    Type<R> repr_ty;
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

export struct ElementSchema<T, R> {
    Type<T> source_ty;
    Type<R> repr_ty;
}

export struct VariantSchema<P> {
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
export Schema<T> schema<T>();
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
export Result<void, Error> send<T: Message>(*const Actor<T> actor, T value) = .send;
export Result<void, Error> stop<T: Message>(*const Actor<T> actor) = .stop;
export Result<void, Error> join<T: Message>(*const Actor<T> actor) = .join;
```

```ciel
// /std/channel
import /std/result;
import /std/message;
import /std/meta;

export struct Channel<T> {
    *void handle;
}

export Result<Channel<T>, ChannelError> make_channel<T: Message>();
export Result<void, ChannelError> channel_send<T: Message>(*const Channel<T> ch, T value) = .send;
export Result<T, ChannelError> channel_recv<T: Message>(*const Channel<T> ch) = .recv;
export Result<void, ChannelError> channel_close<T: Message>(*const Channel<T> ch) = .close;
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

export Result<Atomic<T>, AtomicError> make_atomic<T: AtomicValue>(T initial);
export Result<T, AtomicError> atomic_load<T: AtomicValue>(
    *const Atomic<T> atomic,
    MemoryOrder order
) = .load;
export Result<void, AtomicError> atomic_store<T: AtomicValue>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
) = .store;
export Result<T, AtomicError> atomic_exchange<T: AtomicValue>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
) = .exchange;
export Result<CompareExchange<T>, AtomicError> atomic_compare_exchange<T: AtomicValue>(
    *const Atomic<T> atomic,
    T expected,
    T desired,
    MemoryOrder success,
    MemoryOrder failure
) = .compare_exchange;
export Result<T, AtomicError> atomic_fetch_add<T: AtomicInteger>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
) = .fetch_add;
export Result<T, AtomicError> atomic_fetch_sub<T: AtomicInteger>(
    *const Atomic<T> atomic,
    T value,
    MemoryOrder order
) = .fetch_sub;
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

export enum SyncWithError<E> {
    Sync(SyncError),
    Body(E),
}

export interface<F, T -> R, E> Result<Update<T, R>, E> update_value(
    *const F f,
    T value
);

export Result<Mutex<T>, SyncError> make_mutex<T: Message>(T initial);
export Result<R, SyncWithError<E>> mutex_update<
    T: Message,
    F: update_value<T, R = _, E = _: ErrorTrait>
>(
    *const Mutex<T> mutex,
    *const F f
) = .update;
export Result<R, SyncWithError<E>> mutex_with<T, R: Message, E: ErrorTrait>(
    *const Mutex<T> mutex,
    Result<R, E> |(*T)| body
) = .with;
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
export import /std/resource;
export import /std/meta;
export import /std/actor;
export import /std/channel;
export import /std/sync;
export import /std/atomic;
export import /std/codec;
export import /std/ord;
export import /std/math;
export import /std/ascii;
export import /std/parse;
export import /std/slice;
export import /std/sort;
export import /std/buf;
export import /std/vec;
export import /std/bytes;
export import /std/text;
export import /std/map;
export import /std/iter;
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
export interface<T> Result<void, CodecError> put_be([]u8 out, T value);
export interface<T> Result<void, CodecError> put_le([]u8 out, T value);
export interface<T> Result<T, CodecError> get_be(meta::Type<T> tag, []const u8 data);
export interface<T> Result<T, CodecError> get_le(meta::Type<T> tag, []const u8 data);

export Result<[]u8, CodecError> encode_be<T: encoded_len + put_be>(T value);
export Result<[]u8, CodecError> encode_le<T: encoded_len + put_le>(T value);
```

```ciel
// /std/storage
export import /std/result;

export unsafe struct RawStorage<T> {
    []T storage;
}

export unsafe []T raw_from_ptr<T>(*void ptr, usize capacity);
export unsafe Result<RawStorage<T>, Error> raw_zeroed<T>(usize capacity);
export unsafe Result<RawStorage<T>, Error> raw_realloc_zeroed<T>(
    RawStorage<T> old,
    usize initialized,
    usize next_capacity
);
export []T raw_slice<T>(*RawStorage<T> storage);
export []const T raw_const_slice<T>(*const RawStorage<T> storage);
```

`/std/storage` is the unsafe growable-storage boundary used by trusted
standard-library containers. It is not re-exported by `/std/lib`; application
code should use safe container modules such as `/std/buf`, `/std/vec`, and
`/std/map`.

`RawStorage<T>` owns a GC-managed allocation whose descriptor length is the raw
capacity. The storage runtime uses `meta::type_needs_gc_scan<T>()` to select
scanned GC allocation for layouts that may contain GC-managed pointers and
atomic/no-scan GC allocation for pointer-free layouts. Zero-capacity storage
still has a valid non-null empty slice descriptor. `raw_zeroed<T>` allocates
`capacity` elements, checks the byte-size overflow for `capacity *
type_size<T>()`, and returns storage whose bytes are zeroed before the storage
is visible to Ciel code or the GC. `raw_realloc_zeroed` returns a new owner,
preserves the first `initialized` elements from `old`, and zeros any newly
allocated slots. It fails with `Error` when `initialized > next_capacity`, when
a byte-size overflow is detected, or when the runtime allocation primitive
reports an error. The unsafe caller must ensure that `initialized` is not
greater than the initialized prefix actually maintained in `old`.

`raw_slice` and `raw_const_slice` expose the full raw capacity. Safe containers
must separately track their initialized length and must not expose spare raw
slots as initialized values. A container operation that reallocates raw storage
may leave older interior pointers or slice views pointing at the previous GC
allocation; safe container APIs must document view stability and avoid relying
on such stale views internally.

`raw_from_ptr<T>` is a compiler-recognized intrinsic only for the canonical
`/std/storage` module. It constructs a typed slice descriptor from a non-null
runtime allocation pointer and an element capacity; ordinary user code cannot
define an equivalent trusted slice-construction primitive.

```ciel
// /std/slice
export import /std/result;
import /std/ord as ord;

export enum SliceError {
    NotFound,
    OutOfBounds,
}

export bool slice_equal<T: ord::eq>([]const T left, []const T right) = .equal;
export bool slice_equal_bytes([]const u8 left, []const u8 right);
export void slice_reverse<T>([]T items) = .reverse;
export usize slice_copy<T>([]T dst, []const T src);
export void slice_fill<T>([]T items, T value) = .fill;
export bool slice_contains<T: ord::eq>([]const T items, T needle) = .contains;
export Result<usize, SliceError> slice_index_of<T: ord::eq>(
    []const T items,
    T needle
) = .index_of;
export bool slice_is_sorted<T: ord::lt>([]const T items) = .is_sorted;
```

`/std/slice` contains generic helpers over built-in slice views. It does not
own the backing storage and does not extend any lifetime beyond the ordinary
slice value rules. `slice_copy` copies up to the shorter length and uses
temporary storage so overlapping source and destination views behave as if the
source prefix were first copied aside. `slice_equal`, `slice_reverse`,
`slice_fill`, `slice_contains`, `slice_index_of`, and `slice_is_sorted` are
exposed as receiver selectors on slice values.

```ciel
// /std/sort
export import /std/result;
import /std/ord as ord;

export enum SortError {
    Storage,
}

export void sort_by<T>([]T items, bool |(*const T, *const T)| less);
export void sort<T: ord::lt>([]T items);
export Result<void, SortError> sort_stable_by<T>(
    []T items,
    bool |(*const T, *const T)| less
);
export Result<void, SortError> sort_stable<T: ord::lt>([]T items);
export bool is_sorted<T: ord::lt>([]const T items);
```

`sort` and `sort_by` use an in-place introsort: median-of-three quicksort
partitioning, a heap-sort fallback at the recursion depth limit, and insertion
sort for small ranges. They are not stable and do not allocate. `sort_stable`
and `sort_stable_by` use merge sort with temporary raw storage and return
`SortError::Storage` if allocation fails. The comparator is an ordinary
function item or closure value with signature `bool |(*const T, *const T)|`.

```ciel
// /std/buf
import /std/result;
import /std/bytes as bytes;
import /std/iter as iter;
import /std/storage as storage;

export unsafe struct ByteBuf {
    storage::RawStorage<u8> storage;
    usize len;
}

export Result<ByteBuf, BufError> byte_buf_new(usize capacity);
export usize byte_buf_len(*const ByteBuf buf) = .len;
export void byte_buf_clear(*ByteBuf buf) = .clear;
export []const u8 byte_buf_slice(*const ByteBuf buf) = .slice;
export _: iter::Iterator<u8> byte_buf_iter(*const ByteBuf buf) = .iter;
export []u8 byte_buf_mut_slice(*ByteBuf buf) = .mut_slice;
export usize byte_buf_capacity(*const ByteBuf buf) = .capacity;
export Result<void, BufError> byte_buf_reserve(*ByteBuf buf, usize additional) = .reserve;
export Result<void, BufError> byte_buf_push_slice(*ByteBuf buf, []const u8 data) = .push_slice;
export Result<ByteBuf, BufError> byte_buf_from_slice([]const u8 data);
export Result<[]u8, BufError> byte_buf_spare_mut_slice(
    *ByteBuf buf,
    usize additional
) = .spare_mut_slice;
export Result<void, BufError> byte_buf_commit_tail(*ByteBuf buf, usize additional) = .commit_tail;
export Result<void, BufError> byte_buf_discard_prefix(*ByteBuf buf, usize count) = .discard_prefix;
export Result<bytes::Bytes, BufError> byte_buf_to_bytes(*const ByteBuf buf) = .to_bytes;
export Result<bytes::Bytes, BufError> byte_buf_freeze(ByteBuf @buf) = .freeze;
```

`/std/buf` provides a GC-backed growable byte buffer. `ByteBuf` is an unsafe
struct so safe application code cannot construct invalid internal descriptors;
callers use `byte_buf_new` and the exported operations. Slice-returning
functions expose views into the buffer's initialized prefix. `byte_buf_clear`
sets the initialized length to zero without releasing capacity, and
`byte_buf_reserve` grows while preserving existing bytes.
`byte_buf_iter` creates a read-only byte iterator over the initialized prefix
and is exposed through receiver selector `.iter()`.
`byte_buf_spare_mut_slice` and `byte_buf_commit_tail` support staged appends:
callers reserve writable tail space, fill it through the returned slice, then
commit the number of bytes actually initialized. This pattern is used by frame
readers that append incoming byte chunks into reusable buffers.
`byte_buf_discard_prefix` removes an initialized prefix and shifts the
remaining bytes down, which supports frame parsers that retain partial input
between async reads. `ByteBuf` implements `Message` by copying initialized
bytes into a fresh buffer and has an explicit unsafe async-frame opt-in marker,
but it is not a `ShareHandle`; mutation APIs require unique mutable access and
do not provide synchronization. `byte_buf_freeze` also copies initialized bytes
before constructing `Bytes`; safe code can keep older mutable slice views
alive, so safe freeze must not reuse the same backing storage for immutable
shareable bytes.

```ciel
// /std/vec
export import /std/result;
import /std/iter as iter;
import /std/meta as meta;
import /std/ord as ord;
import /std/slice as slice;
import /std/sort as sort;
import /std/storage as storage;

export enum VecError {
    CapacityOverflow,
    IndexOutOfBounds(usize, usize),
    Empty,
    NotFound,
    Runtime(i64),
}

export unsafe struct Vec<T> {
    storage::RawStorage<T> storage;
    usize len;
}

export Result<Vec<T>, VecError> vec_new<T>(usize capacity);
export usize vec_len<T>(*const Vec<T> vec) = .len;
export usize vec_capacity<T>(*const Vec<T> vec) = .capacity;
export Result<void, VecError> vec_reserve<T>(
    *Vec<T> vec,
    usize additional
) = .reserve;
export Result<void, VecError> vec_push<T>(*Vec<T> vec, T value) = .push;
export Result<*const T, VecError> vec_at<T>(
    *const Vec<T> vec,
    usize index
) = .at;
export Result<*T, VecError> vec_mut_at<T>(*Vec<T> vec, usize index) = .mut_at;
export void vec_clear<T>(*Vec<T> vec) = .clear;
export []const T vec_slice<T>(*const Vec<T> vec) = .slice;
export _: iter::Iterator<T> vec_iter<T>(*const Vec<T> vec) = .iter;
export []T vec_mut_slice<T>(*Vec<T> vec) = .mut_slice;
export Result<Vec<T>, VecError> vec_from_slice<T>([]const T source);
export Result<T, VecError> vec_pop<T>(*Vec<T> vec) = .pop;
export void vec_truncate<T>(*Vec<T> vec, usize len) = .truncate;
export Result<void, VecError> vec_extend<T>(*Vec<T> vec, []const T source) = .extend;
export Result<void, VecError> vec_insert<T>(*Vec<T> vec, usize index, T value) = .insert;
export Result<T, VecError> vec_remove<T>(*Vec<T> vec, usize index) = .remove;
export void vec_reverse<T>(*Vec<T> vec) = .reverse;
export void vec_sort<T: ord::lt>(*Vec<T> vec) = .sort;
export void vec_sort_by<T>(
    *Vec<T> vec,
    bool |(*const T, *const T)| less
) = .sort_by;
export Result<usize, VecError> vec_binary_search<T: ord::ordered>(
    *const Vec<T> vec,
    T needle
) = .binary_search;
export bool vec_equal<T: ord::eq>(*const Vec<T> left, *const Vec<T> right);

impl<T> iter::collect_new<T, VecError>(meta::Type<Vec<T>> collection, usize capacity) {
    return vec_new<T>(capacity);
}

impl<T> iter::collect_push<T, VecError>(*Vec<T> collection, T value) {
    vec_push<T>(collection, value)?;
    return Ok;
}
```

`/std/vec` provides a GC-backed growable sequence for arbitrary element types.
It is re-exported by `/std/lib`. `Vec<T>` is an unsafe struct so safe
application code cannot construct a value whose storage descriptor and
initialized length disagree; callers use `vec_new`, `vec_from_slice`, and the
exported operations.

The vector length is the initialized prefix. The capacity is the full length of
the underlying `RawStorage<T>` view. `vec_reserve` ensures room for
`additional` more initialized elements after the current tail, and `vec_push`
appends one value, growing with checked capacity arithmetic when needed.
Capacity arithmetic overflow returns `VecError::CapacityOverflow`. Runtime
allocation failures are reported as `VecError::Runtime(code)`.

`vec_at` and `vec_mut_at` return read-only and mutable pointers to initialized
items without forcing callers to first convert the vector into a slice and
without moving or cloning a generic `T`. Access outside the initialized prefix
returns `VecError::IndexOutOfBounds(index, len)`, where `len` is the current
initialized length. Pointers and slices returned from a vector are borrowed
views into its current backing storage and are stable only until the next
mutating vector operation that may replace or clear that storage.

`vec_slice` and `vec_mut_slice` expose only the initialized prefix, never spare
capacity. `vec_clear` resets the initialized length to zero while keeping the
capacity reusable; for pointer-containing vectors it also clears the backing
slots so removed elements are not retained by the vector's current storage.
`vec_iter` creates a read-only iterator over the initialized prefix and is
exposed through receiver selector `.iter()`. `vec_from_slice` copies the source
slice into a new vector.

`vec_pop` removes and returns the last initialized item, returning
`VecError::Empty` for an empty vector. `vec_truncate` drops the initialized
tail above a requested length and clears removed backing slots. `vec_extend`
appends a slice, `vec_insert` inserts at an index from `0..=len`, and
`vec_remove` removes at an index from `0..len` while shifting later items.
`vec_reverse`, `vec_sort`, and `vec_sort_by` operate on the initialized prefix.
`vec_binary_search` assumes that prefix is sorted according to `ord::ordered`
and returns `VecError::NotFound` when the needle is absent. `vec_equal`
compares initialized prefixes by element equality.

`VecError` implements `/std/error::ErrorTrait` through `format_error`, so a
`Result<T, VecError>` can be propagated with `?` into a `Result<U, Error>` by
the standard error-boxing rule. The initial messages are
`"vector capacity overflow"`, `"vector index out of bounds"`,
`"vector is empty"`, `"vector item not found"`, and `"vector runtime error"`.
New standard-library containers that need structured failures should prefer
exported module-specific enum errors with `ErrorTrait` implementations; the
enum variants preserve inspectable details, while callers can still erase them
into `/std/error::Error` at API boundaries.

`Vec<T>` implements `Message` exactly when `T: Message`. Its `clone_message`
implementation allocates a fresh vector and clones each initialized element
with `clone_message`; `Vec<T>` for non-`Message` element types does not satisfy
`Message`.

`Vec<T>` is the first standard target collection for `/std/iter::collect`.
Collection uses the generic `CollectTarget<Item, E>` capability: `collect_new`
constructs the vector and `collect_push` appends each item. Allocation,
growth, and capacity overflow failures are returned as `VecError`.

```ciel
// /std/map
import /std/result;
import /std/message;
import /std/storage as storage;

export interface<T> u64 hash_key(*const T value, u64 seed);
export interface<T> bool key_eq(*const T left, *const T right);
export interface map_key = hash_key + key_eq;

export unsafe struct HashMap<K, V> {
    storage::RawStorage<?*void> buckets;
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
    Found(V),
    Missing,
}

export enum PopResult<K, V> {
    Item(K, V),
    Empty,
}

export Result<HashMap<K, V>, MapError> hash_map_new<K: map_key, V>();
export usize hash_map_len<K: map_key, V>(*const HashMap<K, V> map) = .len;
export void hash_map_clear<K: map_key, V>(*HashMap<K, V> map) = .clear;
export Result<bool, MapError> hash_map_contains_key<K: map_key, V>(
    *const HashMap<K, V> map,
    K key
) = .contains_key;
export Result<GetResult<V>, MapError> hash_map_get<K: map_key, V: Message>(
    *const HashMap<K, V> map,
    K key
) = .get;
export Result<InsertResult<V>, MapError> hash_map_insert<K: map_key, V>(
    *HashMap<K, V> map,
    K key,
    V value
) = .insert;
export Result<RemoveResult<V>, MapError> hash_map_remove<K: map_key, V>(
    *HashMap<K, V> map,
    K key
) = .remove;
export Result<PopResult<K, V>, MapError> hash_map_pop_any<K: map_key, V>(
    *HashMap<K, V> map
) = .pop_any;
export enum MapWithError<E> {
    Map(MapError),
    Body(E),
}

export Result<R, MapWithError<E>> hash_map_with<K: map_key, V, R: Message, E: ErrorTrait>(
    *HashMap<K, V> map,
    K key,
    Result<R, E> |(*V)| body
) = .with;
```

Typical call sites write the key/value types at construction and rely on
generic inference from the typed map receiver afterward:

```ciel
_ @table = must(hash_map_new<u32, i64>());
must(table.insert(7 as u32, 10));
usize count = table.len();
```

`HashMap<K, V>` itself is the type witness for operations that take the map;
ordinary map operations do not need separate `meta::Type<T>` tag values.
`hash_map_get` returns a cloned value and therefore requires `V: Message`;
`hash_map_with` is the scoped mutable-access API for values that should not be
cloned. `hash_map_pop_any` removes one arbitrary entry, which is useful for
draining actor-local work queues and for implementing synchronized facades.

`/std/map` provides an actor-local mutable hash table. It uses separate
chaining with GC-backed nodes and a `RawStorage<?*void>` bucket array. `HashMap`
does not implement `Message`; code should send keys, values, snapshots, or
explicit messages rather than live map storage. Primitive key policies cover
`bool`, `char`, signed integer types, unsigned integer types, and `usize`.
Structural policies cover `/std/meta` product and sum nodes used by
`meta::RefRepr<T>` and `meta::Repr<T>`, so visible structs and enums can opt in
with explicit `hash_key` and `key_eq` wrappers that delegate to the structural
representation.

`HashMap<K, V>` intentionally does not expose a safe borrowed `.iter()` in the
alpha surface. A borrowed map iterator would need borrow/lifetime enforcement
to prevent `insert`, `remove`, or `clear` from invalidating outstanding entry
references. A snapshot iterator or entry-list API is possible, but it is a
separate fallible cloning design rather than a borrowed iteration entrypoint.

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
    Found(V),
    Missing,
}

export enum SharedMapPop<K, V> {
    Item(K, V),
    Empty,
}

export interface shared_map_key = map::map_key + Message;

export Result<SharedMap<K, V>, SharedMapError> shared_map_new<K: shared_map_key, V: Message>();
export Result<map::InsertResult<V>, SharedMapError> shared_map_insert<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key,
    V value
) = .insert;
export Result<SharedMapGet<V>, SharedMapError> shared_map_get<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key
) = .get;
export Result<SharedMapGet<V>, SharedMapError> shared_map_remove<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared,
    K key
) = .remove;
export Result<SharedMapPop<K, V>, SharedMapError> shared_map_pop_any<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared
) = .pop_any;
export Result<usize, SharedMapError> shared_map_len<K: shared_map_key, V: Message>(
    SharedMap<K, V> shared
) = .len;
```

`/std/shared_map` wraps an actor-local `HashMap` in a shareable `Mutex` handle.
Keys must be both `map_key` and `Message`, and values must be `Message`, because
operations clone values across the synchronized boundary. It is intended for
registries and routing tables shared by async tasks or actors, while
`/std/map` remains the cheaper actor-local storage primitive. It also does not
expose `.iter()` in the alpha surface: a live iterator would need to hold or
leak a mutex guard lifetime, while a snapshot iterator requires explicit
cloning semantics.

```ciel
// /std/iter
export import /std/result;
export import /std/error;
import /std/meta as meta;
import /std/ord as ord;

export enum Next<Item> {
    Item(Item),
    Done,
}

export interface<I -> Item> Next<Item> next(*I iter);
export interface Iterator<Item> = next<Item>;

export interface<F, In -> Out> Out map_call(*F f, In value);
export interface Mapper<In, Out> = map_call<In, Out>;

export interface<P, Item> bool filter_accept(*P predicate, *const Item value);
export interface Predicate<Item> = filter_accept<Item>;

export interface<C, Item -> E> Result<C, E> collect_new(
    meta::Type<C> collection,
    usize capacity
);
export interface<C, Item -> E> Result<void, E> collect_push(*C collection, Item value);
export interface CollectTarget<Item, E> = collect_new<Item, E> + collect_push<Item, E>;

export struct Range {
    i64 current;
    i64 end;
}

export struct Once<T> {
    Next<T> state;
}

export struct Empty<T> {
    bool done;
}

export struct SliceIter<T> {
    []const T items;
    usize index;
}

export struct Enumerated<Item> {
    usize index;
    Item value;
}

export struct Pair<Left, Right> {
    Left left;
    Right right;
}

export _: Iterator<i64> range(i64 start, i64 end);
export _: Iterator<T> once<T>(T value);
export _: Iterator<T> empty<T>();
export _: Iterator<T> slice_iter<T>([]const T items) = .iter;
export _: Iterator<Out> map<I: Iterator<In = _>, F: Mapper<In, Out = _>>(
    I iter,
    F mapper
) = .map;
export _: Iterator<Item> filter<I: Iterator<Item = _>, P: Predicate<Item>>(
    I iter,
    P predicate
) = .filter;
export _: Iterator<Item> take<I: Iterator<Item = _>>(I iter, usize limit) = .take;
export _: Iterator<Enumerated<Item>> enumerate<I: Iterator<Item = _>>(I iter) = .enumerate;
export _: Iterator<Pair<LeftItem, RightItem>> zip<
    Left: Iterator<LeftItem = _>,
    Right: Iterator<RightItem = _>
>(Left left, Right right) = .zip;
export _: Iterator<Item> chain<First: Iterator<Item = _>, Second: Iterator<Item>>(
    First first,
    Second second
) = .chain;
export _: Iterator<Item> flatten<I: Iterator<Inner = _: Iterator<Item = _>>>(I iter) = .flatten;

export usize count<I: Iterator<Item = _>>(I iter) = .count;
export Acc fold<I: Iterator<Item = _>, Acc>(
    I iter,
    Acc initial,
    Acc |(Acc, Item)| step
) = .fold;
export Result<C, E> collect<
    C: CollectTarget<Item = _, E = _: ErrorTrait>,
    I: Iterator<Item>
>(I iter) = .collect;
export Next<Item> find<I: Iterator<Item = _>, P: Predicate<Item>>(
    I iter,
    P predicate
) = .find;
export Next<Item> iter_min<I: Iterator<Item = _: ord::lt>>(I iter) = .min;
export Next<Item> iter_max<I: Iterator<Item = _: ord::lt>>(I iter) = .max;
export Next<Item> last<I: Iterator<Item = _>>(I iter) = .last;
export Next<Item> nth<I: Iterator<Item = _>>(I iter, usize index) = .nth;
export void for_each<I: Iterator<Item = _>>(I iter, void |(Item)| f) = .for_each;
export Next<usize> position<I: Iterator<Item = _>, P: Predicate<Item>>(
    I iter,
    P predicate
) = .position;
export bool any<I: Iterator<Item = _>, P: Predicate<Item>>(I iter, P predicate) = .any;
export bool all<I: Iterator<Item = _>, P: Predicate<Item>>(I iter, P predicate) = .all;
```

`/std/iter` provides static iterators whose item type is determined by the
iterator receiver through ordinary ICT capability solving. It has no compiler
std-id hook: duplicate or overlapping `next` impls are rejected by the general
determined-parameter coherence rules, and generic `Iterator<Item = _>` bounds
are solved by the same hidden binding machinery available to user code.
Adapter constructors return opaque constrained iterator types so callers do not
depend on nested private adapter structs.

The borrowed slice entrypoint is `slice_iter<T>([]const T)`, exposed through
receiver selector `.iter()` for `[]const T`. Iterator adapters and consumers
also expose receiver selectors: `.map`, `.filter`, `.take`, `.enumerate`,
`.zip`, `.chain`, `.flatten`, `.count`, `.fold`, `.find`, `.min`, `.max`,
`.last`, `.nth`, `.for_each`, `.position`, `.any`, `.all`, and `.collect`.
Selector calls and ordinary function calls are the same operations; selectors
do not introduce separate method semantics.

`iter_min`, `iter_max`, `last`, `nth`, `find`, and `position` return
`Next<T>`-shaped results rather than `Result`, because reaching the end of an
iterator is an expected data outcome, not an error. The exported names for
minimum and maximum are `iter_min` and `iter_max`, while their receiver
selectors are `.min()` and `.max()`.

`collect` consumes the remaining items of an iterator into a target collection
chosen by the expected result type or explicit type argument. A collection
target implements `CollectTarget<Item, E>` by providing `collect_new` on
`meta::Type<C>` and `collect_push` on `*C`. The standard implementation for
`Vec<T>` uses the same interface as any user-defined target collection.
Creation and push failures propagate as the target's concrete error type `E`;
for `Vec<T>`, that type is `VecError`. The exported function also has receiver
selector `.collect`, so both `iter::collect<vec::Vec<i64>>(items)` and
`items.iter::collect<vec::Vec<i64>>()` use the same operation. `range`,
`slice_iter`, `map`, `filter`, `take`, `chain`, `zip`, `enumerate`, and
`flatten` are ordinary `Iterator` values and can be collected through this
interface.

```ciel
// /std/time
import /std/result;

export Result<u64, TimeError> monotonic_ms();
export Result<void, TimeError> sleep_ms(u64 ms);
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

export Result<usize, EnvError> args_len();
export Result<[]const char, EnvError> arg(usize index);
```

`/std/env` exposes process command-line arguments as stable read-only character
slices. Index `0` is the host-provided executable argument. `arg` returns a
`EnvError` when the index is outside the current `args_len`. Environment
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

export []const char hash_algorithm_name(HashAlgorithm algorithm) = .name;
export []const char mac_algorithm_name(MacAlgorithm algorithm) = .name;

export Result<void, CryptoError> random_bytes([]u8 out);
export Result<SystemRng, CryptoError> system_rng();
export Result<void, CryptoError> rng_random_bytes(SystemRng rng, []u8 out) = .random_bytes;

export Result<usize, CryptoError> hash_once(
    []const char algorithm,
    []const u8 data,
    []u8 out
);

export Result<usize, CryptoError> hash_once_alg(
    HashAlgorithm algorithm,
    []const u8 data,
    []u8 out
) = .hash_once;

export Result<Hash, CryptoError> hash_new([]const char algorithm);
export Result<Hash, CryptoError> hash_new_alg(HashAlgorithm algorithm) = .new_hash;
export Result<void, CryptoError> hash_update(Hash hash, []const u8 data) = .update;
export Result<usize, CryptoError> hash_finish(Hash hash, []u8 out) = .finish;
export Result<void, CryptoError> hash_clear(Hash hash) = .clear;

export Result<usize, CryptoError> mac_once(
    []const char algorithm,
    []const u8 key,
    []const u8 data,
    []u8 out
);

export Result<usize, CryptoError> mac_once_alg(
    MacAlgorithm algorithm,
    []const u8 key,
    []const u8 data,
    []u8 out
) = .mac_once;

export Result<Mac, CryptoError> mac_new([]const char algorithm, []const u8 key);
export Result<Mac, CryptoError> mac_new_alg(MacAlgorithm algorithm, []const u8 key) = .new_mac;
export Result<void, CryptoError> mac_update(Mac mac, []const u8 data) = .update;
export Result<usize, CryptoError> mac_finish(Mac mac, []u8 out) = .finish;
export Result<void, CryptoError> mac_clear(Mac mac) = .clear;

export bool constant_time_eq([]const u8 left, []const u8 right);
```

`/std/crypto` exposes backend-neutral cryptographic operations backed by Botan's
C FFI in the first runtime. `random_bytes` uses the system CSPRNG directly.
`SystemRng` is an explicit reusable CSPRNG handle. One-shot and streaming hash
and MAC APIs write into caller-provided output buffers and return the number of
bytes written. A too-small output buffer returns `CryptoError`.

The recommended algorithm names are `SHA-256`, `SHA-384`, `SHA-512`,
`HMAC(SHA-256)`, `HMAC(SHA-384)`, and `HMAC(SHA-512)`. Application code should
prefer the enum-based `*_alg` helpers for those common algorithms. The
string-based entry points are still available for backend-neutral protocol
surfaces and compatibility with older peers; after rejecting empty names,
embedded NUL bytes, and overly long algorithm names, the runtime passes the
algorithm name through to Botan. HMAC keys shorter than 16 bytes are rejected.
When Botan reports an error, `/std/crypto` surfaces Botan's error description as
`CryptoError::Backend`.

`SystemRng` implements `Message` as a shareable handle because Botan's system
RNG is thread-safe. `Hash` and `Mac` are unsafe runtime-backed handle structs
and do not implement `Message`; application code should pass byte slices or
completed digest/MAC values across actor boundaries instead of live streaming
crypto contexts. `hash_clear` and `mac_clear` release their runtime handles;
later use of the cleared value returns an error.

```ciel
// /std/net
import /std/result;
import /std/resource as resource;

export enum AddressFamily {
    Ip4,
    Ip6,
}

export unsafe struct SocketAddr {
    *void handle;
}

export resource unsafe struct TcpListener {
    resource::Handle handle;
}

export resource unsafe struct TcpStream {
    resource::Handle handle;
}

export Result<SocketAddr, NetError> parse_addr([]const char text) = .parse_addr;
export Result<SocketAddr, NetError> resolve_tcp([]const char host, u16 port) = .resolve_tcp;
export Result<AddressFamily, NetError> addr_family(SocketAddr addr) = .family;
export Result<u16, NetError> addr_port(SocketAddr addr) = .port;
export Result<usize, NetError> addr_write(SocketAddr addr, []char out) = .write;
export Result<[]const char, NetError> addr_to_string(SocketAddr addr) = .to_string;

export Result<TcpListener, NetError> tcp_listen(SocketAddr addr) = .listen;
export Result<TcpStream, NetError> tcp_accept(*const TcpListener listener) = .accept;
export Result<TcpStream, NetError> tcp_connect(SocketAddr addr) = .connect;
export Result<TcpStream, NetError> tcp_connect_host([]const char host, u16 port);
export Result<usize, NetError> tcp_read(*const TcpStream stream, []u8 out) = .read;
export Result<usize, NetError> tcp_write(*const TcpStream stream, []const u8 data) = .write;
export Result<void, NetError> tcp_write_all(*const TcpStream stream, []const u8 data) = .write_all;
export Result<void, NetError> tcp_shutdown_read(*const TcpStream stream) = .shutdown_read;
export Result<void, NetError> tcp_shutdown_write(*const TcpStream stream) = .shutdown_write;
export Result<void, NetError> tcp_shutdown(*const TcpStream stream) = .shutdown;
export Result<void, NetError> tcp_close(TcpStream @stream) = .close;
export Result<void, NetError> listener_close(TcpListener @listener) = .close;
export Result<SocketAddr, NetError> listener_addr(*const TcpListener listener) = .addr;
export Result<SocketAddr, NetError> stream_local_addr(*const TcpStream stream) = .local_addr;
export Result<SocketAddr, NetError> stream_peer_addr(*const TcpStream stream) = .peer_addr;

export enum NetWithError<E> {
    Net(NetError),
    ResourceCleanup(resource::ResourceError),
    Body(E),
}

export Result<R, NetWithError<E>> with_tcp_connect<R, E: ErrorTrait>(
    SocketAddr addr,
    Result<R, E> |(TcpStream)| body
) = .with_connect;

export Result<R, NetWithError<E>> with_tcp_connect_host<R, E: ErrorTrait>(
    []const char host,
    u16 port,
    Result<R, E> |(TcpStream)| body
) = .with_tcp_connect;

export Result<R, NetWithError<E>> with_tcp_listen<R, E: ErrorTrait>(
    SocketAddr addr,
    Result<R, E> |(TcpListener)| body
) = .with_listen;
```

`/std/net` provides a blocking TCP socket layer over the platform socket API.
It does not introduce a third-party networking dependency. `parse_addr` parses
numeric IPv4 and bracketed numeric IPv6 endpoints such as `127.0.0.1:8080` and
`[::1]:8080`; it does not perform DNS. Domain-name lookup is explicit through
`resolve_tcp`, and `tcp_connect_host` resolves a host name and tries the
returned TCP addresses until one connects.

`SocketAddr` is an immutable runtime-backed address value and implements
`Message` as a shareable handle. `TcpListener` and `TcpStream` are
resource-affine wrappers over blocking socket descriptors and are actor-local
blocking resources. The scoped `with_tcp_*` helpers follow the `/std/io`
pattern and are convenience wrappers over `resource::scoped`.

The scoped `with_tcp_*` helpers return `NetWithError<E>` so network setup,
resource cleanup, and body failures remain separate matchable domains.

```ciel
// /std/bytes
export import /std/result;
import /std/iter as iter;
import /std/storage as storage;

export unsafe struct Bytes {
    []const u8 data;
}

export Result<Bytes, BytesError> bytes_empty();
export Result<Bytes, BytesError> bytes_copy([]const u8 data);
export Result<Bytes, BytesError> bytes_from_text([]const char text);
export Result<Bytes, BytesError> bytes_concat([]const u8 left, []const u8 right);
export Result<Bytes, BytesError> bytes_prepend([]const u8 prefix, Bytes bytes) = bytes.prepend;
export Result<Bytes, BytesError> bytes_append(Bytes left, Bytes right) = .append;
export Result<Bytes, BytesError> bytes_slice(Bytes bytes, usize offset, usize len) = .slice;
export usize bytes_len(Bytes bytes) = .len;
export []const u8 bytes_const_slice(Bytes bytes) = .const_slice;
export _: iter::Iterator<u8> bytes_iter(Bytes bytes) = .iter;
export Result<usize, BytesError> bytes_copy_to(Bytes bytes, []u8 out) = .copy_to;
export Result<usize, BytesError> bytes_copy_to_chars(Bytes bytes, []char out) = .copy_to_chars;
export Result<[]u8, BytesError> bytes_to_slice(Bytes bytes) = .to_slice;
export unsafe Bytes bytes_from_raw_storage(storage::RawStorage<u8> raw, usize len);
```

`Bytes` is the standard immutable owned byte buffer used by text, async file
I/O, async TCP, and package APIs. It is backed by
`/std/storage.RawStorage<u8>` and exposes only read-only views; mutable reuse is
handled by `/std/buf.ByteBuf`. `Bytes` implements `Message` through an explicit
standard-library clone policy and implements `ShareHandle` because it exposes
only immutable views. `ShareHandle` values opt into async-frame storage through
the standard-library `async_frame_opt_in_marker` impl. Async modules import
`/std/bytes` in their signatures; there is no separate async-specific bytes
public namespace. `bytes_iter` is exposed as `.iter()` and yields `u8` byte
items, matching the byte-sequence contract.

```ciel
// /std/text
export import /std/result;
import /std/bytes as bytes;
import /std/iter as iter;

export struct Text {
    bytes::Bytes bytes;
}

export enum TextError {
    Bytes(bytes::BytesError),
    Storage,
    ShortCopy,
    NotFound,
}

export Result<Text, TextError> text_empty();
export Result<Text, TextError> text_copy([]const char text);
export usize text_len(Text text) = .len;
export Result<bytes::Bytes, TextError> text_to_bytes(Text text) = .to_bytes;
export _: iter::Iterator<char> text_chars(Text text) = .chars;
export Result<[]char, TextError> text_to_chars(Text text) = .to_chars;
export Result<[]const char, TextError> text_to_slice(Text text) = .slice;
export Result<Text, TextError> text_concat(Text left, Text right) = .concat;
export bool text_equal(Text left, Text right);
export bool text_equal_slice(Text left, []const char right);
export bool text_starts_with(Text text, Text prefix);
export bool text_ends_with(Text text, Text suffix);
export bool text_contains(Text text, Text needle);
export Result<usize, TextError> text_find(Text text, Text needle) = .find;
export Result<Text, TextError> text_trim(Text text) = .trim;
export Result<Text, TextError> text_to_upper_ascii(Text text) = .to_upper_ascii;
export Result<Text, TextError> text_to_lower_ascii(Text text) = .to_lower_ascii;
export Result<Text, TextError> text_from_bool(bool value);
export Result<Text, TextError> text_from_i64(i64 value);
```

`/std/text` wraps immutable owned bytes as text-oriented data. It does not yet
perform Unicode normalization or validation beyond preserving byte contents.
`Text` implements `Message` as a shareable handle, so it is suitable for actor
and async-task payloads. Conversion helpers copy the contents out when mutable
or slice inspection is needed. `text_chars` is exposed as `.chars()` and
iterates the stored UTF-8 bytes as `char` code units; it does not perform
Unicode scalar decoding.

Text search and comparison functions operate byte-for-byte. `text_trim` removes
leading and trailing ASCII whitespace. ASCII case conversion changes only bytes
in the ASCII letter ranges and leaves all other bytes unchanged. The conversion
helpers allocate new owned `Text` values rather than exposing mutable access to
the underlying `Bytes` handle. `text_find` returns `TextError::NotFound` when
the needle is absent. `text_concat`, `text_find`, `text_trim`,
`text_to_upper_ascii`, and `text_to_lower_ascii` are exposed as receiver
selectors.

```ciel
// /std/async
export import /std/async/core;
import /std/async/internal/adapter as adapter;
import /std/async/internal/runtime_future;

export resource unsafe struct Future<T> {
    *void handle;
}

export unsafe interface<A -> Out> *void awaitable_future(*const A awaitable);
export interface Awaitable<Out> = awaitable_future<Out>;

export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;
export interface SelectableFuture<Out> = Awaitable<Out> + CancelSafe + Abortable;

export Out block_on<A: Awaitable<Out = _> + Abortable>(A future);

export resource unsafe struct OperationFutureAdapter<Op, Out> {
    Future<Result<Out, AsyncError>> inner;
}

export OperationFutureAdapter<Op, Out> future_from_op<
    resource Op: adapter::OperationFuture<Out = _>
>(Op op);

export AsyncError timeout_error();
export AsyncError channel_closed_error();

export struct Task<T> {
    *void handle;
}

export Result<Task<T>, AsyncError> spawn<T, A: Awaitable<Result<T, Error>> + Abortable>(
    A body
);
export Result<void, AsyncError> cancel<T>(*const Task<T> task) = .cancel;
export Result<bool, AsyncError> is_finished<T>(*const Task<T> task) = .is_finished;

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

export resource unsafe struct ChannelSendFuture<T> {
    Future<Result<void, AsyncError>> inner;
}

export resource unsafe struct ChannelReserveFuture<T> {
    Future<Result<SendPermit<T>, AsyncError>> inner;
}

export resource unsafe struct ChannelRecvFuture<T> {
    Future<Result<T, AsyncError>> inner;
}

export Result<ChannelPair<T>, AsyncError> channel<T>(usize capacity);
export ChannelSendFuture<T> send<T: Message>(Sender<T> sender, T value);
export Result<void, AsyncError> try_send<T: Message>(Sender<T> sender, T value) = .try_send;
export ChannelReserveFuture<T> reserve<T: Message>(Sender<T> sender);
export Result<void, AsyncError> permit_send<T: Message>(SendPermit<T> permit, T value) = .send;
export ChannelRecvFuture<T> recv<T: Message>(Receiver<T> receiver);
export Result<void, AsyncError> close<T>(Sender<T> sender) = .close;
export Result<void, AsyncError> close_receiver<T>(Receiver<T> receiver) = .close;

export struct TaskGroup<T> {
    *void handle;
}

export Result<TaskGroup<T>, AsyncError> task_group<T>();
export Result<void, AsyncError> group_add<T>(*const TaskGroup<T> group, Task<T> task) = .add;
export async Result<T, Error> group_next<T>(*const TaskGroup<T> group) = .next;
export Result<void, AsyncError> group_cancel_all<T>(*const TaskGroup<T> group) = .cancel_all;
export Result<void, AsyncError> group_close<T>(*const TaskGroup<T> group) = .close;

export enum TaskGroupError<E> {
    TaskGroupAsync(AsyncError),
    TaskGroupBody(E),
    TaskGroupCleanup(AsyncError),
    TaskGroupBodyCleanup(E, AsyncError),
}

export async Result<R, TaskGroupError<E>> with_task_group<T: Message, R, E: ErrorTrait>(
    Future<Result<R, E>> |(*const TaskGroup<T>)| body
) = .with_task_group;

export async Result<Out, AsyncError> timeout<A: SelectableFuture<Out = _>>(
    A future,
    u64 ms
);
```

`/std/async` is the user-facing async/await surface. `Future<T>` is a
resource-backed, one-shot runtime future handle. It is resource-affine
regardless of `T`, because the suspended frame or runtime context may own
resources even when the eventual output is copyable. Compiler-generated async
functions and closures also implement `Awaitable<T>` without exposing their
generated frame type.
Standard-library adapter types such as `OperationFutureAdapter<T, Out>`,
`ChannelSendFuture<T>`, `ChannelReserveFuture<T>`, and
`ChannelRecvFuture<T>` wrap `Future<T>` and implement `Awaitable`, `Abortable`,
and, where the protocol permits, `CancelSafe`. The `awaitable_future` interface
determines `Out` from the awaitable receiver, so generic helpers can write
`A: Awaitable<Out = _>` when they need to name the output without exposing it as
an explicit type parameter. `block_on` is the synchronous bridge for `main`,
tests, and embedding hosts; it starts a future on the task runtime and blocks
the current thread until the future returns. Async bodies should use `await`
instead of nested `block_on`.

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
`channel_closed_error()`. Channel payloads must satisfy the standard-library
`Message` interface on `send`, `try_send`, `reserve`, `permit_send`, and `recv`.
The async `send`, `reserve`, and `recv` functions are ordinary standard-library
functions returning awaitable adapter newtypes; they are not compiler-generated
future variants. Structural payloads become usable channel messages through
ordinary `derive Message`, explicit `clone_message` implementations, or direct
`meta::Repr<T>` payload types.

Task groups support dynamic concurrency. `group_next` returns completed task
results in completion order without cancelling unfinished tasks. `group_cancel_all`
aborts unfinished tasks through `Abortable`, and `group_close` releases the
remaining group handle state. `with_task_group` creates a group for an async
body and closes it on return or cancellation, cancelling unfinished tasks before
closing the group. On normal return paths it reports group creation and async
runtime failures as `TaskGroupAsync`, body failures as `TaskGroupBody`, cleanup
failures as `TaskGroupCleanup`, and the combination of body plus cleanup
failure as `TaskGroupBodyCleanup`.

`timeout` races a selectable future with a timer. Timing out cancels only the
waiter future; it does not assume that an arbitrary underlying protocol can
discard partial state. The operand therefore must satisfy
`SelectableFuture<Out = _>`, which expands to the selectable view of
`Awaitable`, `CancelSafe`, and `Abortable` with `Out` determined from the
operand.

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

export interface<Op -> Out> Result<Out, Error> finish(Op op);
export unsafe interface<Op> ?*void raw_operation(*const Op op);
export unsafe interface<Op -> Out> c::c_int poll_done(*Op op, *Out out);
export interface OperationFuture<Out> = raw_operation + poll_done<Out>;
```

The internal adapter namespace describes runtime operation tokens. `notify_done`
and `finish` support low-level actor completion tests and direct operation
integration.
`raw_operation` returns null for a stale resource-backed operation token.
`OperationFuture<Out = _>` is used by `future_from_op` to wrap a one-shot
runtime operation as a future while deriving `Out` from the operation token.
Normal application code should call
awaitable stdlib functions such as `async_io::read_bytes`, `async_net::read`,
or `async_time::sleep_ms` instead of implementing operation adapters directly.

```ciel
// /std/async/internal/runtime_future
import /std/c as c;

export unsafe extern "C" {
    opaque struct CielFuture;
}

export type RuntimeRun = extern "C" c::c_int fn(*CielFuture, *void, *void);
export type RuntimeCleanup = extern "C" void fn(*CielFuture, *void, c::c_int);

export unsafe Future<Out> new<Out, Ctx>(
    Ctx @ctx,
    RuntimeRun run,
    RuntimeCleanup cleanup
);

export unsafe *T alloc_slot<T>();
```

`runtime_future::new` allocates runtime-owned context storage, moves `ctx` into
it, and calls the C runtime future constructor with result size, result
alignment, the run callback, the context pointer, and the cleanup callback. The
run callback receives the `CielFuture*` being polled, the context pointer, and
the output storage pointer. Returning `0` means the output has been completed;
returning `ciel_async_again_errno()` means the future is pending. Cleanup
receives the same future and context plus the runtime cleanup reason. The
constructor is unsafe because the callbacks must cast the context and output
pointers back to the exact types used at construction.

```ciel
// /std/async_io
export import /std/result;
import /std/actor as actor;
import /std/bytes as bytes;
import /std/io;
import /std/message;
import /std/os/fd as os_fd;
import /std/resource as resource;

export resource unsafe struct AsyncFd {
    resource::Handle handle;
}

export resource unsafe struct AsyncRead {
    resource::Handle handle;
}

export resource unsafe struct AsyncWrite {
    resource::Handle handle;
}

export Result<AsyncFd, AsyncIoError> open_async([]const char path, io::OpenMode mode) = .open_async;
export Result<AsyncFd, AsyncIoError> open_async_read([]const char path) = .open_async_read;
export Result<AsyncFd, AsyncIoError> create_async([]const char path) = .create_async;
export Result<AsyncFd, AsyncIoError> append_async([]const char path) = .append_async;
export unsafe Result<AsyncFd, AsyncIoError> async_from_raw_fd(os_fd::RawFd fd);
export Result<void, AsyncIoError> close_async(AsyncFd @fd) = .close;

export Result<AsyncRead, AsyncIoError> read_bytes_async(*const AsyncFd fd, usize max_len) = .read_async;
export Result<AsyncWrite, AsyncIoError> write_bytes_async(*const AsyncFd fd, bytes::Bytes data) = .write_async;
export async Result<bytes::Bytes, AsyncIoError> read_bytes(*const AsyncFd fd, usize max_len) = .read;
export async Result<usize, AsyncIoError> write_bytes(*const AsyncFd fd, bytes::Bytes data) = .write;

export Result<void, AsyncIoError> notify_read_done<M: Message>(
    *const AsyncRead op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncIoError> notify_write_done<M: Message>(
    *const AsyncWrite op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<bytes::Bytes, AsyncIoError> finish_read(AsyncRead op) = .finish;
export Result<usize, AsyncIoError> finish_write(AsyncWrite op) = .finish;
export Result<void, AsyncIoError> cancel_read(AsyncRead op) = .cancel;
export Result<void, AsyncIoError> cancel_write(AsyncWrite op) = .cancel;
```

`/std/async_io` provides awaitable file-descriptor operations over the resource
model. `AsyncFd`, `AsyncRead`, and `AsyncWrite` are resource-affine wrappers
over descriptor and operation entries. The high-level `read_bytes` and
`write_bytes` functions are async functions and are the normal API. The
`*_async`, `notify_*`, `finish_*`, and `cancel_*` operation-token functions are
low-level hooks for direct actor-completion integration; `finish_*` and
`cancel_*` consume or close the operation token entry. Raw fd reads and writes
are `Abortable` but not `CancelSafe` by default because cancellation may hide
offset changes or partial writes.

```ciel
// /std/async_net
export import /std/result;
import /std/actor as actor;
import /std/buf as buf;
import /std/bytes as bytes;
import /std/message;
import /std/net;
import /std/resource as resource;

export resource unsafe struct AsyncTcpListener {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpStream {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpReadHalf {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpWriteHalf {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpSplit {
    AsyncTcpReadHalf read;
    AsyncTcpWriteHalf write;
}

export resource unsafe struct BufferedStreamReader {
    *void handle;
    resource::Handle fd_handle;
}

export resource unsafe struct AsyncTcpBufferedSplit {
    BufferedStreamReader reader;
    AsyncTcpWriteHalf write;
}

export resource unsafe struct AsyncAccept {
    resource::Handle handle;
}

export resource unsafe struct AsyncConnect {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpRead {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpReadInto {
    resource::Handle handle;
}

export resource unsafe struct AsyncTcpWrite {
    resource::Handle handle;
}

export resource unsafe struct AsyncBufferedRead {
    resource::Handle handle;
}

export struct ReadIntoResult {
    buf::ByteBuf buffer;
    usize read;
}

export Result<AsyncTcpListener, AsyncNetError> listen_async(net::SocketAddr addr) = .listen_async;
export Result<net::SocketAddr, AsyncNetError> listener_addr(*const AsyncTcpListener listener) = .addr;
export Result<void, AsyncNetError> close_listener(AsyncTcpListener @listener) = .close;
export Result<AsyncAccept, AsyncNetError> accept_async(*const AsyncTcpListener listener) = .accept_async;
export Result<AsyncConnect, AsyncNetError> connect_async(net::SocketAddr addr) = .connect_async;
export async Result<AsyncTcpStream, AsyncNetError> accept(*const AsyncTcpListener listener) = .accept;
export async Result<AsyncTcpStream, AsyncNetError> connect(net::SocketAddr addr);
export async Result<AsyncTcpStream, AsyncNetError> connect_timeout(net::SocketAddr addr, u64 ms) = .connect_timeout;

export Result<void, AsyncNetError> close_stream(AsyncTcpStream @stream) = .close;
export Result<AsyncTcpSplit, AsyncNetError> split(AsyncTcpStream @stream) = .split;
export Result<AsyncTcpBufferedSplit, AsyncNetError> buffered_split(
    AsyncTcpStream @stream,
    usize capacity
) = .buffered_split;
export Result<void, AsyncNetError> shutdown_read(*const AsyncTcpStream stream) = .shutdown_read;
export Result<void, AsyncNetError> shutdown_read_half(*const AsyncTcpReadHalf half) = .shutdown_read;
export Result<void, AsyncNetError> shutdown_write(*const AsyncTcpStream stream) = .shutdown_write;
export Result<void, AsyncNetError> shutdown_write_half(*const AsyncTcpWriteHalf half) = .shutdown_write;
export Result<net::SocketAddr, AsyncNetError> stream_local_addr(*const AsyncTcpStream stream) = .local_addr;
export Result<net::SocketAddr, AsyncNetError> stream_peer_addr(*const AsyncTcpStream stream) = .peer_addr;

export Result<AsyncTcpRead, AsyncNetError> read_bytes(*const AsyncTcpStream stream, usize max_len) = .read_async;
export Result<AsyncTcpReadInto, AsyncNetError> read_into_async(*const AsyncTcpStream stream, buf::ByteBuf @buffer) = .read_into_async;
export Result<AsyncTcpWrite, AsyncNetError> write_bytes(*const AsyncTcpStream stream, bytes::Bytes data) = .write_async;
export Result<AsyncTcpWrite, AsyncNetError> write_half_bytes(*const AsyncTcpWriteHalf half, bytes::Bytes data) = .write_async;
export async Result<bytes::Bytes, AsyncNetError> read(*const AsyncTcpStream stream, usize max_len) = .read;
export async Result<ReadIntoResult, AsyncNetError> read_into(*const AsyncTcpStream stream, buf::ByteBuf @buffer) = .read_into;
export async Result<usize, AsyncNetError> write(*const AsyncTcpStream stream, bytes::Bytes data) = .write;
export async Result<usize, AsyncNetError> write_half(*const AsyncTcpWriteHalf half, bytes::Bytes data) = .write;
export async Result<AsyncTcpStream, AsyncNetError> write_all(AsyncTcpStream @stream, bytes::Bytes data) = .write_all;
export async Result<AsyncTcpWriteHalf, AsyncNetError> write_all_half(AsyncTcpWriteHalf @half, bytes::Bytes data) = .write_all;

export Result<BufferedStreamReader, AsyncNetError> buffered_reader(
    AsyncTcpReadHalf @half,
    usize capacity
) = .buffered_reader;
export Result<BufferedStreamReader, AsyncNetError> buffered_reader_from_split(
    AsyncTcpSplit @split,
    usize capacity
) = .buffered_reader;
export Result<BufferedStreamReader, AsyncNetError> buffered_reader_from_stream(
    AsyncTcpStream @stream,
    usize capacity
) = .buffered_reader;
export Result<AsyncTcpReadHalf, AsyncNetError> into_read_half(BufferedStreamReader @reader) = .into_read_half;
export Result<BufferedStreamReader, AsyncNetError> take_buffered_split_reader(
    *AsyncTcpBufferedSplit split_value
);
export Result<AsyncTcpWriteHalf, AsyncNetError> take_buffered_split_write(
    *AsyncTcpBufferedSplit split_value
);
export Result<AsyncBufferedRead, AsyncNetError> read_buffered_async(
    *const BufferedStreamReader reader,
    usize max_len
) = .read_async;
export Result<AsyncBufferedRead, AsyncNetError> read_exact_buffered_async(
    *const BufferedStreamReader reader,
    usize len
) = .read_exact_async;
export async Result<bytes::Bytes, AsyncNetError> read_buffered(
    *const BufferedStreamReader reader,
    usize max_len
) = .read;
export async Result<bytes::Bytes, AsyncNetError> read_exact_buffered(
    *const BufferedStreamReader reader,
    usize len
) = .read_exact;

export Result<void, AsyncNetError> notify_accept_done<M: Message>(
    *const AsyncAccept op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncNetError> notify_connect_done<M: Message>(
    *const AsyncConnect op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncNetError> notify_read_done<M: Message>(
    *const AsyncTcpRead op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncNetError> notify_read_into_done<M: Message>(
    *const AsyncTcpReadInto op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncNetError> notify_write_done<M: Message>(
    *const AsyncTcpWrite op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;

export Result<AsyncTcpStream, AsyncNetError> finish_accept(AsyncAccept op) = .finish;
export Result<AsyncTcpStream, AsyncNetError> finish_connect(AsyncConnect op) = .finish;
export Result<bytes::Bytes, AsyncNetError> finish_read(AsyncTcpRead op) = .finish;
export Result<ReadIntoResult, AsyncNetError> finish_read_into(AsyncTcpReadInto op) = .finish;
export Result<usize, AsyncNetError> finish_write(AsyncTcpWrite op) = .finish;
export Result<void, AsyncNetError> cancel_accept(AsyncAccept op) = .cancel;
export Result<void, AsyncNetError> cancel_connect(AsyncConnect op) = .cancel;
export Result<void, AsyncNetError> cancel_read(AsyncTcpRead op) = .cancel;
export Result<void, AsyncNetError> cancel_read_into(AsyncTcpReadInto op) = .cancel;
export Result<void, AsyncNetError> cancel_write(AsyncTcpWrite op) = .cancel;
export Result<void, AsyncNetError> cancel_buffered_read(AsyncBufferedRead op) = .cancel;
```

`/std/async_net` provides awaitable TCP operations over nonblocking runtime
resources. `AsyncTcpListener`, `AsyncTcpStream`, split halves, buffered
readers, and low-level async operation tokens are resource-affine wrappers over
registry entries. `accept` and `connect` are `CancelSafe + Abortable`, so they
can be used directly with `timeout` and `select`. `read` returns zero-length
`bytes::Bytes` for EOF. `read_into` moves an owned `buf::ByteBuf` into the
future and returns the same buffer with the number of bytes read so hot loops
can reuse capacity without treating immutable `Bytes` as a mutable destination.

`/std/async_net` no longer exposes a Message-safe stream transfer wrapper.
`AsyncTcpStream` and its split/read/write operation tokens are resource-affine
values. They move through ordinary lexical returns, `resource::scoped`, and
`resource::scoped_async`; clone-based task, actor, and channel APIs reject them.
The lower-level `resource::TransferToken` remains an affine registry fallback
for standard-library internals and explicit resource tests, not a stream
message API.

Raw TCP `read`, `read_into`, `write`, and `write_all` are `Abortable` but not
`CancelSafe`; they are rejected by `SelectableFuture` bounds. Task abort may
close or poison the stream to release a stuck operation, but a losing
`select`/`timeout` cannot keep using the same stream after possibly discarding
bytes, losing an owned buffer, or observing partial writes.

The `*_async`, `notify_*`, `finish_*`, and `cancel_*` functions are low-level
operation-token hooks for actor completion tests and direct operation
integration. `finish_*` and `cancel_*` consume or close the operation token.
Normal async application code should prefer `accept`, `connect`, `read`,
`write`, and the buffered reader helpers.
Operation token types are split by completion value: for example,
`AsyncTcpRead` completes to `bytes::Bytes`, while `AsyncTcpReadInto` completes
to `ReadIntoResult`. This preserves the determined `Op -> Out` contract on
`adapter::finish` and `adapter::poll_done`, allowing `future_from_op` to infer
its output through `OperationFuture<Out = _>` instead of accepting an explicit
output type argument.

Selectable stream reads use `BufferedStreamReader`. The reader owns the read
half token and a private buffer. It does not register a second owner entry for
the same fd; converting it back with `into_read_half` returns the retained
read-half token. `read_buffered` is `CancelSafe + Abortable` because
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
import /std/resource as resource;

export resource unsafe struct AsyncSleep {
    resource::Handle handle;
}

export Result<AsyncSleep, AsyncTimeError> sleep_ms_async(u64 ms);
export async Result<void, AsyncError> sleep_ms(u64 ms);
export Result<void, AsyncTimeError> notify_sleep_done<M: Message>(
    *const AsyncSleep op,
    *const actor::Actor<M> actor_handle,
    M message
) = .notify_done;
export Result<void, AsyncTimeError> finish_sleep(AsyncSleep op) = .finish;
export Result<void, AsyncTimeError> cancel_sleep(AsyncSleep op) = .cancel;
```

`/std/async_time` provides monotonic awaitable timers. Low-level `AsyncSleep`
values are resource-affine async operation tokens. `sleep_ms` is the normal
async timer API and is `CancelSafe + Abortable`. `sleep_ms_async`,
`notify_sleep_done`, `finish_sleep`, and `cancel_sleep` are low-level
operation-token hooks for direct actor-completion integration; `finish_sleep`
and `cancel_sleep` consume or close the operation token. Timer policy is
deliberately narrow: protocol-specific heartbeat, missed-pong, retry, and
deadline behavior belongs in application code or in helpers such as
`async::timeout`.

These modules are standard library API. They are not compiler intrinsics except
where this specification names `/std/meta` type metadata helpers or a runtime
hook.

## 20. C Interop and ABI

`extern "C"` declarations are C ABI declarations. C APIs require explicit
pointer nullability and view mutability: users write `*T`, `*const T`, `?*T`,
and `?*const T`. Standalone `const T` is not a Ciel source type. Ciel specifies
`extern "C"` as its C ABI spelling. C ABI callable types are named C ABI
functions and `extern "C" ... fn(...)` function-pointer types. Closure values
use the Ciel ABI.

Imported C functions are unsafe functions. They must be declared with an unsafe
C boundary, either as `unsafe extern "C" T name(...);` or inside an
`unsafe extern "C"` block. A top-level `export extern "C" T name(...) { ... }`
defines a user-visible C ABI symbol implemented in Ciel; it is not unsafe to
call from Ciel unless the declaration itself is marked `unsafe`. A non-exported
top-level `extern "C" T name(...) { ... }` defines an internal C ABI function
implemented in Ciel. It can be used as a C callback value from Ciel, but it does
not expose the source name as a public C symbol. `export unsafe extern "C" {
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
i32 fn(i64)                     // Ciel ABI
extern "C" i32 fn(*void, *void) // C ABI
```

The Ciel internal ABI may lower large returns and arguments using hidden
pointers. Any declaration marked `extern "C"` or `export extern "C"` must obey
the target platform C ABI as written. By-value `void` parameters are invalid in
`extern "C"` declarations; an empty C parameter list is written by omitting
parameters. Opaque constrained returns are also invalid for C ABI declarations
because the C signature must expose a concrete source return type.

Imported C declarations cannot be generic. Exported C ABI functions cannot be
generic because their source name is the stable C symbol. Non-exported
`extern "C"` function bodies may be generic templates when their C-visible
parameter and return types do not mention the template's type parameters. Type
parameters may appear inside the body. A type-applied function item such as
`compare<Item>` checks the generic arity and constraints, substitutes the
function type, records the concrete instance for monomorphization, and lowers
to the generated internal C symbol for that instance.

```ciel
type Compare = extern "C" c::c_int fn(*const void, *const void, *void);

extern "C" c::c_int compare_items<T>(
    *const void left_raw,
    *const void right_raw,
    *void ctx_raw,
) {
    *const T left = unsafe { left_raw as *const T };
    *const T right = unsafe { right_raw as *const T };
    ...
}

Compare compare = compare_items<Item>;
```

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
registration. It is idempotent. The runtime configures BDWGC for allocation-heavy
Ciel workloads by default, using a 128 MiB initial heap target and a free-space
divisor of `1` unless overridden by `CIEL_GC_INITIAL_HEAP_SIZE`,
`CIEL_GC_FREE_SPACE_DIVISOR`, or the corresponding BDWGC `GC_*` environment
variables. Generated executables call it before user `main`. Shared libraries
also emit an internal constructor or target-equivalent initializer:

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

## 21. Debug Information

A debug build emits target debug information through the generated C compiler.
The Ciel compiler:

1. preserves generated C files when requested
2. passes the target compiler's debug flag such as `-g`
3. emits `#line` directives mapping generated C back to Ciel source files
4. uses deterministic mangled names for generated C symbols
5. keeps a source-location table for runtime diagnostics such as panic messages

The minimum debug contract is source-line mapping, readable panic locations,
and deterministic generated names.

## 22. C Backend Lowering

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
