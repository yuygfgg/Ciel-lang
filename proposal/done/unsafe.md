# Unsafe Boundary Proposal

## Historical Status

The later `error-downcast` proposal removes the trusted `Message` policy from
`Error` and assigns it to the immutable diagnostic `Report` type. References
below to standard message implementations should be read with that replacement.
`design.md` is normative.

This proposal adds an explicit unsafe boundary for operations whose safety
depends on contracts outside the Ciel type checker. It does not make unsafe code
unchecked Ciel. Ordinary type checking, definite assignment, nullability, view
mutability, C ABI validation, and generic constraints still apply inside unsafe
code.

Unsafe marks the proof obligation. Safe code may call safe wrappers, but it
cannot directly perform operations whose memory, lifetime, actor, or foreign ABI
safety is not proved by Ciel.

## Proposal Order

```text
unsafe < dispatch-actor-io-runtime[raw descriptors and C runtime hooks]
unsafe <= monomorphized-c-callbacks[C callback declarations]
pure-library-message < unsafe[manual policy impls become unsafe]
```

`dispatch-actor-io-runtime` depends on this proposal for raw descriptor
adoption, dispatch runtime hooks, and C APIs used by the runtime-backed standard
library.

`pure-library-message` is already implemented and remains the owner of
`Message` semantics. This proposal follows it. It does not change how
structural message policy is derived through `/std/meta`; it only marks manual
policy impls as trusted implementation sites.

## Audit Of Existing Design

Current `design.md` already contains trusted boundaries. They are safe only
because users, the standard library, generated code, or host C code honor
contracts that Ciel does not prove.

1. C imports. Imported `extern "C"` function declarations rely on the author
   spelling the target C ABI, pointer nullability, pointee mutability, return
   type, and ownership convention accurately. A call also relies on the C callee
   honoring that declaration at runtime.
2. `#c_include`. Header inclusion changes the generated C translation unit but
   does not create Ciel names. It is not itself an unsafe operation, but it is
   normally paired with unsafe imported declarations.
3. `noescape`. The escape analyzer treats an imported C pointer parameter as
   non-escaping only when the declaration says `noescape`. That is a promise
   about foreign code, not a fact Ciel can verify.
4. Raw pointer casts from `*void` and `?*void` back to typed pointers. Such
   casts can change the pointee type seen by later Ciel code. View mutability
   and nullability are still checked, but the pointee layout and lifetime
   contract is trusted. Casting a typed pointer to `*void` or `?*void` is only
   type erasure and is not unsafe by itself.
5. Opaque C handles. `opaque struct FILE`, host handles, and other C resources
   carry no structural ownership, lifetime, close, or thread-safety facts.
6. Current `/std/io::Fd`. `Fd { raw }`, `from_raw_fd`, `raw_fd`, `close`, and
   direct POSIX `read` / `write` expose a copyable integer resource. The OS may
   reuse a closed descriptor, and the type system cannot invalidate old copies.
7. Actor and channel message policy. Manual `clone_message` implementations can
   move values across actors or channels. A bad impl can share actor-local
   mutable state, lose required roots, or copy a handle with the wrong lifetime
   policy.
8. Shared-handle policy. Manual `share_handle_marker`,
   `thread_local_marker`, `atomic_value_marker`, and `atomic_integer_marker`
   implementations assert synchronization or runtime representation facts that
   are not derived from ordinary field types.
9. Runtime hooks. GC roots, thread attachment, actor handles, channel handles,
   mutex slots, atomics, panic hooks, dispatch queues, and async I/O operation
   objects are safe only through the wrapper contracts that own their state
   machines.
10. Host-created threads. The host ABI requires threads to attach before they
    call Ciel or hold Ciel GC pointers, and to detach only after those pointers
    are gone. Ciel cannot prove that discipline for foreign threads.

This proposal does not reject these features. It makes the trusted edges
visible.

C spelling type aliases and opaque type declarations are not executable unsafe
operations by themselves. They become part of an unsafe boundary when a function
call, raw handle adoption, pointer cast, or policy impl relies on them.

## Syntax

Add `unsafe` as a contextual marker in four positions:

```ciel
unsafe {
    c_call(...);
}

unsafe extern "C" {
    c::c_ssize_t read(c::c_int fd, *void buf, c::c_size_t count);
}

unsafe interface<T> Result<T, Error> clone_message(*const T value);

unsafe impl clone_message(*const SharedHandle value) {
    return Ok(*value);
}
```

