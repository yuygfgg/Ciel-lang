# Derivable Impl Proposal

This proposal adds explicit derive declarations for library-defined capability
templates. The goal is to remove repetitive wrapper impls for structural wire
policies and structural `Message` policies while preserving the current
explicit opt-in model.

Derivation is not a macro system and does not add blanket impls. A derivable
impl is an inert template until a source file asks for one concrete derived
capability:

```ciel
derive message::Message<User>;
derive json::Wire<User>;
```

## Proposal Order

```text
metaprogramming < derivable-impls
pure-library-message < derivable-impls[Message nominal convenience]
wire-postponed <= derivable-impls[derived thin wrappers]
```

Structural metaprogramming supplies `meta::RefRepr<T>`, `meta::Repr<T>`,
`meta::Schema<T>`, and the projection functions used by the standard derive
templates. Pure-library `Message` remains the semantic baseline: the language
does not make every structural type a `Message` automatically. This proposal
only adds explicit user-requested nominal impl generation.

## Problem

The current structural APIs work, but they require too much repeated code in
two common cases.

For wire formats, every visible struct or enum that wants the default JSON
structural policy must write thin wrappers:

```ciel
impl wire::encode_value(*const Packet value, *json::Writer writer) {
    _ schema = meta::schema<Packet>();
    _ repr = meta::as_ref_repr(value);
    return json::write_struct(&schema, &repr, writer);
}

impl wire::decode_value(meta::Type<Packet> target, *json::Reader reader) {
    json::Value value = json::read_value(reader)?;
    _ schema = meta::schema<Packet>();
    return json::decode_struct_value(
        target,
        &schema,
        meta::type_tag<meta::Repr<Packet>>(),
        &value
    );
}
```

For `Message`, the pure-library model asks users either to expose
`meta::Repr<T>` at actor and channel boundaries, or to hand-write the nominal
adapter:

```ciel
unsafe impl clone_message(*const Event value) {
    meta::Repr<Event> repr = meta::into_repr(value);
    meta::Repr<Event> copied = clone_message(&repr)?;
    return Ok(meta::from_repr<Event>(copied));
}
```

The boilerplate is mechanical and obscures the policy choice. The intended
source-level action is simply:

```ciel
derive message::Message<Event>;
derive json::Wire<Event>;
```

There is a third smaller case: many marker interfaces have impl bodies whose
only behavior is `return true;`. Examples include share-handle, thread-local,
atomic-value, and cancel-safe markers. These are still real capability choices,
but the implementation body adds no information beyond the selected marker and
receiver type.

## Goals

1. Let libraries publish derive templates for ordinary interfaces and interface
   aliases.
2. Let user code explicitly derive a concrete capability with
   `derive path<T>;`.
3. Keep all generated impls subject to ordinary coherence.
4. Preserve visibility and privacy rules for structural reflection.
5. Support safe derivation of impls for unsafe interfaces when the template
   discharges the unsafe contract through checked obligations.
6. Keep manual impls valid as an alternative to derive, while rejecting a
   program that derives and manually implements the same interface term for the
   same receiver type.
7. Give diagnostics at the original structural field, payload, or capture path
   when a derived template fails.
8. Let marker-only interfaces replace repetitive `return true` impl bodies with
   explicit derive declarations.
9. Keep first-version template selection deterministic by allowing at most one
   derivable template for a fully applied interface term.

## Non-Goals

1. Adding a general source macro system.
2. Adding blanket structural impls for all structs and enums.
3. Making `meta::Repr<T>: Message` a general source-level `where` clause in the
   first implementation.
4. Inferring constraints for every generic derive declaration in the first
   implementation.
5. Adding field or variant policy metadata such as rename, defaults, skip, or
   enum representation selection.
6. Removing the ability to hand-write `wire::encode_value`,
   `wire::decode_value`, or `clone_message` impls.
7. Making compiler-owned marker facts, such as closure or Ciel function value
   markers, user-derivable.

## Syntax

Add contextual top-level declarations:

```ebnf
TopLevel        ::= ... | DeriveDecl | DerivableImplDecl

DeriveDecl      ::= [ "unsafe" ] "derive" [ GenericParamList ]
                    DeriveTarget TypeArgList ";"

DeriveTarget    ::= Path

DerivableImplDecl ::= [ "unsafe" ] "derivable" [ "unsafe" ] "impl"
                      [ GenericParamList ] Path [ TypeArgList ]
                      "(" ParameterList ")" Block
```

