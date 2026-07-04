# Ciel

[![macOS CI](https://github.com/yuygfgg/Ciel-lang/actions/workflows/macos-ci.yml/badge.svg)](https://github.com/yuygfgg/Ciel-lang/actions/workflows/macos-ci.yml)
[![Linux CI](https://github.com/yuygfgg/Ciel-lang/actions/workflows/linux-ci.yml/badge.svg)](https://github.com/yuygfgg/Ciel-lang/actions/workflows/linux-ci.yml)

Ciel is an experimental AOT-compiled programming language built around
compiler-verified memory safety, resource lifetime, and concurrency safety.

It features GC-backed value semantics, deterministic resource management,
compiler-verified concurrency safety, and expressive generics with structural
metaprogramming / reflection. The compiler performs whole-program checking,
translating the whole programs to a single C translation unit, then invokes 
the target system C compiler. The runtime uses BDWGC (libgc) for garbage collection
and a libdispatch-backed actor and async I/O runtime on supported targets.

> [!Warning]
> Ciel is still in early experimental phase. **No guarantees are provided.**

> [!Warning]
> **Only MacOS and Linux are supported.** Windows is not supported yet.

## ⚠️ $\Huge\color{Orange}{\textbf{DISCLAIMER}}$ ⚠️

> [!Warning]
> 
> This is a personal hobby project for exploring programming language design.
> 
> **Large Language Models (LLMs) are used extensively.**
> 
> If you feel uncomfortable with this, you are welcome to stop reading.
> No offense is intended.
> 
> Recommended reading: <https://codeberg.org/ethical-foss/open-slopware>
> for non-ai alternative softwares.

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

## Building the Compiler

**Prerequisites:**

- [Rust toolchain](https://rust-lang.org/)
- [Clang](https://clang.llvm.org/)
- [CMake](https://cmake.org/)
- [BDWGC](https://hboehm.info/gc/)
- [libdispatch](https://github.com/swiftlang/swift-corelibs-libdispatch)
- [blocksruntime](https://github.com/swiftlang/swift-corelibs-blocksruntime)
- [Botan](https://botan.randombit.net/)

> [!Note]
> libdispatch and blocksruntime are only required for Linux. On MacOS, they are system-provided.
> On Linux, provide libdispatch through `pkg-config`, or set `CIEL_LIBDISPATCH_INCLUDE_DIR`
> and `CIEL_LIBDISPATCH_LIBRARY`. Explicit paths take precedence over `pkg-config`.

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

## VS Code Integration

### Installation

1. Install the VS Code extension:
 
```sh
cd editors/vscode-ciel/
npx @vscode/vsce package
code --install-extension vscode-ciel-0.1.0.vsix
```

2. Build `ciel-lsp`:

```sh
cargo build --release --bin ciel-lsp
```

3. Either put `ciel-lsp` in your PATH, or configure the extension
  by setting `ciel.languageServer.path` to the executable path.

### Development

```sh
cargo build --bin ciel-lsp
cd editors/vscode-ciel
npm install
npm run build        # generate Tree-sitter wasm and check the extension entry point
npm test             # run extension smoke tests
```

## Examples

- **[intranet_tunnel](examples/intranet_tunnel/)** — a multiplexed,
  authenticated TCP tunnel implementing both server and agent programs using
  actors, async I/O, wire protocol serialization, and HMAC authentication.
  See its [README](examples/intranet_tunnel/README.md) for build and run
  instructions.
- **[intranet_tunnel_go](examples/intranet_tunnel_go/)** — a Go reference
  implementation of the same tunnel protocol for comparison.
- **[benchmark](examples/benchmark/)** — benchmarks.

## Documentation

- **[design.md](design.md)** — the normative language specification
- **[async-await-detailed.md](async-await-detailed.md)** — detailed async/await notes
- **[proposal/](proposal/)** — active and completed design proposals
- **[AGENTS.md](AGENTS.md)** — repository guidelines for llm agents
