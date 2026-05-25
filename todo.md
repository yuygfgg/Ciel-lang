# Ciel Compiler TODO

This file tracks gaps between the current Rust compiler implementation and
`design.md`. It includes both missing features and implemented behavior that is
currently inconsistent with the design.

## P0: Runtime, GC, And Safety

- [x] Integrate BDWGC/libgc in generated C. Generated C includes `<gc/gc.h>`,
      calls `GC_INIT()`, and allocates through `GC_MALLOC()`.
- [x] Allocate all promoted locals and compiler-created backing storage with the
      GC.
- [x] Implement the conservative escape-analysis rules from the spec:
      return escape, global/runtime storage escape, heap-object escape, unknown C
      calls, thread-entry data, function summaries, fixed-point iteration, and
      conservative dynamic-interface calls.
- [x] Honor `noescape` on imported `extern "C"` functions. It is parsed but not
      used outside trusted C escape contracts.
- [x] Keep slice backing storage alive when slices escape. Array-to-slice and
      slice-producing literals use GC-backed storage when required.
- [x] Emit real runtime hooks:
      `ciel_runtime_init`, `ciel_thread_attach`, `ciel_thread_detach`,
      `ciel_root_pin`, `ciel_root_get`, and `ciel_root_unpin`.
- [x] Emit the shared-library constructor or target-equivalent initializer for
      Ciel runtime initialization.
- [x] Register host-created threads with BDWGC before they call Ciel code.
- [x] Implement explicit root handling for C memory that stores Ciel GC pointers.
- [x] Add bounds checks for array and slice indexing.
- [x] Implement integer division-by-zero panic for integer `/` and `%`.
- [x] Implement debug integer-overflow traps and release wrapping semantics.
- [x] Make string literal backing storage safe for mutable `[]char`.

## P0: Error Model And Panic

- [x] Implement the `?` operator for `Result<T, E>` from `/std/result`.
- [x] Ensure `?` requires the enclosing function to return `Result<U, E>` with an
      exactly matching error type.
- [x] Lower `?` without C extensions: expression lowering supports
      pre-statements and early return.
- [x] Implement or provide standard-library `must` and `expect` as ordinary
      generic functions.
- [x] Emit readable panic diagnostics and source locations. Current `ciel_panic`
      reports a message and source location before terminating.
- [x] Ensure panic always terminates with exit status `0` and never runs defer
      handlers.

## P0: Name Resolution, Modules, And Imports

- [x] Replace string-based expression names with resolved `DefId`/local IDs in
      HIR/THIR. THIR keeps display names only for diagnostics and C emission.
- [x] Build a complete HIR layer where global identifiers are bound before type
      checking.
- [x] Enforce the single namespace for values, functions, types, enum variants,
      interfaces, and aliases in visible scopes.
- [x] Implement lexical shadowing exactly: declarations shadow outer declarations
      from their declaration point forward.
- [x] Implement ambiguous bare lookup according to import visibility, not by a
      whole-program name search.
- [x] Complete `import ./x` semantics for exported functions, structs, enums,
      enum variants, interfaces, interface aliases, and opaque structs.
- [x] Complete `import ./x as y` semantics for exported functions, structs,
      enums, enum variants, interfaces, interface aliases, and opaque structs.
- [x] Ensure bare lookup never searches alias namespaces.
- [x] Implement `export import ./x` re-export of bare names.
- [x] Implement `export import ./x as y` re-export of the alias namespace.
- [x] Enforce that non-exported imported structs, enums, interface aliases,
      interfaces, opaque structs, and variants are not usable by importers.
- [x] Add correct diagnostics for unresolved imported modules and unresolved
      import targets.
- [x] Support standard-library absolute imports such as `/std/result` with an
      actual standard-library source tree.

## P0: Configuration Gates And Preprocessing

- [x] Implement item-level `#if`, `#elif`, `#else`, `#endif`.
- [x] Implement restricted config functions:
      `has_feature`, `is_target_os`, and `is_target_arch`.
- [x] Treat unknown features as `false`.
- [x] Tokenize inactive branches and skip syntax/semantic analysis for them.
- [x] Reject config gates inside statements, parameter lists, type lists, and
      expressions.
- [x] Add CLI or build-system inputs for target OS, target arch, and features.
- [x] Preserve `#c_include` declarations in generated C.

## P0: Generics And Monomorphization

- [x] Implement generic function type checking and inference for explicit type
      arguments, argument types, constraints, and expected result type.