`derive` and `derivable` are contextual item-start tokens. They do not become
reserved identifiers in expression, type, or module-path positions.

The optional `unsafe` before `derivable` marks a template whose instantiation
requires `unsafe derive`. The optional `unsafe` before `impl` has the same
meaning as ordinary `unsafe impl`: the generated impl implements an unsafe
interface.

In the first implementation, a public derive target takes exactly one type
argument: the receiver type being opted into the capability. Interfaces with
additional non-receiver generic parameters should be exposed through
one-argument aliases that fix those policy parameters.

Examples:

```ciel
derive message::Message<User>;
derive json::Wire<User>;
```

Generic derive declarations may introduce generic parameters with the existing
generic parameter syntax:

```ciel
struct Envelope<T> {
    T payload;
}

derive<T: message::Message> message::Message<Envelope<T>>;
derive<T: json::Wire> json::Wire<Envelope<T>>;
```

The first implementation should require generic derive declarations to provide
the generic constraints needed by the generated impl body. Constraint inference
for derives can be added later.

## Derivable Impl Templates

A derivable impl has the same signature shape as an ordinary impl, but it is not
entered into the global impl table.

```ciel
derivable unsafe impl<T> clone_message(*const T value) {
    meta::Repr<T> repr = meta::into_repr(value);
    meta::Repr<T> copied = clone_message(&repr)?;
    return Ok(meta::from_repr<T>(copied));
}
```

This declaration says that the module can generate a concrete
`clone_message(*const X)` impl when a derive declaration requests a capability
that needs it. It does not make any `X` implement `Message` by itself.

Derivable impl bodies are checked after instantiation. At declaration time, the
compiler parses them, resolves names, validates the target interface, and stores
the body as a template. At derive time, the compiler substitutes the requested
receiver type and any generic derive parameters, normalizes structural meta
types, then type-checks the generated concrete or generic impl under the derive
declaration's constraints.

This delayed check is the narrow replacement for a general computed-type
constraint such as:

```ciel
where meta::Repr<T>: Message
```

The language does not need to expose that `where` form to users in the first
implementation.

## Deriving Interface Aliases

A derive target may name either an interface or an interface alias.

When the target is an interface, it must be a one-receiver interface with no
unfixed non-receiver type parameters.

When the target is an interface alias, the compiler expands the alias:

1. positive interface terms must be generated from derivable templates;
2. negative terms are checked after generated impls are added;
3. removed terms in a narrowed alias are not generated;
4. alias expansion is recursive and follows the same capability algebra as
   ordinary constraints.

For example, `/std/message` already defines:

```ciel
interface MessageInternal = clone_message;
interface ThreadLocalInternal = thread_local_marker;
interface Message = MessageInternal + !ThreadLocalInternal;
```

Therefore:

```ciel
derive message::Message<User>;
```

generates the positive `clone_message<User>` witness and then checks that
`User` does not satisfy `ThreadLocalInternal`.

For JSON, `/std/json` should expose a recommended alias:

```ciel
export interface Wire =
    wire::encode_value<Writer> + wire::decode_value<Reader>;
```

User code can then write:

```ciel
derive json::Wire<User>;
```

rather than deriving the two low-level wire interfaces separately.

`/std/json` may also expose one-argument aliases for partial policies:

```ciel
export interface Encode = wire::encode_value<Writer>;
export interface Decode = wire::decode_value<Reader>;
```

Then low-level explicit derives remain one-argument declarations:

```ciel
derive json::Encode<User>;
derive json::Decode<User>;
```

The first implementation should not allow direct multi-argument derive targets
such as `derive wire::encode_value<User, json::Writer>;`.

## Single Template Rule

For the first implementation, a fully applied interface term may have at most
one derivable impl template in the imported program. The receiver type parameter
is not counted for this rule, because it is supplied by each derive declaration.

Examples of distinct fully applied terms:

```ciel
wire::encode_value<json::Writer>
wire::encode_value<cbor::Writer>
```

These may each have one derivable template because their non-receiver policy
arguments differ.