`unsafe { ... }` is an expression block. It permits unsafe operations inside the
block and evaluates to the value of its final expression when used in expression
position.

`unsafe extern "C" { ... }` imports foreign C functions. The unsafe marker
belongs to imported callable declarations, not to the ABI. The block may also
contain opaque structs or C spelling types used by those function signatures;
those type declarations are not unsafe to name. Exported C ABI functions
written in Ciel are not automatically unsafe to call from Ciel, because their
bodies are checked Ciel code.

`unsafe interface` declares that implementing the interface is a safety
contract. `unsafe impl` is required for an implementation of an unsafe
interface.

`unsafe` may also mark a Ciel function declaration:

```ciel
unsafe Result<AsyncFd, Error> async_from_raw_fd(os::RawFd fd);
```

Calling an unsafe Ciel function requires an unsafe block. The function body may
use unsafe operations, but each unsafe operation still appears in an unsafe
block unless it is part of the function's trusted primitive implementation.

## Unsafe Operations

The first implementation should require an unsafe block for:

1. calling an imported `unsafe extern "C"` function;
2. calling an `unsafe` Ciel function;
3. casting from `*void` or `?*void` back to a typed pointer;
4. constructing a safe wrapper from a raw OS descriptor or opaque C handle;
5. taking ownership of a raw handle from another runtime owner;
6. calling host runtime hooks that manipulate GC roots, thread attachment, actor
   internals, channel internals, mutex internals, atomics, dispatch queues, or
   async I/O internals.

The first implementation should require `unsafe impl` for every implementation
of an unsafe interface. The standard library should make these interfaces
unsafe in the first slice:

1. `clone_message`;
2. `share_handle_marker`;
3. `thread_local_marker`;
4. `atomic_value_marker`;
5. `atomic_integer_marker`;
6. any future interface whose implementation is a safety contract.

Compiler-owned marker facts such as Ciel function values and concrete closure
values should remain compiler-owned. If a future design allows user-written
impls for those marker interfaces, those interfaces must be declared unsafe
first.

## Message As An Unsafe Interface

`clone_message` is the boundary that moves values across actors and channels.
The compiler cannot verify that an arbitrary implementation constructs an
independent receiver value or a correctly synchronized shared handle. Therefore
the interface and its implementations must be unsafe as a pair: `unsafe
interface` marks the contract, and `unsafe impl` marks the implementation site
where the author accepts that contract.

The standard library should declare:

```ciel
export unsafe interface<T> Result<T, Error> clone_message(*const T value);
export unsafe interface<T> bool share_handle_marker(*const T value);
export unsafe interface<T> bool thread_local_marker(*const T value);

export interface Message = clone_message;
export interface ShareHandle = share_handle_marker;
export interface ThreadLocal = thread_local_marker;
```

The interface aliases remain safe to use as constraints. The unsafe obligation
is on implementation, not on asking whether a type satisfies the capability.
An implementation of an unsafe interface must write `unsafe impl`; an
implementation of a safe interface must not write `unsafe impl`.

Standard-library impls for primitives, `Report`, `Result<T, E>`, owned
`/std/meta` SOP nodes, actor handles, channels, atomics, mutexes, and async
runtime handles are written as `unsafe impl` inside the standard library. User
impls of the same unsafe interfaces must also use `unsafe impl`.

Example:

```ciel
struct Event {
    i64 value;
}

unsafe impl clone_message(*const Event value) {
    return Ok(*value);
}
```

The body is still checked. The unsafe marker records the contract that `Event`
contains no actor-local borrowed state and that `*value` creates a valid
receiver-owned value.

Other marker capabilities follow the same rule:

```ciel
export unsafe interface<T> bool atomic_value_marker(*const T value);
export unsafe interface<T> bool atomic_integer_marker(*const T value);

export interface AtomicValue = atomic_value_marker;
export interface AtomicInteger = atomic_integer_marker;
```

The implementation proof is different for each interface. `clone_message`
proves cross-actor transfer. `share_handle_marker` proves shared identity is
synchronized or immutable. `thread_local_marker` proves the value must stay
actor-local. Atomic markers prove the runtime can perform the requested atomic
operation for that representation.

## C FFI

C function imports should be unsafe by default:

```ciel
unsafe extern "C" {
    c::c_ssize_t read(c::c_int fd, *void buf, c::c_size_t count);
    c::c_int close(c::c_int fd);
}
```

Calling these declarations requires an unsafe block:

```ciel
c::c_ssize_t n = unsafe {
    posix::read(fd, out.ptr as *void, out.len as c::c_size_t)
};
```