- [x] Implement reachable generic function instantiation in `mono.rs`. Type
      checking now leaves typed generic call placeholders in THIR, and the mono
      pass creates concrete instances, rewrites calls, and walks generated
      generic bodies.
- [x] Implement generic struct instantiation and monomorphized layouts in the
      final mono output.
- [x] Implement generic type aliases.
- [x] Keep generic enum support but move it into the real monomorphization
      pipeline.
- [x] Use the root-set rule from the design: if `main` exists, roots include
      `main` and all `extern "C"` functions; otherwise roots are `extern "C"`
      functions only.
- [x] Reject unresolved generic parameters after full unification for generic
      calls.
- [x] Detect infinite generic instantiation cycles with mono instantiation-chain
      growth checks while allowing same-instance recursion and large finite
      instance sets.
- [x] Infer generic calls from explicit type arguments, argument types,
      constraints, and expected result type.

## P0: Interfaces, Impl Table, And Capabilities

- [x] Type-check `interface` declarations instead of rejecting them.
- [x] Type-check `impl` declarations instead of rejecting them.
- [x] Build the global impl table during semantic collection.
- [x] Check impl signatures against interface signatures after receiver and
      non-receiver type substitution for concrete impls.
- [x] Implement static capability calls for concrete receiver types.
- [x] Implement generic constraints such as `T: read + write + !seek`.
- [x] Implement whole-program `!capability` hard rejection.
- [x] Implement interface aliases with `+` and `-` view composition for
      constraints and dynamic interface views.
- [x] Implement dynamic interface values as data pointer plus vtable pointer.
- [x] Enforce dynamic-interface rules: first generic parameter is erased, later
      generic parameters must be supplied by the dynamic type, and the receiver
      must appear in an input parameter.
- [x] Generate vtables and shims for dynamic interface dispatch.
- [x] Ensure dynamic interface calls are conservative in escape analysis.
- [x] Keep `make` as an ordinary capability with no compiler special case.

## P0: Types And Type Checking

- [x] Implement transparent type aliases.
- [x] Emit and type-check opaque structs from `extern "C"` blocks.
- [x] Treat `void` as an implicit zero-size value in locals, fields,
      parameters, enum payloads, pattern bindings, and `[N]void` arrays.
- [x] Reject explicit concrete initialization, assignment, and address-taking of
      `void` values.
- [x] Check by-value recursive struct/enum layout cycles.
- [x] Implement nullable local narrowing:
      `if (p != null)`, `if (p == null) return`, and `&&` short-circuit cases.
- [x] Ensure narrowing never applies to fields or arbitrary expressions.
- [x] Invalidate nullable narrowing after reassignment or address-taking.
- [x] Reject address-taking of immutable parameter bindings.
- [x] Implement literal range checks for all integer, float, and char targets.
- [x] Enforce cast rules. Current casts are accepted broadly and lowered to C
      casts.
- [x] Enforce pointer-cast rules involving only `*void` and `?*void`.
- [x] Ensure nullable-to-non-null conversion requires narrowing.
- [x] Implement array-to-slice conversion for existing arrays. Current assignment
      from `[N]T` to `[]T` is not supported.
- [x] Implement array literal lowering when the expected type is `[]T`; it needs
      compiler-created backing storage.
- [x] Prevent struct and enum `==` / `!=`. Current type checking may accept equal
      nominal types, but generated C cannot compare structs.
- [x] Ensure function type values are real function pointer types, not `void *`.
- [x] Preserve function pointer ABI, including nested `extern "C"` function
      types.
- [x] Support direct calls through function pointer values.
- [x] Complete slice semantics for empty slices: non-null `ptr`, not
      dereferenceable when `len == 0`.

## P0: Definite Assignment And Control Flow

- [x] Implement formal compile-time definite-assignment dataflow.
- [x] Merge `if` branch assignment state by intersection.
- [x] Ensure loop bodies do not prove post-loop assignment without stronger
      control-flow proof.
- [x] Treat read, address take, return, field access, and index access as
      requiring definite assignment.
- [x] Reject `break` and `continue` outside valid loop/switch contexts.
- [x] Track whether `break` targets a loop or switch.
- [x] Complete return-path analysis for all supported control flow.

## P0: Defer

- [x] Evaluate deferred call arguments at the `defer` statement, not at block
      exit.