These may not coexist:

```ciel
derivable impl<T> wire::encode_value(*const T value, *json::Writer writer) {
    ...
}

derivable impl<T> wire::encode_value(*const T value, *json::Writer writer) {
    ...
}
```

This is stricter than ordinary impl coherence. Derivable templates do not
overload by constraints and do not participate in specialization. If two
imported modules provide templates for the same fully applied term, the program
is rejected before considering any particular derive declaration.

## Single-Argument Derive Targets

The first implementation should require derive targets to be one-argument
capabilities, where that argument is the receiver type. This is a deliberate
restriction, not a limitation of the underlying interface system.

Benefits:

1. `derive x::Capability<T>;` always means "make the nominal type `T` satisfy
   this capability".
2. Template selection has no partial type-argument inference problem.
3. Documentation and diagnostics can point at one source type.
4. Multi-parameter policy choices stay named by libraries instead of appearing
   as ad hoc derive argument lists.

For multi-parameter interfaces, libraries should expose a one-argument alias
that fixes the policy parameters:

```ciel
export interface Wire =
    wire::encode_value<Writer> + wire::decode_value<Reader>;

export interface Encode = wire::encode_value<Writer>;
export interface Decode = wire::decode_value<Reader>;
```

This keeps the public derive spellings simple:

```ciel
derive json::Wire<User>;
derive json::Encode<User>;
derive json::Decode<User>;
```

The main cost is that a library must name each useful policy combination. That
is acceptable for the first implementation because those names are also better
API documentation. A future extension may allow direct multi-argument derive
targets, but it should not be required for structural `Message`, JSON wire, or
marker-only migration.

## Marker-Only Derivation

Marker interfaces often have a boolean return type and implementations that
only return `true`:

```ciel
unsafe impl share_handle_marker(*const Text value) {
    return true;
}
```

Libraries should be able to publish marker-only derivable templates:

```ciel
unsafe derivable unsafe impl<T> share_handle_marker(*const T value) {
    return true;
}
```

The leading `unsafe derivable` is important for most marker policies. A marker
such as `share_handle_marker`, `thread_local_marker`, `atomic_value_marker`, or
`cancel_safe_marker` usually asserts a semantic fact that the compiler cannot
prove from the type shape alone. Deriving it is therefore an unsafe policy
assertion:

```ciel
unsafe derive message::share_handle_marker<Text>;
```

Safe marker derives are still possible for safe marker interfaces or for unsafe
interfaces whose template fully discharges the safety contract through checked
obligations, but marker-only templates should not default to safe instantiation.

Interface aliases can bundle marker derivation with other capabilities. For
example, a library-owned synchronized handle may derive both its clone policy
and its share-handle marker, then satisfy the public `ShareHandle` alias through
ordinary capability solving.

Compiler-owned marker facts remain outside this feature. Interfaces such as
`meta::ciel_fn_value_marker` and `meta::closure_value_marker` are still reserved
for compiler-provided facts and must reject user derivation just as they reject
user impls.

## Unsafe Model

Unsafe target interfaces and unsafe derive instantiation are separate concepts.

`derivable unsafe impl` means the generated impl is an `unsafe impl` because the
target interface is unsafe:

```ciel
derivable unsafe impl<T> clone_message(*const T value) {
    ...
}
```

This does not automatically require an unsafe derive declaration. The template
may be safe to instantiate when all of its obligations are checked by the
compiler and by ordinary capability solving.

`/std/message` structural derivation is safe to instantiate:

```ciel
derive message::Message<User>;
```

The generated impl is unsafe internally, but its safety contract is discharged
by:

1. concrete normalization of `meta::Repr<User>`;
2. ordinary `Message` checks for every representation leaf;
3. negative capability checks from the `Message` alias;
4. existing unsafe leaf impls taking responsibility for their own types.

Some derivable templates may need an unchecked promise from the deriving module.
Those templates are declared with an unsafe derive requirement:

```ciel
unsafe derivable unsafe impl<T> trusted_marker(*const T value) {
    ...
}
```

They must be used with:

```ciel
unsafe derive pkg::TrustedMarker<User>;
```

The first implementation should reject safe `derive` for templates marked with
an unsafe derive requirement.

## Message Derivation

