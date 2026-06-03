# Package, Runtime, and Compiler Organization Proposal

This proposal reorganizes Ciel around package-owned build metadata, split
runtime/native support code, modular standard-library packages, and a more
maintainable compiler structure. It is inspired by tin's engineering shape, but
it deliberately avoids tin's source-comment build directives and arbitrary shell
expansion.

This document uses the current implementation and `design.md` as the baseline.
Older proposal files are useful context, but they are not treated as source of
truth when they disagree with the working compiler.

Reference implementation to study:

- tin repository: <https://github.com/Azer0s/tin>
- tin standard library and optional libraries: `stdlib/` and `libs/`
- tin runtime C code: `runtime/`
- tin package-owned native directives: leading `//!+file.c` and `//!-lfoo`
  lines in `.tin` source files
- tin compiler package loading and directive scanning:
  `codegen/packages*.go` and `cmd/tin/main_target.go`

The useful tin lesson is not that all build metadata should live in comments.
The useful lesson is that a library should own its native implementation and
native dependencies, while the compiler driver should collect the selected
CMake targets and execute a generic build plan.

## Problem

Ciel currently has three organization problems that will get worse as the
language and standard library grow.

First, `src/runtime_prelude.c` is a single generated-C prelude containing core
runtime support, GC integration, async, actors, channels, networking, atomics,
crypto/Botan bindings, and many standard-library helper functions. It is easy
to append to, but it has become a global dumping ground. Adding UDP, richer
serialization support, TLS, filesystem helpers, or more crypto backends would
make this file larger and harder to reason about.

Second, native dependencies are hard-coded in the compiler pipeline.
`bdw-gc`, Botan, pthreads, libdispatch, BlocksRuntime, and profile-specific
linker flags are selected in the CLI driver rather than being owned by the
runtime or standard-library module that needs them. This makes optional
standard-library features and third-party libraries awkward.

Third, the compiler has a clean high-level pipeline, but important
implementation details are concentrated in very large files. `typeck.rs` and
`codegen.rs` carry many separate domains: async, actors, meta representation,
closures, retained capability witnesses, dynamic interfaces, runtime hooks, and
C emission. Future features should not keep expanding those files.

Current implementation anchors:

- `src/codegen.rs` embeds `src/runtime_prelude.c` through one
  `include_str!`.
- `src/main.rs` hard-codes GC, Botan, pthreads, dispatch, BlocksRuntime, and
  profile-specific linker flags in the native build path.
- `src/driver.rs` special-cases `/std/async` when parsing detects async syntax.
- `src/typeck.rs` and `src/codegen.rs` are already large enough that adding more
  feature-specific logic directly to them has high maintenance cost.

## Goals

1. Add declarative package metadata for Ciel projects, standard-library
   packages, runtime packages, and third-party packages.
2. Let a manifest declare the project/package root, Ciel source files, CMake
   native targets, and their target filters.
3. Avoid arbitrary shell execution in package metadata.
4. Split `runtime_prelude.c` into runtime and package-owned native units.
5. Move standard-library native helper code out of the compiler prelude and
   into the standard-library packages that need it.
6. Replace hard-coded special cases such as importing `/std/async` with a
   generic compiler prelude mechanism.
7. Refactor the compiler into smaller modules and explicit pass boundaries
   without introducing a new MIR in the first step.
8. Preserve the existing design-aligned implementation and regression suite
   while changing organization incrementally.

## Non-Goals

1. A public package registry in the first step.
2. Arbitrary shell snippets in package metadata.
3. Replacing the generated C backend.
4. Introducing MIR in this proposal.
5. Supporting fully general build systems with compiler-specific scripting.
6. Requiring every single-file program to write a manifest.
7. Adding a `build.ciel` build script in the first version.

## Tin Model To Borrow

tin organizes source code into separate top-level areas:

```text
ast/
lexer/
parser/
types/
codegen/
runtime/
stdlib/
libs/
cmd/
repl/
format/
```

tin's standard library is split by package, and many packages carry adjacent C
helpers. For example, tin has `stdlib/net/udp/udp.tin` next to
`stdlib/net/udp/udp.c`, and `stdlib/sync/channel.tin` next to
`stdlib/sync/channel_arc.c`.

tin packages can declare native build requirements near the package source:

```text
//!+udp.c -- -I $TIN_RUNTIME
//!+channel_arc.c -- -lpthread
//!-lraylib
//!-framework Accelerate [darwin]
//!-lopenblas [linux]
```