- [x] Restrict `defer` to a single direct function call.
- [x] Reject suffixes such as `?` after a deferred call.
- [x] Preserve strict LIFO execution for all normal exits.
- [x] Ensure block-level loop semantics: a `defer` inside a loop body runs at the
      end of that iteration.
- [x] Ensure `continue` runs block defers before the loop step expression.
- [x] Avoid emitting duplicate/unreachable deferred calls after `return`.
- [x] Ignore deferred call return values.

## P1: Enum And Pattern Matching

- [x] Keep enum payload constructors as ordinary names in the single namespace
      under full module visibility rules.
- [x] Enforce variant-name conflicts across visible scopes.
- [x] Support top-level wildcard pattern only where the spec allows it, or
      document that `default:` is the only fallback.
- [x] Implement nested enum patterns.
- [x] Ensure pattern bindings use copy semantics under full value semantics.
- [x] Avoid backend fallback code after exhaustive enum switches when the type
      checker has proven all paths return.

## P1: Local Type Holes

- [x] Parse `_` as a type hole in type grammar while keeping pattern `_`
      unchanged.
- [x] Represent type holes in AST/HIR type nodes so partial annotations such as
      `Actor<_>`, `Result<Actor<_>, Error>`, `[]_`, and `[3]_` survive until type
      checking.
- [x] Reject type holes outside initialized local declarations and initialized
      `for` declarations, including function signatures, struct fields, enum
      payloads, interface declarations, impl signatures, type aliases, extern
      declarations, casts, and explicit generic type arguments.
- [x] Require an initializer when a local or `for` declaration contains a type
      hole.
- [x] During local declaration checking, replace holes with fresh inference
      variables and check the initializer against the partial expected type.
- [x] Solve holes using the existing inference rules for literals, generic calls,
      aggregate literals, array literals, nullable pointers, and closure
      literals.
- [x] Reject unresolved holes before the local binding reaches THIR,
      monomorphization, or code generation.
- [x] Preserve ordinary assignment semantics: `_ x = expr` declares a local, but
      `x = expr` remains assignment to an existing binding and misspelled names
      stay unknown-name errors.
- [x] Add diagnostics for missing initializers, illegal hole contexts, unresolved
      holes, `null` without nullable pointer context, empty array literals,
      struct literals without an expected struct type, untyped closure
      parameters, and block-bodied closures without expected return context.
- [x] Add fixtures for whole-local holes, partial aggregate holes, slice and
      fixed-array holes, concrete closure local inference, initialized `for`
      declarations, later assignment type stability, and rejection contexts.

## P1: Structural Metaprogramming

- [x] Add `/std/meta` compiler built-ins `type_size<T>()` and
      `type_align<T>()`, lowered generically to C `sizeof(T)` and
      `CIEL_ALIGNOF(T)`.
- [x] Add `/std/meta` product and sum vocabulary:
      `HNil`, `HCons`, `FieldRef`, `Field`, `CoNil`, `Coproduct`,
      `VariantRef`, `Variant`, `PayloadRef`, and `Payload`.
- [x] Add compiler-recognized marker forms `RefRepr<T>` and `Repr<T>`.
- [x] Normalize `RefRepr<T>` and `Repr<T>` for visible structs during type
      lowering, before generic constraint checking and impl lookup.
- [x] Lower `as_ref_repr<T>(*T)` for visible structs to borrowed structural
      values whose field pointers follow ordinary address-taking and escape
      rules.
- [x] Lower `into_repr<T>(T)` for visible structs to owned structural values.
- [x] Lower `from_repr<T>(Repr<T>)` for visible structs to ordinary struct
      construction by structural position.
- [x] Add tests showing generic policy recursion over `HNil`, `HCons`,
      `FieldRef`, and `Field`.
- [x] Extend normalization and lowering to enums using `CoNil`, `Coproduct`,
      `VariantRef`, `Variant`, `PayloadRef`, and `Payload`.
- [x] Extend structural representation to concrete closure capture
      environments once the struct path is stable.
- [x] Move structural `Message` cloning for user structs and enums to ordinary
      `/std/message` impls over owned `/std/meta` SOP nodes.
- [x] Add diagnostics that report the original source type plus field, variant,
      or capture path when generic structural recursion fails.
- [x] Decide whether a future declaration-level convenience should auto-emit
      wrapper impls; the core mechanism remains explicit projection plus
      ordinary policy code.
