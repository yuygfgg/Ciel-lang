const assert = require('assert');
const fs = require('fs');
const Module = require('module');
const path = require('path');

const extensionRoot = path.resolve(__dirname, '..');
const parserWasmPath = path.join(extensionRoot, 'parsers', 'tree-sitter-ciel.wasm');
const highlightQueryPath = path.join(extensionRoot, 'tree-sitter-ciel', 'queries', 'highlights.scm');

const source = [
    'import /std/async as async;',
    'async void f() {',
    '    plain_call();',
    '    await async::block_on(task);',
    '    biased select { case value = async::recv(rx): value; };',
    '}',
    'type T = async::Task<i64>;',
    'type F = i64 fn(i64);',
    'impl async::poll(i64 x) { return x; }',
    'interface I = async::Readable + PlainReadable;',
    'void match_one(i64 x) { switch (x) { case Foo: break; case ns::Bar: break; } }',
    '',
].join('\n');

async function main() {
    const { parser, language, Query } = await createParser();
    const tree = parser.parse(source);
    assertNoParseProblems(tree.rootNode);

    assertHighlightQuery(language, Query, tree.rootNode);
    assertSemanticClassifier(tree.rootNode);

    console.log('Tree-sitter highlighting keeps contextual async identifiers non-keyword.');
}

async function createParser() {
    if (!fs.existsSync(parserWasmPath)) {
        throw new Error(`missing parser wasm at ${parserWasmPath}; run npm run build first`);
    }

    const treeSitter = require('web-tree-sitter');
    const Parser = treeSitter.Parser || treeSitter.default || treeSitter;
    const Language = treeSitter.Language || Parser.Language;
    const Query = treeSitter.Query || Parser.Query;

    if (!Parser || typeof Parser !== 'function') {
        throw new Error('web-tree-sitter did not expose a Parser constructor');
    }
    if (!Language || typeof Language.load !== 'function') {
        throw new Error('web-tree-sitter did not expose Language.load');
    }
    if (!Query || typeof Query !== 'function') {
        throw new Error('web-tree-sitter did not expose a Query constructor');
    }

    if (typeof Parser.init === 'function') {
        await Parser.init({
            locateFile: fileName => path.join(extensionRoot, 'node_modules', 'web-tree-sitter', fileName),
        });
    }

    const language = await Language.load(parserWasmPath);
    const parser = new Parser();
    if (typeof parser.setLanguage === 'function') {
        parser.setLanguage(language);
    } else {
        parser.language = language;
    }
    return { parser, language, Query };
}

function assertHighlightQuery(language, Query, rootNode) {
    const querySource = fs.readFileSync(highlightQueryPath, 'utf8');
    const query = new Query(language, querySource);
    const captures = query.captures(rootNode).map(capture => ({
        name: capture.name,
        text: capture.node.text,
        row: capture.node.startPosition.row,
        column: capture.node.startPosition.column,
    }));

    const asyncKeywordCaptures = captures.filter(capture =>
        capture.name === 'keyword' && capture.text === 'async'
    );
    assert.deepStrictEqual(
        asyncKeywordCaptures.map(formatPosition),
        ['1:0'],
        '`async` should only be a query keyword capture in the async function modifier',
    );

    for (const text of ['await', 'biased', 'select', 'fn']) {
        assert(
            captures.some(capture => capture.name === 'keyword' && capture.text === text),
            `expected query keyword capture for ${text}`,
        );
    }

    assertCapture(captures, 'function.call', 'plain_call');
    assertCapture(captures, 'function.call', 'block_on');
    assertCapture(captures, 'function.call', 'recv');
    assertCapture(captures, 'function', 'poll');
    assertCapture(captures, 'type', 'Task');
    assertCapture(captures, 'type', 'Readable');
    assertCapture(captures, 'type', 'PlainReadable');
    assertCapture(captures, 'constant', 'Foo');
    assertCapture(captures, 'constant', 'Bar');
}