The tin driver scans the entry file and imported package files, collects C
sources and linker flags, deduplicates them, compiles native sources, and links
them with the program. tin also has `libs/` roots for optional libraries such
as BLAS and raylib.

Ciel should borrow the ownership boundary, not the exact syntax. Ciel package
metadata should be structured TOML, validated by the compiler, and restricted
to known native build actions.

That gives Ciel the part of tin that scales: packages carry their own native C
files and link needs, and the compiler driver aggregates those needs from the
actual import graph. Ciel should not copy the fragile part: build information
hidden in source comments, raw linker strings with loose substitution rules, or
package metadata that can grow into shell scripting.

## Package Metadata

A Ciel package root may contain a `ciel.toml` manifest. Single-file programs
without a manifest continue to work. The compiler treats a manifest as the
authoritative description of public package exports and native build
requirements.

The manifest declares four boundaries explicitly:

1. the project or package root;
2. the public Ciel import paths exposed by the package and their source files;
3. CMake native targets owned by the package.

The first manifest version should use this concrete shape:

```toml
manifest_version = 1

[package]
name = "std.async_net"
kind = "stdlib"
root = "."

[ciel.exports]
"/std/async_net" = "async_net.ciel"

[[native.cmake]]
path = "native/CMakeLists.txt"
target = "ciel_std_async_net"
when = { os = ["linux", "macos"] }
```

Top-level fields:

1. `manifest_version`: required integer. Version `1` is the schema described
   here.
2. `[package]`: required package identity.
3. `[ciel.exports]`: required for packages that expose Ciel modules.
4. `[[native.cmake]]`: optional native CMake targets.

`[package]` fields:

1. `name`: required stable package id, using lowercase segments separated by
   `.`.
2. `kind`: one of `project`, `stdlib`, `runtime`, or `library`.
3. `root`: package root relative to the manifest file. It defaults to `"."`.

`[ciel.exports]` fields:

1. Each key is a public absolute import path exposed by this package. The
   import path syntax remains compatible with current `/std/...` imports.
2. Each value is the `.ciel` source file for that import path, relative to
   `package.root`. Globs are not part of version 1.

Version 1 uses one manifest per package directory. A package may contain
multiple Ciel source files and may expose multiple import paths through
`[ciel.exports]`. Import path mapping is explicit; the compiler must not infer
import paths or source files from `package.name`. Non-public helper files are
loaded through normal relative imports from exported source files and do not
need to be listed in the manifest.

Package metadata may describe CMake native targets:

1. `native.cmake`: a CMake target. All package-owned native source code and
   native dependency discovery go through this path in version 1.

`native.cmake` fields:

1. `path`: required `CMakeLists.txt` path.
2. `target`: required CMake target name.
3. `when`: optional target filter.

Version 1 should not compile package-owned native sources directly and should
not infer source language from extensions. CMake owns the native language
selection, compiler flags, include directories, generated headers, source
graph, dependency discovery, and target link interface. The Ciel manifest only
points at the CMake target.

Version 1 should avoid raw `cflags` and link flags in package metadata. Add
new CMake targets or CMake target properties instead when a real
standard-library package needs native flags.

The proposal chooses CMake as the only external build descriptor for version 1.
Make is deliberately excluded because Make recipes are shell commands. CMake is
not perfectly declarative either. Version 1 allows CMake for the entry project
and compiler-shipped runtime/standard-library packages by default. CMake targets
loaded from `--package-root` require `--allow-native-build`.

Builtin runtime and standard-library packages should use small CMake targets
too, even when the native code is only one `.c` file.

The descriptor is intentionally a file reference plus structured fields. A
package can provide `CMakeLists.txt`, static archives, generated headers, or
object files through a CMake target, but the manifest cannot say `run this
command`.

The Ciel driver still owns build orchestration, but executable and
shared-library final links are performed by a generated top-level CMake project.
That project adds the runtime and selected package CMake projects with
`add_subdirectory`, then links the generated C target against the selected
CMake targets. Native dependencies such as `Threads::Threads`, Botan,
libdispatch, frameworks, and package-manager discoveries live in package
`CMakeLists.txt` files, not in `ciel.toml`.

## CMake Profile Propagation

Ciel CLI build profiles must be part of the CMake invocation. If the user builds
with Debug or Release, every selected `native.cmake` target receives the same
profile.