- [ ] Move fixed-size array, Ciel ABI `fn`, and concrete-closure message leaves
      out of compiler-known policy once the type system can express those
      library impls.

## P1: Closures

- [x] Parse `T |(A, B)|` callable type suffixes while keeping `T fn(A, B)`
      as the existing noncapturing function-pointer type.
- [x] Parse closure expressions: `|| body` and `|params| body`, where each
      parameter is either `name` or `Type name` and the body is either an
      expression or a block.
- [x] Type-check omitted parameter types from expected callable types, explicit
      parameter types, expression-bodied return inference, and block-bodied
      closure return-path rules.
- [x] Reject closure literals whose parameter or block return types cannot be
      determined from context or an `as Return |(Args)|` annotation.
- [x] Implement capture analysis for bare references to enclosing local
      bindings and parameters, excluding top-level functions, imported names,
      types, variants, and interfaces.
- [x] Enforce by-value capture only: captured locals must be definitely assigned
      at closure creation, and captured bindings inside the closure body are
      read-only snapshots.
- [x] Represent closure values in HIR/THIR/mono as callable values with generated
      environment layouts and call thunks.
- [x] Represent closure literals internally as unique concrete closure-instance
      types and erase them to signature-only closure types only when an erased
      closure is expected.
- [x] Lower closure calls through the generated environment pointer and call
      thunk, while preserving source-order argument evaluation.
- [x] Implement implicit conversions from function items/function pointers to
      matching closure types and from noncapturing closures to matching Ciel-ABI
      `fn` types.
- [x] Treat `as Return |(Args)|` and non-`extern` `as Return fn(Args)` on
      closure literals as compile-time expected-type annotations.
- [x] Reject captured closures converting to `fn`, closure equality, closure
      types in `extern "C"` declarations, and closure expressions that would
      produce C ABI function pointers.
- [x] Integrate closure environments with escape analysis so escaping closures
      keep captured values and slice backing storage alive.
- [x] Emit generated C environment structs and thunks for closures, using GC
      allocation for escaping environments and stack allocation only when proven
      nonescaping.

## P1: Concurrency And Actor Model

- [x] Add built-in recognition for the standard-library `Message` marker and
      `clone_message` interface, resolved through `/std/message` definitions.
- [x] Add built-in recognition for the standard-library `ShareHandle` and
      `ThreadLocal` markers, resolved through `/std/message` rather than by
      user-defined names.
- [x] Keep `ShareHandle` explicit: only synchronized or immutable handle
      wrappers should implement `share_handle_marker`, and the compiler should
      not structurally derive it from fields.
- [x] Treat `ThreadLocal` as an actor-local policy marker. A type with an
      explicit or compiler-known `ThreadLocal` implementation must not satisfy
      `Message` unless it also provides an explicit, policy-defined
      `clone_message` implementation.
- [x] Mark raw pointers, actor-local slices, extern C function pointers, dynamic
      interface values without a concrete message path, and opaque C handles as
      non-derived-message/thread-local cases for constraints.
- [x] Add richer diagnostics for raw pointers, actor-local slices, extern C
      function pointers, dynamic interface values without a concrete message
      path, and opaque C handles when they block `Message` derivation.
- [x] Remove compiler-derived structural `Message` for user structs and enums;
      require either an explicit `clone_message(*T)` impl or a boundary type
      such as `meta::Repr<T>`.
- [x] Diagnose failed `meta::Repr<T>` `Message` policies for raw pointer,
      actor-local slice, dynamic-interface, erased-closure, and opaque-handle
      leaves.
- [x] Use ordinary `/std/message` `clone_message` impls for owned `/std/meta`
      SOP nodes so existing generic constraint checking handles
      `meta::Repr<T>: Message` without actor-specific exceptions.
- [x] Add whole-program coherence checks that prevent duplicate concrete
      `clone_message` implementations.
- [x] Extend coherence checks to reject conflicting marker policies such as
      explicit `ThreadLocal` plus explicit `Message`, duplicate concrete
      `clone_message` implementations, and ambiguous generic marker impls.
- [x] Enforce actor boundaries through `Message` conversion for `spawn_actor`
      state/handler values and `send` payloads.
- [x] Add escape-analysis diagnostics that explain actor-local pointer/slice
      captures in terms of cross-actor movement.
- [x] Generate message clone thunks for concrete closure environments and carry
      the clone operation in closure values. Concrete closure instances implement
      `Message` only when every captured field is `Message`; erased closure
      signature types are not `Message` by default.