`/std/message` should provide a structural nominal derive template:

```ciel
import /std/meta as meta;

derivable unsafe impl<T> clone_message(*const T value) {
    meta::Repr<T> repr = meta::into_repr(value);
    meta::Repr<T> copied = clone_message(&repr)?;
    return Ok(meta::from_repr<T>(copied));
}
```

User code:

```ciel
import /std/message as message;
import /std/text as text;

struct User {
    i64 id;
    text::Text name;
}

derive message::Message<User>;
```

After derivation, ordinary APIs can use `User: Message`:

```ciel
channel::Channel<User> users = channel::make_channel<User>()?;
channel::channel_send<User>(&users, user)?;
```

If derivation fails, the error is a compile-time capability error:

```ciel
struct Bad {
    *i64 ptr;
}

derive message::Message<Bad>;
```

Example diagnostic:

```text
cannot derive message::Message<Bad>
field `ptr` has type `*i64`, which does not implement message::Message
```

The compiler should preserve normalized SOP paths internally, but diagnostics
should prefer the original field, payload, or capture path.

## Wire Derivation

`/std/json` should provide default structural templates for the common typed
wire policy. The public recommended target is the `json::Wire` alias:

```ciel
export interface Wire =
    wire::encode_value<Writer> + wire::decode_value<Reader>;
```

The encode template:

```ciel
derivable impl<T> wire::encode_value(*const T value, *Writer writer) {
    _ schema = meta::schema<T>();
    _ repr = meta::as_ref_repr(value);
    return write_structural(&schema, &repr, writer);
}
```

The decode template:

```ciel
derivable impl<T> wire::decode_value(meta::Type<T> target, *Reader reader) {
    Value value = read_value(reader)?;
    _ schema = meta::schema<T>();
    return decode_structural_value(
        target,
        &schema,
        meta::type_tag<meta::Repr<T>>(),
        &value
    );
}
```

`write_structural` and `decode_structural_value` are placeholders for the JSON
helper surface that handles both structs and enums through schema shape. The
implementation may keep separate struct and enum helpers internally, but the
derive template should not require users to choose a different derive spelling
for structs and enums.

User code:

```ciel
import /std/json as json;
import /std/text as text;

struct Packet {
    i64 sequence;
    text::Text label;
}

derive json::Wire<Packet>;
```

Partial policy derives should use the one-argument aliases:

```ciel
derive json::Encode<Packet>;
derive json::Decode<Packet>;
```

The recommended documentation style should use `derive json::Wire<T>;`.

This proposal does not add field metadata. Derived JSON uses the current
default structural policy: source field names, externally tagged enums, current
unknown-field behavior, and existing leaf/container policies.

## Visibility

A derive declaration must be checked in a context where the generated template
body is legal.

For structural templates, this means the deriving module must be allowed to use
the relevant `meta` operations for the receiver type. A module that cannot see
the shape of `User` cannot derive `json::Wire<User>` or
`message::Message<User>` through the structural templates.

This preserves the existing rule for hand-written wrappers: the module that
owns or can see the data shape chooses whether to expose a structural policy.

Impls generated by derive declarations participate in the whole imported
program exactly like ordinary impls. Derive declarations themselves are not
exported names.

## Coherence And Template Selection

Each generated impl is checked by the existing impl coherence rules.

Errors:

```ciel
impl clone_message(*const User value) {
    ...
}

derive message::Message<User>; // duplicate impl error
```

If a requested positive interface term has no applicable derivable impl
template, derive fails. An existing explicit impl for the same receiver and
interface term does not satisfy the derive request; it conflicts with it.

If more than one derivable impl template exists for the same fully applied
interface term, the program is rejected by the single template rule.

Derivable impl templates may be generic and may overlap because they are inert.
Only the generated impls enter ordinary impl coherence, but template collection
still enforces the single template rule.

## Lowering Model

For each derive declaration:

1. resolve the derive target path;
2. collect the requested receiver type and reject derive targets with additional
   unfixed type parameters;
3. expand interface aliases into positive, negative, and removed terms;
4. reject the derive if an explicit impl already covers any generated positive
   term for the requested receiver type;
5. for each positive term, locate the unique matching derivable impl template;
6. instantiate each selected template with the derive type arguments;
7. normalize `meta::RefRepr`, `meta::Repr`, and `meta::Schema` in the generated
   impl body;