Recommended mapping:

1. single-config generators: configure with
   `-DCMAKE_BUILD_TYPE=Debug` or `-DCMAKE_BUILD_TYPE=Release`;
2. multi-config generators: build with `--config Debug` or `--config Release`;
3. all generators: pass `-DCIEL_BUILD_PROFILE=debug` or
   `-DCIEL_BUILD_PROFILE=release` during configure.

The driver should also pass stable target facts when they are known:

```text
-DCIEL_TARGET_OS=<os>
-DCIEL_TARGET_ARCH=<arch>
-DCIEL_TARGET_TRIPLE=<triple>
```

Version 1 should not let package manifests pass arbitrary CMake cache options.
Package defaults belong in `CMakeLists.txt`; driver-owned profile and target
settings are the only CMake variables injected by Ciel.

Target filters use the same vocabulary as Ciel configuration gates:

```toml
when = { os = ["linux", "macos"] }
```

Version 1 only supports `when.os`. Architecture filters, package features, and
profile-specific package selection are deferred until a concrete standard
library package needs them.

All manifest paths are relative to `package.root` and are canonicalized before
use. Version 1 should reject paths that escape the package root unless the path
is supplied by an explicit CLI flag.

## Build Script Policy

Ciel should not add a `build.ciel` equivalent of Rust's `build.rs` in the first
version. A build script is project code executed by the compiler driver, so it
creates a trust boundary, cache invalidation problem, host/target distinction,
and filesystem/network policy problem before the package model itself is
stable.

The first version should stay declarative: manifests describe CMake targets and
package exports. If Ciel later needs build scripts, they should be a separate
proposal with a restricted API that emits `BuildPlan` fragments, declares
inputs and outputs, and requires explicit user opt-in for side effects.

## Package Discovery

The compiler should search package roots in this order:

1. the entry project's manifest root;
2. builtin runtime packages shipped with the compiler;
3. builtin standard-library packages shipped with the compiler;
4. user-specified package roots from `--package-root`;
5. relative imports next to the importing source file.

The exact path syntax can remain compatible with current Ciel imports. The
important change is that a resolved import points to a package-owned module
record, not only to a `.ciel` file path. A package may expose one or more Ciel
modules, and may attach native build requirements to those modules.

The current `--std-path` option can remain as a compatibility alias for the
builtin standard-library root during migration. Version 1 should not add
`CIEL_PACKAGE_PATH`; hidden global package state makes tests and bug reports
harder to reproduce.

## Generic Prelude Stdlib

Ciel currently has hard-coded import behavior for `/std/async` when async
syntax is used. That should become a generic compiler prelude mechanism, but
the first version should not make prelude imports depend on source inspection.

The first version should use a hard-coded compiler-owned prelude list:

```rust
const COMPILER_PRELUDE_IMPORTS: &[&str] = &[
    "/std/result",
    "/std/error",
    "/std/panic",
    "/std/async",
];
```

This replaces the existing `/std/async` special case with a fixed prelude
import set. The loader imports configured prelude modules before ordinary
resolution, and it does not consult parser facts, AST flags, names found in
source text, resolved definitions, or type-checking results.

User package manifests cannot declare, override, or disable
`COMPILER_PRELUDE_IMPORTS`. Prelude membership is part of the compiler
implementation. During migration, `--std-path` may point those import paths at
a different standard-library root, but it does not change the hard-coded list.

Prelude imports are still real modules. They are loaded, resolved, checked, and
monomorphized like ordinary standard-library source. The only special behavior
is that the compiler decides when the import is required.

Version 1 should not support syntax-triggered or type-triggered prelude imports.
Syntax-triggered imports keep the loader coupled to parser flags. Type-triggered
imports such as "import `/std/message` when a `Message` constraint is seen" are
also unsafe as a pre-resolution check: a user package can define its own
`Message`, and the compiler only knows whether a definition is the
standard-library `Message` after the relevant modules have already been loaded
and resolved. Doing this late would require type checking, discovering a missing
prelude module, then rewinding to load and type check again.

If `/std/message` becomes part of the compiler prelude surface, it should be
added to `COMPILER_PRELUDE_IMPORTS`. Otherwise libraries that need
`/std/message` should import it explicitly. Existing standard-library identity
checks in `std_id.rs` can continue to validate that a resolved definition is the
intended std definition, but they should not trigger new imports in phase 1.

## Runtime Split