- [x] Add runtime mailbox support for actor allocation, enqueue, dequeue,
      wakeup, dispatch, shutdown, and worker-thread GC attachment.
- [x] Generate actor dispatch thunks for each concrete
      `spawn_actor<S, M, H>` handler so the runtime can call `H` as
      `Result<S, Error>(S, M)` and store the next actor state. These thunks are
      runtime glue and must not introduce a separate actor type-system rule.
- [x] Use `/std/meta` from ordinary standard-library Ciel code for generic
      runtime handles that store arbitrary Ciel values, especially `Channel<T>`
      and `Mutex<T>`.

## P1: Standard Library

- [x] Add `/std/error`.
- [x] Add complete `/std/result` with `must` and `expect`. A minimal
      `/std/result` enum exists for `?` lowering.
- [x] Add `/std/panic`.
- [x] Add `/std/c`.
- [x] Add `/std/io`.
- [x] Remove the incorrect `/std/sync` `lock`/`unlock` stub and stop re-exporting
      incomplete concurrency APIs from `/std/lib`.
- [x] Add `/std/lib` as explicit facade re-exporting the implemented standard
      modules.
- [x] Ensure standard-library APIs are ordinary Ciel source except named runtime
      hooks and the generic `/std/meta` type metadata helpers.
- [x] Ensure `Result` recognition for `?` is tied to `/std/result`, not any enum
      named `Result`.
- [x] Add `/std/message` with `clone_message`, `Message`, `ShareHandle`, and
      `ThreadLocal` declarations. Primitive values, `Error`, `Result<T, E>`,
      and owned `/std/meta` SOP nodes are ordinary library impls; actor,
      channel, mutex, and atomic handles define explicit handle policies in
      their own modules.
- [x] Add `/std/actor` with `Actor<M>`,
      `spawn_actor<S: Message, M: Message, H: Message>` where `H` is callable as
      `Result<S, Error>(S, M)`, `send<T: Message>` that calls `clone_message`
      for each payload, actor lifecycle helpers, and mailbox close errors.
- [ ] Add typed mailbox/backpressure error surfaces beyond `Error::Code`.
- [x] Add `/std/channel` as ordinary Ciel code built on the same explicit
      `clone_message` conversion rules.
- [x] Add `/std/channel` runtime hooks as a minimal pthread-backed proof of
      concept only: unbounded queue, close, send, recv, and no long-term API
      commitment to pthread internals. The intended replacement backend is
      libdispatch.
- [x] Add `/std/atomic` for primitive atomics that expose value operations.
- [x] Define atomic memory ordering in `/std/atomic`: `Relaxed`, `Acquire`,
      `Release`, `AcqRel`, and `SeqCst`, including invalid-order diagnostics or
      `Result` errors for operations such as store and compare-exchange failure
      ordering.
- [x] Add primitive atomic APIs for `bool`, `i64`, `u64`, and `usize`: new,
      load, store, exchange, compare-exchange, and integer fetch-add/fetch-sub
      where applicable.
- [x] Make atomic handles explicit `ShareHandle` and `Message` values by sharing
      the synchronized handle, not by cloning the stored value.
- [x] Add revised `/std/sync` as ordinary Ciel code where mutexes expose
      value-update APIs first, not borrowed interior pointers guarded by
      separate `lock` and `unlock`.
- [x] Add `/std/sync` runtime hooks as a minimal pthread-backed proof of concept
      only: allocate a mutex-backed value slot, run value replacement under the
      lock, and keep the public API independent from pthread so it can move to
      libdispatch.
- [x] Ensure `Mutex<T>` stores and returns values through `Message` conversion or
      concrete value metadata, never through a user-visible borrowed interior
      pointer.
- [x] Define `/std/io` wrapper policies: handles are actor-local by default, and
      any `Message` or `ShareHandle` implementation must explicitly duplicate,
      reconnect, or prove synchronized sharing.
- [x] Mark `/std/io::Fd` as `ThreadLocal` so file descriptors no longer receive
      accidental `Message` policy.

## P1: C Interop And ABI

- [x] Emit `#c_include` lines before generated declarations.
- [x] Generate opaque struct forward declarations for C types such as `FILE`.
- [x] Preserve explicit C pointer nullability in type checking.
- [x] Enforce C ABI for `extern "C"` and `export extern "C"` declarations.
- [x] Reject by-value `void` parameters in `extern "C"` declarations while
      allowing erased `void` parameters in the internal Ciel ABI.
