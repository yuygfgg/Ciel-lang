# Ciel

Ciel is a statically typed, ahead-of-time (AOT) compiled language featuring
GC-backed value semantics, deterministic resource management,
compiler-verified concurrency safety, and expressive generics with structural
metaprogramming / reflection. It compiles whole programs to a single generated
C translation unit, then invokes the target system C compiler. The runtime
uses BDWGC (libgc) for garbage collection and a libdispatch-backed actor and
async I/O runtime on supported targets.

> [!Warning]
> Ciel is still in early experimental phase. **No guarantees are provided.**
> **Only MacOS and Linux are supported.** Windows is not supported yet.

## Disclaimer

Generative AI / large language models (LLMs) were used in the design and
implementation of Ciel. Do not use this language unless you are comfortable
with the risks of using software that may have been influenced by
AI-generated content.

## Building the Compiler

**Prerequisites:**

- [Rust toolchain](https://rust-lang.org/)
- [Clang](https://clang.llvm.org/)
- [CMake](https://cmake.org/)
- [BDWGC](https://github.com/ivmai/bdwgc)
- [libdispatch](https://github.com/swiftlang/swift-corelibs-libdispatch)
- [blocksruntime](https://github.com/swiftlang/swift-corelibs-blocksruntime)
- [Botan](https://botan.randombit.net/)

> [!Note]
> libdispatch and blocksruntime are only required for Linux. On MacOS, they are system-provided.

> [!Note]
> Only Clang is supported as the backend C compiler for its
> [Blocks](<https://en.wikipedia.org/wiki/Blocks_(C_language_extension)>) support.

```sh
# Build the compiler
cargo build

# Build in release mode
cargo build --release
```

## Common usage

The compiler binary is `cielc`. You can invoke it via `cargo run` during
development, or directly after installing the release binary.

### Compile a Single File

```sh
# Compile a .ciel source file to an executable
cargo run -- path/to/file.ciel -o /tmp/my_program

# Run the resulting binary
/tmp/my_program
```

### Compile a Project (TOML Manifest)

Ciel projects use a `ciel.toml` manifest. Point the compiler at the manifest
and specify the entry point:

```sh
cargo run -- --manifest-path path/to/ciel.toml --std-path "$PWD" --entry main -o /tmp/my_project
```

- `--manifest-path`: path to the `ciel.toml` project manifest
- `--std-path`: path to the standard library root (use the repository root)
- `--entry`: the name of the entry function to compile, as declared in the manifest

### Emit C Without Compiling

To inspect the generated C translation unit:

```sh
cargo run -- --emit-c path/to/file.ciel -o /tmp/app.c
```

> [!Note]
> See `cargo run -- --help` for all compiler options.

## VS Code Extension

The VS Code extension provides syntax highlighting, Tree-sitter-based
parsing, and a syntax tree inspection command for `.ciel` files.

### Install from Source

```sh
cd editors/vscode-ciel
npm install
npm run build
npx @vscode/vsce package
code --install-extension vscode-ciel-0.1.0.vsix
```

After installation, open any `.ciel` file to get syntax highlighting. Use
the `Ciel: Show Tree-sitter Syntax Tree` command from the VS Code command
palette to inspect the parser output.

### Development

```sh
cd editors/vscode-ciel
npm install
npm run build        # regenerate Tree-sitter parser and wasm
npm test             # run parser smoke tests and highlighting tests
```

## Language Features

- **Value semantics with GC backing** — automatic memory management without
  manual lifetimes
- **Actor-based concurrency** — compiler-verified message passing without data races
- **Async/await** — stackless coroutines with `async` / `await` and `select`, based
  on the Actor model
- **Deterministic resource management** — affine resource types, `defer`,
  scoped owners, and auto-close for non-memory resources
- **Structural metaprogramming** — compile-time reflection over types,
  derivable trait implementations, and schema generation
- **Interfaces and capabilities** — capability-based type system with
  inferred capability types
- **Pattern matching** — `switch` with exhaustive enum matching
- **Error handling** — ADT-based `Result` and `Option` types with suffix `?` operator
- **C interop** — `extern` declarations, raw pointers, and C callbacks
- **Conditional compilation** — `#if` / `#elif` / `#else` at the source level

## Standard Library

The standard library (`std/`) contains 40+ packages organized by category:

| Category             | Packages                                                                           |
| -------------------- | ---------------------------------------------------------------------------------- |
| **Concurrency**      | `actor`, `async`, `channel`, `message`, `sync`                                     |
| **Data Structures**  | `buf`, `bytes`, `map`, `option`, `result`, `slice`, `vec`, `shared_map`, `storage` |
| **I/O & Networking** | `io`, `io_posix`, `net`, `async_io`, `async_net`, `async_time`                     |
| **Serialization**    | `wire`, `json`, `codec`                                                            |
| **Text & Format**    | `ascii`, `text`, `format`, `base64`, `parse`                                       |
| **Math & Crypto**    | `math`, `crypto`, `sort`                                                           |
| **System**           | `os`, `env`, `time`, `panic`, `error`                                              |
| **Meta/Reflection**  | `meta`                                                                             |
| **Utilities**        | `iter`, `lib`, `resource`, `ord`, `c`                                              |

## Examples

- **[intranet_tunnel](examples/intranet_tunnel/)** — a multiplexed,
  authenticated TCP tunnel implementing both server and agent programs using
  actors, async I/O, wire protocol serialization, and HMAC authentication.
  See its [README](examples/intranet_tunnel/README.md) for build and run
  instructions.
- **[intranet_tunnel_go](examples/intranet_tunnel_go/)** — a Go reference
  implementation of the same tunnel protocol for comparison.
- **[benchmark](examples/benchmark/)** — benchmarks.

## Project Structure

### Compiler

```
src/
├── lexer.rs             # Tokenization (logos-based)
├── parser.rs            # Parsing
├── ast.rs               # Abstract syntax tree
├── source.rs            # Source file handling
├── resolve.rs           # Name resolution
├── hir.rs / checked.rs  # High-level and checked IR
├── types.rs             # Type representations
├── typeck/              # Type checking (inference, capabilities, async, etc.)
├── thir/                # Typed HIR lowering
├── mono.rs              # Monomorphization
├── escape.rs            # Escape analysis
├── codegen/             # C code generation
├── build/               # Build system (manifests, packages, native builds)
├── driver.rs            # Compiler driver
├── main.rs              # CLI entry point
└── lib.rs               # Library entry point
```

### Runtime

```
runtime/
├── include/             # Public C headers (ciel_runtime.h, ciel_actor.h, etc.)
├── src/                 # C source files (actor.c, async.c, gc.c, io.c, etc.)
└── CMakeLists.txt       # CMake build for the runtime
```

### Standard Library

```
std/                     # Standard library packages, each with a ciel.toml
├── actor/ async/ channel/ ...
├── buf/ bytes/ map/ vec/ ...
├── io/ net/ wire/ json/ ...
└── ...
```

### Tests

```
tests/
├── cases/               # .ciel fixture files
├── ciel_cases.rs        # Fixture test runner
├── KNOWN_FAILURES.md    # Tracked regressions
└── ...
```

## Documentation

- **[design.md](design.md)** — the normative language specification
- **[async-await-detailed.md](async-await-detailed.md)** — detailed async/await notes
- **[proposal/](proposal/)** — active and completed design proposals
- **[AGENTS.md](AGENTS.md)** — repository guidelines for llm agents
