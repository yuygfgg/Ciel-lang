# Ciel Language Support

VSCode language support for Ciel source files.

## Features

- Registers `.ciel` files as the `ciel` language.
- Uses the shared Tree-sitter grammar for baseline semantic highlighting.
- Registers one semantic-token provider that returns Tree-sitter tokens first,
  then merges compiler-backed LSP semantic tokens as refinements.
- Uses Tree-sitter highlighting for Ciel code blocks in markdown editors.
- Starts `ciel-lsp` on macOS and Linux when available.
- Uses the Ciel language server for compiler-backed diagnostics, semantic
  token refinements, hover, go-to-definition, signature help, and inlay hints.
- Does not start the language server on Windows.
- Adds `Ciel: Restart Language Server` and `Ciel: Show Tree-sitter Syntax Tree`.

## Development

Install dependencies, generate the Tree-sitter wasm parser, check the extension
entry point, and launch the extension host:

```sh
npm install
npm run build
cargo build --bin ciel-lsp
```

Then open this folder in VS Code and press F5. In that layout, the extension
can find `../../target/debug/ciel-lsp` automatically.

Tree-sitter source lives in the repository's top-level `tree-sitter-ciel/`
directory. `npm run build` generates `parsers/tree-sitter-ciel.wasm` and copies
the shared highlight query into the extension runtime tree. Those generated
files are ignored. Parser/highlighting regression coverage is exercised from
Rust with `cargo test`.

On non-Windows hosts, the extension searches for the language server in:

- `server/ciel-lsp` inside the installed extension
- `../../target/debug/ciel-lsp` from the extension directory
- `../../target/release/ciel-lsp` from the extension directory
- `ciel-lsp` on `PATH`

Set `ciel.languageServer.path` to use a specific executable.

Run the extension smoke test with:

```sh
npm test
```

## Package and Install

Check the extension and build the language server before packaging:

```sh
cargo build --release --bin ciel-lsp
npm install
npm run build
```

The `.vsix` package does not currently bundle `ciel-lsp` by default. After
installing it, make sure `ciel-lsp` is on `PATH`, set
`ciel.languageServer.path`, or package a server binary at `server/ciel-lsp`.

Create a VSCode extension package:

```sh
npx @vscode/vsce package
```

Install the generated `.vsix` into VSCode:

```sh
code --install-extension vscode-ciel-0.1.0.vsix
```