- [x] Implement `export extern "C"` symbol naming for function bodies.
- [x] Implement `export extern "C" { ... }` re-export semantics for Ciel
      importers.
- [x] Support external C prototypes without generating invalid duplicate bodies.
- [x] Respect `noescape` only inside `extern "C"` blocks.
- [x] Lower internal Ciel ABI independently from C ABI once large-value lowering
      is added.
- [x] Add C ABI tests for structs, enums, pointers, function pointers, and slices.

## P1: C Backend

- [x] Print generated C in the full dependency-safe phases from the design:
      includes, typedefs/forwards, layouts, prototypes, and bodies.
- [x] Emit type aliases where they affect C output.
- [x] Generate deterministic mangled names for all monomorphized functions and
      types.
- [x] Add `#line` directives mapping generated C back to Ciel source.
- [x] Add runtime support declarations before generated user code.
- [x] Avoid relying on native C undefined behavior for Ciel-defined operations.
- [x] Add C compile-and-run tests for every supported source feature.
- [x] Add C warning-clean tests. Some generated functions currently need fallback
      returns because path analysis is conservative.

## P1: Debug Information

- [x] Preserve generated C files on request.
- [x] Add a mode that invokes the target C compiler with debug flags such as `-g`.
- [x] Emit `#line` directives.
- [x] Keep a source-location table for panic diagnostics.
- [x] Make generated names deterministic and readable enough for debugging.

## P1: CLI And Build Pipeline

- [x] Distinguish `--emit-c` from a full compile mode. Current CLI always emits C.
- [x] Add an option to invoke the system C compiler.
- [x] Add output modes for executable, object file, shared library, and generated C.
- [x] Add target compiler flags and linker flags, including BDWGC/libgc.
- [x] Add feature/target configuration inputs for config gates.
- [x] Add standard-library search paths.
- [x] Add a project root option instead of relying only on current working
      directory.

## P2: Parser And Lexing Completeness

- [ ] Parse config items instead of rejecting `#if`.
- [ ] Validate `fn` contextual keyword behavior and `|(Args)|` parsing with
      complex callable types.
- [ ] Add tests for all type-expression precedence cases in `design.md`.
- [ ] Add tests for trailing commas in every grammar position where accepted.
- [ ] Decode char and string escapes into semantic values instead of carrying raw
      token text through most of the pipeline.
- [ ] Add diagnostics for invalid UTF-8 source files.

## P2: Tests Needed

- [ ] GC promotion test: returning pointer/slice to local storage must remain
      valid.
- [ ] Host-created thread attach/detach test.
- [ ] Nullable narrowing positive and negative tests.
- [ ] Definite-assignment branch merge tests.
- [ ] `defer` argument-evaluation timing tests.
- [x] `defer` LIFO tests across nested blocks, `return`, `break`, and `continue`.
- [x] `?` success and early-return tests.
- [x] Generic function inference tests, including expected-result inference.
- [x] Interface static dispatch tests.
- [x] Dynamic interface vtable dispatch tests.
- [ ] `!capability` whole-program rejection tests.
- [ ] Closure parse/type/lowering tests for expression bodies, block bodies,
      captures, noncapturing conversion to `fn`, and function-pointer wrapping
      as closures.
- [ ] Closure rejection tests for unassigned captures, captured binding
      mutation, captured closure conversion to `fn`, closure equality, and
      closure types in C ABI declarations.
- [x] `Message` tests showing ordinary pointer-free structs no longer derive
      automatically, while `meta::Repr<T>` is accepted when all leaves implement
      `Message`.
- [x] `meta::Repr<T>` rejection tests for raw pointer leaves.
- [ ] `clone_message` tests for explicit wrapper policies, conversion error
      propagation, and rejection of non-`Message` values in generic constraints.
- [ ] Closure `clone_message` tests covering captured primitive values, captured
      owned containers, captured pointers that return `Err`, and actor-local
      slices that return `Err`.
- [ ] Actor and channel send tests that require `T: Message` through ordinary
      generic constraints and call `clone_message` before entering runtime hooks.
- [x] Runtime actor mailbox integration test once `/std/actor` exists.
- [ ] Import ambiguity and shadowing tests.
- [ ] Re-export tests.
- [ ] C ABI interop tests.
- [ ] Bounds-check panic tests.
- [ ] Debug `#line` mapping tests.