function assertSemanticClassifier(rootNode) {
    const extension = loadExtensionForTest();
    const { classifyLeaf, TOKEN_TYPES } = extension._test;
    const tokens = collectLeafTokens(rootNode, classifyLeaf, TOKEN_TYPES);
    const asyncTokens = tokens.filter(token => token.text === 'async');

    assert(asyncTokens.length > 1, 'expected all async spellings to receive semantic tokens');
    assert.deepStrictEqual(
        asyncTokens.filter(token => token.type === 'keyword').map(formatPosition),
        ['1:0'],
        '`async` should only be a semantic keyword in the async function modifier',
    );
    assert(
        asyncTokens.filter(token => token.type !== 'keyword').every(token => token.type === 'namespace'),
        '`async` module path, import alias, and qualified-name prefixes should be namespaces',
    );

    assertToken(tokens, 'function', 'plain_call');
    assertToken(tokens, 'function', 'block_on');
    assertToken(tokens, 'function', 'recv');
    assertToken(tokens, 'function', 'poll');
    assertToken(tokens, 'type', 'Task');
    assertToken(tokens, 'interface', 'Readable');
    assertToken(tokens, 'interface', 'PlainReadable');
    assertToken(tokens, 'enumMember', 'Foo');
    assertToken(tokens, 'enumMember', 'Bar');
}

function assertCapture(captures, name, text) {
    assert(
        captures.some(capture => capture.name === name && capture.text === text),
        `expected query ${name} capture for ${text}`,
    );
}

function assertToken(tokens, type, text) {
    assert(
        tokens.some(token => token.type === type && token.text === text),
        `expected semantic ${type} token for ${text}`,
    );
}

function loadExtensionForTest() {
    const originalLoad = Module._load;
    Module._load = function load(request, parent, isMain) {
        if (request === 'vscode') {
            return {
                SemanticTokensLegend: class SemanticTokensLegend { },
                SemanticTokensBuilder: class SemanticTokensBuilder { },
                languages: {
                    registerDocumentSemanticTokensProvider() {
                        return { dispose() { } };
                    },
                },
                commands: {
                    registerCommand() {
                        return { dispose() { } };
                    },
                },
                window: {
                    showInformationMessage() { },
                    showWarningMessage() { },
                },
                workspace: {
                    async openTextDocument() {
                        return {};
                    },
                },
            };
        }
        return originalLoad.call(this, request, parent, isMain);
    };

    try {
        return require(path.join(extensionRoot, 'src', 'extension.js'));
    } finally {
        Module._load = originalLoad;
    }
}

function collectLeafTokens(rootNode, classifyLeaf, tokenTypes) {
    const tokens = [];
    visitLeaves(rootNode, node => {
        const token = classifyLeaf(node);
        if (!token) {
            return;
        }
        tokens.push({
            text: node.text,
            row: node.startPosition.row,
            column: node.startPosition.column,
            type: tokenTypes[token.type],
        });
    });
    return tokens;
}

function visitLeaves(node, onLeaf) {
    const childCount = typeof node.childCount === 'number' ? node.childCount : 0;
    if (childCount === 0) {
        onLeaf(node);
        return;
    }

    for (let index = 0; index < childCount; index += 1) {
        const child = typeof node.child === 'function' ? node.child(index) : undefined;
        if (child) {
            visitLeaves(child, onLeaf);
        }
    }
}

function assertNoParseProblems(rootNode) {
    const problems = [];
    collectParseProblems(rootNode, problems);
    if (problems.length > 0) {
        throw new Error(`highlighting test source did not parse cleanly:\n${problems.join('\n')}`);
    }
}

function collectParseProblems(node, problems) {
    if (node.type === 'ERROR' || isMissing(node)) {
        problems.push(`${node.type} at ${formatPosition(node)}`);
    }

    const childCount = typeof node.childCount === 'number' ? node.childCount : 0;
    for (let index = 0; index < childCount; index += 1) {
        const child = typeof node.child === 'function' ? node.child(index) : undefined;
        if (child) {
            collectParseProblems(child, problems);
        }
    }
}

function isMissing(node) {
    if (typeof node.isMissing === 'function') {
        return node.isMissing();
    }
    return Boolean(node.isMissing);
}

function formatPosition(nodeOrCapture) {
    return `${nodeOrCapture.row}:${nodeOrCapture.column}`;
}

main().catch(error => {
    console.error(error && error.stack || error);
    process.exit(1);
});