`runtime_prelude.c` should be split into runtime packages. The initial split
should be mechanical and preserve behavior:

```text
runtime/
  include/
    ciel_runtime.h
    ciel_gc.h
    ciel_async.h
    ciel_actor.h
    ciel_net.h
    ciel_crypto.h
  core.c
  gc.c
  args.c
  alloc.c
  error.c
  panic.c
  async.c
  async_io.c
  actor_dispatch.c
  sync_channel.c
  atomic.c
  net_tcp.c
  crypto_botan.c
```

Each runtime unit has package metadata:

```toml
manifest_version = 1

[package]
name = "runtime.crypto_botan"
kind = "runtime"
root = "."

[[native.cmake]]
path = "CMakeLists.txt"
target = "ciel_runtime_crypto_botan"
```

The compiler owns a small runtime package registry. In version 1 the driver
builds a fixed runtime target set for every executable build. This removes the
need to paste a complete C runtime into every generated C file without adding a
new codegen-to-driver feature selection API.

The compiler distribution owns these runtime packages. The first
implementation may still embed or copy them from compiler resources and build
all runtime CMake targets by default for simplicity. The architectural change
is that runtime units are separate CMake targets with separate metadata. After
that, selecting only needed units through a future `RuntimeNeed` mechanism is
an incremental optimization.

## Standard Library Split

Standard-library modules with native helpers should own those helpers.

Example layout:

```text
std/
  async/
    ciel.toml
    core.ciel
    channel.ciel
    native/CMakeLists.txt
    native/channel.c
  async_net/
    ciel.toml
    async_net.ciel
    native/CMakeLists.txt
    native/tcp_posix.c
  crypto/
    ciel.toml
    crypto.ciel
    native/CMakeLists.txt
    native/botan.c
  atomic/
    ciel.toml
    atomic.ciel
    native/CMakeLists.txt
    native/atomic.c
```

The `/std/crypto` package should own Botan requirements. The `/std/async_net`
package should own TCP/UDP helper sources. `/std/channel` and `/std/atomic`
should own their native synchronization helpers. The core runtime should not
know about every standard-library wrapper.

This mirrors tin's structure, where `stdlib/net/udp` owns `udp.c`, `stdlib/sync`
owns channel and mutex helpers, and optional packages such as `libs/raylib`
declare their own native dependencies.

## Build Plan

The compiler driver should stop treating `compile_to_c` as the complete build
result. It should produce a build plan:

```rust
struct BuildPlan {
    generated_c: String,
    profile: BuildProfile,
    cmake_targets: Vec<CmakeTarget>,
    package_inputs: Vec<PathBuf>,
}
```

```rust
struct CmakeTarget {
    package_root: PathBuf,
    cmake_file: PathBuf,
    target: String,
}
```

`compile_to_c` can stay as a compatibility API for tests and `--emit-c`.
Executable/object/shared builds should use `compile_to_build_plan`.

The build plan is assembled from:

1. entry project package metadata;
2. imported Ciel packages;
3. compiler prelude packages;
4. builtin runtime packages;
5. standard-library package metadata;
6. explicit CLI flags such as `--cflag` and `--ldflag`.

The driver deduplicates CMake targets by canonical path and target name. It
should include the build metadata and selected profile in any future build cache
key.

The driver can still expose `--cflag` and `--ldflag` escape hatches for local
experimentation. Package manifests should not expose native cflags or link
flags in version 1; package CMake targets own them.

## Compiler Refactor Without MIR

This proposal does not introduce MIR. The first compiler refactor is a
structural split around the existing AST, HIR, THIR, monomorphization, escape
analysis, and C codegen pipeline.

Recommended module layout:

```text
src/
  build/
    manifest.rs
    package.rs
    requirements.rs
    planner.rs
    native.rs
  driver/
    mod.rs
    loader.rs
    prelude.rs
    config.rs
  typeck/
    mod.rs
    context.rs
    types.rs
    expr.rs
    stmt.rs
    pattern.rs
    interfaces.rs
    meta.rs
    async_checks.rs
    diagnostics.rs
  codegen/
    mod.rs
    context.rs
    c_emit.rs
    decl.rs
    expr.rs
    stmt.rs
    async.rs
    actor.rs
    closure.rs
    meta.rs
    interface.rs
    runtime.rs
    names.rs
```

The first step is to move code without changing behavior:

