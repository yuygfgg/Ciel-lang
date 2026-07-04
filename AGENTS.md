# Repository Guidelines

## Agent Operating Rules
Unless the user explicitly asks otherwise, converse with the user in Simplified
Chinese. Keep code, comments, documentation, fixture names, commit messages,
and PR text in English by default.

When sandbox-external execution is required, request permission directly instead
of working around the sandbox. Prefer `ast-grep` for structural code searches or
edits when it is a better fit than text search.

Do not overwrite user changes. If the working tree is dirty, inspect the
relevant files and keep edits scoped to the task.

## Project Structure
`src/` contains the Rust compiler and CLI:

- `lexer.rs`, `parser.rs`, `ast.rs`, and `source.rs` handle source text,
  tokens, parsing, and spans.
- `resolve.rs`, `hir.rs`, `checked.rs`, `types.rs`, and `typeck/` implement
  name resolution, type checking, capability checks, and semantic validation.
- `thir/`, `mono.rs`, and `codegen/` lower checked programs into generated C.
- `build/`, `driver.rs`, and `main.rs` implement package manifests, native
  build planning, CLI options, and compiler entry points.

`runtime/` contains the packaged C runtime, public headers, and CMake target
linked into generated programs. `std/` contains standard-library packages; each
package has a `ciel.toml` manifest and may include native C/CMake support.
`libs/` contains bundled third-party package dependencies. `tests/cases/`
contains `.ciel` fixture programs, and `tests/ciel_cases.rs` defines the
fixture runner. `examples/` contains end-to-end programs, especially
`examples/intranet_tunnel`. `tree-sitter-ciel/` contains the shared
Tree-sitter grammar and highlight query. `editors/vscode-ciel/` contains the
VS Code Tree-sitter and LSP adapter.
`proposal/` contains design proposals; `design.md` is the normative language
specification.

## Build And Smoke Commands
Use these commands from the repository root unless noted:

```sh
cargo build
cargo run -- path/to/file.ciel -o /tmp/app
cargo run -- --emit-c path/to/file.ciel -o /tmp/app.c
cargo run -- --manifest-path examples/intranet_tunnel/ciel.toml --std-path "$PWD" --entry frame_test -o /tmp/frame_test
```

The compiler invokes the system C toolchain for executable/shared/object output.
Native package builds require CMake. For project-style builds, pass
`--manifest-path`, `--entry`, and `--std-path "$PWD"` when the project should
use this checkout's standard library. Third-party native package targets require
`--allow-native-build`; do not add that flag unless the test or task is meant to
exercise native build policy.

## Test Commands
Run the smallest relevant test first, then broaden coverage when the change
touches shared compiler behavior.

```sh
cargo fmt
cargo test -q --test ciel_cases
cargo test
sh examples/intranet_tunnel/test/test.sh
```

`cargo test -q --test ciel_cases` is the fastest language-behavior regression
suite. `cargo test` also runs Rust unit tests and CLI/build-plan tests. The
intranet tunnel integration script builds all project test entries, then runs a
Python loopback integration test unless `CIEL_TUNNEL_SKIP_LOOPBACK=1` is set.
If a custom compiler binary should be used by that script, set `CIELC`.

Run sanitizer fixture variants only through fixture metadata. Do not hand-roll
parallel sanitizer scripts unless the fixture runner cannot express the case.

## Fixture Guidelines
Every `.ciel` fixture under `tests/cases/` must start with metadata comments.
Keep one behavior per fixture directory and name directories descriptively, for
example `tests/cases/types/array_repeat_literals_fill_arrays_and_slices/`.

Supported `ciel-test` kinds:

- `compile`: compile to a build plan and optionally assert generated C.
- `run`: compile, link, execute, and check `expect-exit`.
- `error`: require compilation failure and at least one `expect-error`.
- `host`: generate C, include it from a host C file, link, execute, and check
  `expect-exit`.
- `dependency`: helper source imported by another fixture; no expectations.
- `manual`: fixture kept out of generated tests; no expectations.
- `known-fail-compile`, `known-fail-cc`, `known-fail-run`,
  `known-fail-accepts`: tracked regressions that require
  `known-fail-reason` and must be promoted when they start passing.

Common metadata:

- `expect-exit: N`
- `expect-stdout: text`
- `expect-stderr-contains: text`
- `expect-error: diagnostic substring`
- `expect-c-contains: generated C substring`
- `expect-c-not-contains: generated C substring`
- `expect-c-count: substring => N`
- `run-arg: value` or `run-arg: @tmp/name`
- `feature: name`
- `package-root: relative/path` or `package-root: @repo/path`
- `allow-native-build: true`
- `sanitizer: address` or `sanitizer: thread`
- `warning-clean: true`
- `host: host_fixture.c`

Use `expect-c-*` assertions for lowering and ABI changes, `expect-error` for
diagnostic behavior, and runtime assertions for observable semantics. Update
`tests/KNOWN_FAILURES.md` only when a failure is intentional and tracked outside
the self-describing `known-fail-*` fixture metadata.

## Standard Library, Runtime, And Native Code
Standard-library packages live in `std/<package>/` and are described by
`std/<package>/ciel.toml`. Keep package exports, native requirements, and import
paths consistent when moving files. If a std package needs native code, declare
the CMake target in its manifest and keep the target scoped to that package.

Runtime C sources live in `runtime/src/`; public headers live in
`runtime/include/`. Format Rust with `cargo fmt`. Format C and headers with the
repository `.clang-format` (`BasedOnStyle: LLVM`, 4-space indent,
left-aligned pointers), for example:

```sh
clang-format -i runtime/src/file.c runtime/include/file.h
```

## VS Code Extension And Highlighting
The extension lives in `editors/vscode-ciel/`. Shared Tree-sitter source lives
in top-level `tree-sitter-ciel/`; Rust generates and compiles its native parser
from that directory in `build.rs`, while the VS Code build generates its own
ignored wasm parser under `editors/vscode-ciel/parsers/` and copies the shared
highlight query to `editors/vscode-ciel/src/tree_sitter/` for packaging.

Use these commands from `editors/vscode-ciel/`:

```sh
npm install
npm run build
npm test
```

`npm run build` and `npm test` generate the VS Code Tree-sitter wasm and check
the extension entry point. Parser and highlighting regression coverage is run
from Rust with `cargo test`.

When language syntax changes, update `tree-sitter-ciel/grammar.js` and
`tree-sitter-ciel/highlights.scm`, extend Rust Tree-sitter tests as needed, and
run `cargo test` plus `npm test`. Do not commit generated parser files or wasm.
For manual VS Code validation, open
`editors/vscode-ciel/` in VS Code, press F5 to launch the extension host, and
open a `.ciel` file or a markdown document with a fenced `ciel` code block.
Only include screenshots for actual editor UI changes.

Package the extension after checking it:

```sh
npm run build
npx @vscode/vsce package
code --install-extension vscode-ciel-0.1.0.vsix
```

## Proposal Workflow
`design.md` is the source of truth for accepted language behavior. Active or
draft proposals live directly under `proposal/`. Accepted and completed
proposals live under `proposal/done/`.

New proposals should explain the problem, goals, non-goals, syntax, semantics,
type-checking impact, lowering/runtime impact, diagnostics, testing strategy,
and open questions. If ordering matters, add a `Proposal Order` section using
the notation documented in `proposal/README.md`.

When a proposal is implemented:

- Merge its normative rules into `design.md` in the appropriate section.
- Add or update compiler, runtime, stdlib, fixture, and editor tests as needed.
- Move the proposal file from `proposal/` to `proposal/done/`.
- Leave only genuinely postponed follow-up work in an active proposal file,
  with a clear title and scope.

Do not leave implemented language semantics documented only in `proposal/`.
If the implementation intentionally diverges from the proposal, update both the
proposal history and `design.md` so future work follows the implemented design.

## Coding Style
Rust uses standard `rustfmt` formatting, 4-space indentation, snake_case item
names, and explicit types at public API boundaries. Keep modules focused and
prefer existing compiler data structures over new parallel abstractions.

Ciel fixture code should be small and direct. Prefer stable output and exact
diagnostic substrings over broad assertions. Generated C assertions should be
specific enough to catch the intended lowering change without depending on
irrelevant temporary names.

Native C and headers follow `.clang-format`. Keep unsafe C ABI assumptions
visible in names, comments, or tests when the compiler cannot prove them.

## Commits And Pull Requests
Use short, specific commit subjects such as `Implement Vec type` or
`Refactor build planner`. Pull requests should describe the language, runtime,
stdlib, or editor behavior changed; list the commands run; and link the
relevant issue or proposal when semantics shift.

Before handing off a change, check `git status --short` and call out any tests
you could not run. Do not include unrelated formatting churn or generated-file
updates unless they are required by the change.
