const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const extensionRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(extensionRoot, '..', '..');
const grammarDir = path.join(repoRoot, 'tree-sitter-ciel');
const grammarPath = path.join(grammarDir, 'grammar.js');
const parserDir = path.join(extensionRoot, 'parsers');
const runtimeQueryDir = path.join(extensionRoot, 'src', 'tree_sitter');
const wasmPath = path.join(parserDir, 'tree-sitter-ciel.wasm');
const querySourcePath = path.join(grammarDir, 'highlights.scm');
const queryRuntimePath = path.join(runtimeQueryDir, 'highlights.scm');
const treeSitterBin = path.join(
    extensionRoot,
    'node_modules',
    '.bin',
    process.platform === 'win32' ? 'tree-sitter.cmd' : 'tree-sitter',
);

function run(command, args, options = {}) {
    const result = spawnSync(command, args, {
        cwd: options.cwd || repoRoot,
        stdio: 'inherit',
        shell: false,
    });
    if (result.error) {
        throw result.error;
    }
    if (result.status !== 0) {
        throw new Error(`${path.basename(command)} ${args.join(' ')} failed`);
    }
}

function main() {
    if (!fs.existsSync(grammarPath)) {
        throw new Error(`missing Ciel Tree-sitter grammar at ${grammarDir}`);
    }
    if (!fs.existsSync(treeSitterBin)) {
        throw new Error(
            'tree-sitter CLI is missing. Run `npm install` in editors/vscode-ciel first.',
        );
    }

    fs.mkdirSync(parserDir, { recursive: true });
    fs.mkdirSync(runtimeQueryDir, { recursive: true });
    run(treeSitterBin, ['generate', 'grammar.js'], { cwd: grammarDir });
    run(treeSitterBin, ['build', '--wasm', '--output', wasmPath, '.'], { cwd: grammarDir });
    fs.copyFileSync(querySourcePath, queryRuntimePath);
}

main();