1. split package loading and configuration preprocessing out of `driver.rs`;
2. add manifest parsing and build requirement data types;
3. split type checking by expression, statement, pattern, interface, async, and
   meta domains;
4. split C code generation by declarations, expressions, statements, async,
   actors, closures, meta, and runtime hooks;
5. add generic AST/HIR walker utilities where they remove ad hoc traversals;
6. keep THIR visitor usage for typed tree traversals;
7. keep the public compile API and generated C output stable while files move.

After these splits, MIR can be designed as a separate proposal or follow-up.
The split should make MIR easier to add, but not block on it.

## Migration Plan

### Phase 1: Build Metadata Skeleton

1. Add the `manifest_version = 1` `ciel.toml` parser and validator.
2. Add `BuildPlan` and `CmakeTarget`.
3. Keep current CLI behavior while the metadata skeleton lands.
4. Keep `compile_to_c` unchanged for tests.
5. Do not add `build.ciel`.
6. Thread the CLI build profile through `BuildPlan`.

### Phase 2: Runtime Split

1. Move `runtime_prelude.c` into multiple runtime C files.
2. Add runtime headers under `runtime/include`.
3. Teach codegen to emit declarations/includes for runtime headers instead of
   pasting the whole runtime body.
4. Add CMake targets for runtime units.
5. Teach the driver to link generated C through a top-level CMake project that
   consumes runtime CMake targets.
6. Include all runtime targets in version 1.

### Phase 3: Standard-Library Package Split

1. Add manifests to standard-library modules.
2. Move standard-library native helpers out of runtime units.
3. Move Botan requirements to `/std/crypto`.
4. Move net helper requirements to `/std/async_net` and future UDP packages.
5. Replace hard-coded `/std/async` loading with unconditional compiler prelude
   loading.
6. Keep `/std/message` and similar type-level dependencies as explicit imports.

### Phase 4: Third-Party Package Roots

1. Add `--package-root`.
2. Define package root resolution rules.
3. Allow third-party package manifests to declare CMake targets and final link
   requirements.
4. Gate third-party CMake execution behind driver policy and pass the selected
   Debug/Release profile to CMake.
5. Add one repository-local example package modeled after tin's `libs/raylib`
   or `libs/blas`, but with TOML metadata instead of `//!` directives.

### Phase 5: Compiler File Refactor

1. Split `typeck.rs` into a module directory.
2. Split `codegen.rs` into a module directory.
3. Move runtime hook naming and std package ids into dedicated registries.
4. Keep behavioral changes minimal and rely on the existing fixture suite.

## Deferred Beyond Version 1

1. Public package registry and semantic version resolution.
2. Package features and architecture-specific package filters.
3. Manifest-provided CMake cache options.
4. Environment package search paths such as `CIEL_PACKAGE_PATH`.
5. Runtime target pruning through a `RuntimeNeed` mechanism.
6. A restricted `build.ciel` build script API.
7. A future MIR.

## Resolved Direction

1. Use TOML metadata, not source-comment build directives.
2. Do not support arbitrary shell snippets in package metadata.
3. Keep package-owned native metadata in `ciel.toml`, not in `.ciel` source
   files.
4. Use CMake as the only external build descriptor in version 1; do not support
   Makefile descriptors.
5. Require all package-owned native code to go through CMake in version 1; do
   not compile manifest-listed native sources directly and do not infer
   languages from file extensions.
6. Pass the selected CLI Debug/Release build profile to every selected CMake
   target.
7. Support only `when.os` target filters in version 1.
8. Do not let package manifests pass arbitrary CMake cache options in version
   1.
9. Require `--allow-native-build` for CMake targets loaded from
   `--package-root`.
10. Do not introduce `build.ciel` in version 1.
11. Do not add `CIEL_PACKAGE_PATH` in version 1.
12. Runtime packages are build-planning inputs, not user-importable modules.
13. Keep `compile_to_c` for tests and `--emit-c`; executable/object/shared
   builds use `BuildPlan`.
14. Let packages own their native code and CMake target dependencies.
15. Let the compiler collect package CMake targets into a build plan.
16. Use only unconditional hard-coded compiler prelude imports in version 1; do
   not add source-based or type-triggered prelude rules.
17. Keep the compiler prelude as an implementation constant, not TOML metadata
   and not user `ciel.toml`.
18. Split runtime and standard-library native C code before adding more native
   standard-library features.
19. Refactor compiler structure before introducing MIR.
