# Repository Guidelines

## Project Structure & Module Organization
`src/` contains the Rust compiler frontend, type checker, build planner, and code generator. `runtime/` holds the packaged C runtime and public headers used by generated programs. `std/` contains standard-library packages, each with a `ciel.toml` manifest and, when needed, native C/CMake support. `tests/cases/` stores `.ciel` fixture programs grouped by feature area, while [`tests/ciel_cases.rs`](/Users/a1/Ciel-lang/tests/ciel_cases.rs) drives compile/run/error expectations. Use `examples/` for end-to-end demos, `libs/` for bundled package dependencies, `editors/` for tooling integrations, and `proposal/` for design notes.

## Build, Test, and Development Commands
Run `cargo build` to build `cielc`. Use `cargo run -- path/to/file.ciel -o /tmp/app` for quick compiler smoke tests, or add `--manifest-path examples/intranet_tunnel/ciel.toml --std-path "$PWD"` for project-style builds. `cargo test` runs the Rust unit tests plus generated fixture coverage. `cargo test -q --test ciel_cases` is the fastest way to validate language behavior changes. For the demo integration suite, run `sh examples/intranet_tunnel/test/test.sh`.

## Coding Style & Naming Conventions
Rust uses standard `rustfmt` formatting with 4-space indentation and snake_case item names; keep modules focused and prefer explicit types at API boundaries. Format Rust with `cargo fmt`. Native C and headers should follow the repository `.clang-format` file (`BasedOnStyle: LLVM`, 4-space indent, left-aligned pointers); run `clang-format -i` on edited C sources. Name new fixture directories descriptively, for example `tests/cases/types/array_repeat_literals_fill_arrays_and_slices/`.

## Testing Guidelines
Each `.ciel` fixture starts with metadata comments such as `// ciel-test: run`, `// expect-stdout: ...`, or `// sanitizer: address`. Keep one behavior per fixture directory and use stable, descriptive names matching the scenario under test. Add compile, runtime, and emitted-C assertions when changing lowering or diagnostics. Update `tests/KNOWN_FAILURES.md` only when a failure is intentional and tracked.

## Commit & Pull Request Guidelines
Recent history favors short, imperative or noun-phrase subjects like `Implement Vec type` or `Refactor & Cleanup`. Keep commit titles brief and specific. Pull requests should describe the language/runtime behavior changed, list the commands you ran, and link the relevant issue or proposal when semantics shift. Include screenshots only for `editors/vscode-ciel` UI changes.