8. type-check the generated impl under the derive declaration's generic
   parameters and constraints;
9. add the generated impls to the ordinary impl table;
10. run ordinary positive and negative capability checks for the requested target;
11. lower generated impls through the existing monomorphization and codegen
    pipeline.

The generated impl should keep source spans that point back to both the derive
declaration and the derivable template. Diagnostics should report the user
derive site first, then the failed template operation or structural path.

## Migration Plan

Existing source remains valid. Manual impls and direct `meta::Repr<T>` message
types continue to work.

The recommended documentation and examples should migrate mechanically:

1. Add `derivable` templates to `/std/message`.
2. Add `json::Wire` and the JSON structural derive templates to `/std/json`.
3. Add marker-only derivable templates for ordinary library-owned marker
   interfaces whose manual impl bodies only return `true`.
4. Replace documentation examples that hand-write default JSON wrappers with
   `derive json::Wire<T>;`.
5. Replace documentation examples that recommend public
   `type EventMessage = meta::Repr<Event>` solely for ordinary structural actor
   or channel payloads with `derive message::Message<Event>;` and the nominal
   `Event` type.
6. Replace repetitive marker impls such as share-handle, thread-local,
   atomic-value, atomic-integer, and cancel-safe `return true` bodies with
   `derive` or `unsafe derive` declarations as appropriate for the marker's
   safety contract.
7. Keep low-level `meta::Repr<T>` examples in the structural metaprogramming
   section as advanced examples and as the underlying mechanism.
8. Keep examples that need custom clone, validation, resource transfer, custom
   JSON policy, or field defaults as hand-written impls.
9. Update `design.md` recommended style in the `Message`, actor/channel, JSON
   wire, and marker-interface sections.
10. Leave old tests and examples valid where they exercise explicit low-level
    representation behavior.

This is a broad but mostly mechanical migration. The language behavior remains
backward compatible because a derive declaration only adds impls at explicit
derive sites.

## Test Plan

Add focused tests for the language feature:

1. deriving `message::Message` for a simple struct succeeds;
2. deriving `message::Message` for an enum succeeds;
3. deriving `message::Message` for nested visible ADTs succeeds;
4. derived `Message` works with channels and actors using the nominal type;
5. raw pointer fields reject with a field-path diagnostic;
6. slice fields reject with a field-path diagnostic;
7. dynamic interface fields reject unless an explicit safe policy exists;
8. thread-local handles reject through the negative `Message` alias term;
9. duplicate explicit impl plus derive is rejected by coherence;
10. duplicate derive declarations for the same concrete target are rejected;
11. derive outside a module that can see the structural shape is rejected;
12. safe `derive` of a template marked as requiring unsafe derive is rejected;
13. `unsafe derive` of such a template is accepted when its body type-checks;
14. marker-only derivation generates the same behavior as an impl that returns
    `true`;
15. compiler-owned marker interfaces reject derive declarations;
16. a second derivable template for the same fully applied interface term is
    rejected by the single template rule.

Add JSON/wire tests:

1. `derive json::Wire<Struct>` encodes and decodes the default struct shape;
2. `derive json::Wire<Enum>` encodes and decodes the default externally tagged
   enum shape;
3. `derive json::Wire<Nested>` reports paths through nested fields;
4. low-level one-argument aliases such as `derive json::Encode<T>` and
   `derive json::Decode<T>` work independently;
5. existing manual wrapper tests continue to pass.

Add migration coverage:

1. update at least one actor/channel example to use nominal derived `Message`;
2. update at least one JSON example to use `derive json::Wire<T>;`;
3. update at least one standard-library marker-only impl to use `derive` or
   `unsafe derive`;
4. keep one explicit `meta::Repr<T>` message example to prove the low-level path
   remains supported.

## Open Questions

1. Whether generic derive declarations should infer their required constraints
   from the instantiated template body after the first implementation.
2. Whether a future `where` clause over computed type expressions should be
   added generally once derives prove the need.
3. Whether field and variant metadata should attach to the type declaration, the
   derive declaration, or a separate policy object.
4. Whether named derive strategies are needed when multiple libraries provide
   different templates for the same interface.
