# Ciel Language Support

VSCode language support for Ciel source files.

## Features

- Registers `.ciel` files as the `ciel` language.
- Uses a Tree-sitter grammar for parsing Ciel source.
- Loads the generated `parsers/tree-sitter-ciel.wasm` parser through `web-tree-sitter`.
- Provides syntax coloring through VSCode semantic tokens.
- Adds `Ciel: Show Tree-sitter Syntax Tree` for inspecting the parser output.

## Development

Install dependencies, build the Tree-sitter parser, and launch the extension host:

```sh
npm install
npm run build
```

Then open this folder in VSCode and press F5.

The parser source lives in `tree-sitter-ciel/grammar.js`. The build script runs
`tree-sitter generate` and writes the wasm parser to
`parsers/tree-sitter-ciel.wasm`.

Run the parser smoke tests with:

```sh
npm test
```

This regenerates the parser and checks all positive Ciel fixtures under
`tests/cases`, plus the checked-in `.ciel` files under `std` and `examples`.

## Package and Install

Build the generated parser before packaging:

```sh
npm install
npm run build
```

Create a VSCode extension package:

```sh
npx @vscode/vsce package
```

Install the generated `.vsix` into VSCode:

```sh
code --install-extension vscode-ciel-0.1.0.vsix
```