The standard library should expose safe wrappers that validate state and convert
errno into `Error`.

`noescape` remains allowed only on imported C functions, but such declarations
must appear in `unsafe extern "C"` blocks. The author promises that the C
callee will not retain the pointer beyond the call.

`#c_include` itself is not an executable unsafe operation. Header inclusion does
not create Ciel names and does not bypass unsafe call rules.

Type-only C ABI declarations may stay safe:

```ciel
extern "C" {
    type c_size_t = "size_t";
    opaque struct FILE;
}
```

Once a module imports callable C symbols that operate on those types, those
function declarations belong in an `unsafe extern "C"` block.

Re-exporting an imported C function keeps the unsafe call requirement:

```ciel
export unsafe extern "C" {
    c::c_int close(c::c_int fd);
}
```

## Raw Handles

Raw handles live in low-level modules, not in safe facades:

```ciel
// /std/os/fd
export struct RawFd {
    c::c_int raw;
}

export unsafe RawFd from_raw_fd(c::c_int raw);
export unsafe c::c_int raw_fd(RawFd fd);
```

Safe modules may adopt raw handles only through unsafe functions with explicit
ownership contracts:

```ciel
export unsafe Result<AsyncFd, Error> async_from_raw_fd(os::RawFd fd);
```

After a successful adoption, the caller must not use the old raw handle unless
the adopting function explicitly duplicates it.

The current `design.md` `/std/io::Fd` surface is therefore classified as a
legacy unsafe boundary. A safe facade should avoid returning a copyable raw
descriptor value as its main abstraction. The dispatch actor and async I/O
proposal replaces the safe file facade with scoped `with_open_*` helpers and
runtime-checked file tokens.

## Safe Wrappers

A safe wrapper around unsafe operations must state its contract in ordinary API
terms. Examples:

1. `/std/io::with_open_read` owns the descriptor, closes it after the callback,
   and checks `CielFile` state before every operation.
2. `/std/async_io::open_async_read` creates the descriptor and dispatch I/O
   channel together, so no raw handle is exposed.
3. `/std/channel` and `/std/sync` use runtime hooks through safe value APIs
   rather than exposing borrowed interior pointers.
4. `/std/actor` uses runtime hooks only after `Message` conversion has
   constructed receiver-owned state, handler, and message values.

The wrapper may contain unsafe blocks. Callers of the wrapper do not write
unsafe when the wrapper's public contract is safe.

## Diagnostics

The compiler should reject:

```ciel
extern "C" {
    c::c_int close(c::c_int fd); // error: imported C function must be unsafe
}

posix::close(fd); // error: call to unsafe function requires unsafe block

impl clone_message(*const Event value) { // error: clone_message is unsafe
    return Ok(*value);
}

unsafe impl printable(*const Event value) { // error: printable is not unsafe
    return "...";
}
```

An unsafe block is not required merely to pass `T: Message` or call a safe API
whose signature requires `T: Message`. The unsafe proof was made when the
`clone_message` impl was accepted.

## Implementation Plan

1. Parse `unsafe` before blocks, function declarations, extern blocks,
   interfaces, and impls.
2. Mark function and interface definitions with an unsafe flag in HIR and THIR.
3. Require imported `extern "C"` function declarations to be in
   `unsafe extern "C"` blocks.
4. Require unsafe calls to appear under an unsafe-block depth.
5. Require unsafe interfaces to be implemented with `unsafe impl`.
6. Reject `unsafe impl` for safe interfaces.
7. Mark `/std/message` policy interfaces as unsafe and update standard-library
   impls.
8. Mark atomic marker interfaces as unsafe unless their impls are entirely
   compiler-owned primitive facts.
9. Move raw descriptor adoption APIs under unsafe low-level modules.

## Tests

1. imported C function declarations require `unsafe extern "C"` and calls
   require `unsafe {}`;
2. safe wrappers may call unsafe C functions internally;
3. `noescape` C declarations require an unsafe extern block;
4. `clone_message`, `share_handle_marker`, `thread_local_marker`, and atomic
   marker interfaces require `unsafe impl`;
5. safe constraints such as `T: Message` do not require unsafe at call sites;
6. `unsafe impl` for a safe interface is rejected;
7. raw pointer casts from `*void` back to a typed pointer require
   `unsafe {}`;
8. raw descriptor adoption requires an unsafe call;
9. generated C remains unchanged except for code paths enabled by accepted
   unsafe source.
