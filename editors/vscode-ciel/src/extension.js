const fs = require('fs');
const path = require('path');
const vscode = require('vscode');
const MarkdownIt = require('markdown-it');

let languageClient;
let languageClientStartPromise;
let semanticTokensProvider;
let warnedMissingLanguageClient = false;
let warnedMissingLanguageServer = false;
const markdownParser = new MarkdownIt();

const TOKEN_TYPES = [
    'namespace',
    'type',
    'struct',
    'enum',
    'interface',
    'typeParameter',
    'parameter',
    'variable',
    'property',
    'enumMember',
    'function',
    'keyword',
    'comment',
    'string',
    'number',
    'operator',
];

const TOKEN_MODIFIERS = [
    'declaration',
    'definition',
    'readonly',
    'async',
    'defaultLibrary',
    'modification',
    'mutable',
];

const TOKEN_TYPE_INDEX = new Map(TOKEN_TYPES.map((type, index) => [type, index]));
const TOKEN_MODIFIER_INDEX = new Map(TOKEN_MODIFIERS.map((modifier, index) => [modifier, index]));
const treeSitterLegend = new vscode.SemanticTokensLegend(TOKEN_TYPES, TOKEN_MODIFIERS);

const CAPTURE_TOKENS = new Map([
    ['keyword', token('keyword')],
    ['type.builtin', token('type', ['defaultLibrary'])],
    ['boolean', token('keyword')],
    ['constant.builtin', token('variable', ['readonly', 'defaultLibrary'])],
    ['number', token('number')],
    ['number.float', token('number')],
    ['string', token('string')],
    ['string.special', token('string')],
    ['comment', token('comment')],
    ['function', token('function', ['declaration'])],
    ['function.call', token('function')],
    ['type', token('type')],
    ['type.definition', token('type', ['declaration'])],
    ['type.parameter', token('typeParameter')],
    ['property.definition', token('property', ['declaration'])],
    ['property', token('property')],
    ['constant', token('enumMember')],
    ['variable.parameter', token('parameter')],
    ['variable', token('variable')],
    ['namespace', token('namespace')],
    ['operator', token('operator')],
]);

function activate(context) {
    const treeSitterProvider = new CielTreeSitterSemanticTokensProvider(context);
    semanticTokensProvider = treeSitterProvider;
    context.subscriptions.push(
        vscode.languages.registerDocumentSemanticTokensProvider(
            [
                { language: 'ciel' },
                { language: 'markdown' },
            ],
            treeSitterProvider,
            treeSitterLegend,
        ),
        treeSitterProvider,
    );
    treeSitterProvider.prewarm();

    context.subscriptions.push({ dispose: () => stopLanguageServer() });

    context.subscriptions.push(
        vscode.commands.registerCommand('ciel.restartLanguageServer', async () => {
            await stopLanguageServer();
            await startLanguageServer(context, { showMissingWarning: true });
        }),
        vscode.commands.registerCommand('ciel.showSyntaxTree', async () => {
            const editor = vscode.window.activeTextEditor;
            if (!editor || editor.document.languageId !== 'ciel') {
                vscode.window.showInformationMessage('Open a Ciel source file first.');
                return;
            }

            const parser = await treeSitterProvider.getParser();
            if (!parser) {
                return;
            }

            const tree = parser.parse(editor.document.getText());
            const document = await vscode.workspace.openTextDocument({
                language: 'scheme',
                content: formatTree(tree.rootNode),
            });
            await vscode.window.showTextDocument(document, { preview: true });
        }),
    );

    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument(document => {
            if (document.languageId === 'ciel') {
                startLanguageServer(context, { showMissingWarning: false });
            }
        }),
        vscode.window.onDidChangeActiveTextEditor(editor => {
            if (editor && editor.document.languageId === 'ciel') {
                startLanguageServer(context, { showMissingWarning: false });
            }
        }),
    );

    if (vscode.workspace.textDocuments.some(document => document.languageId === 'ciel')) {
        startLanguageServer(context, { showMissingWarning: false });
    }
}

async function deactivate() {
    await stopLanguageServer();
}

async function startLanguageServer(context, options = {}) {
    if (languageClient || languageClientStartPromise) {
        return languageClientStartPromise;
    }

    languageClientStartPromise = doStartLanguageServer(context, options).finally(() => {
        languageClientStartPromise = undefined;
    });
    return languageClientStartPromise;
}

async function doStartLanguageServer(context, options = {}) {
    if (process.platform === 'win32') {
        if (options.showMissingWarning) {
            vscode.window.showInformationMessage(
                'Ciel language server is not supported on Windows.',
            );
        }
        return;
    }

    const config = vscode.workspace.getConfiguration('ciel.languageServer');
    if (!config.get('enabled', true)) {
        return;
    }

    const serverCommand = resolveLanguageServerCommand(context);
    if (!serverCommand) {
        if (options.showMissingWarning || !warnedMissingLanguageServer) {
            warnedMissingLanguageServer = true;
            vscode.window.showWarningMessage(
                'Ciel language server was not found. Build it with `cargo build --bin ciel-lsp` or set `ciel.languageServer.path`.',
            );
        }
        return;
    }

    let languageClientModule;
    try {
        languageClientModule = require('vscode-languageclient/node');
    } catch (error) {
        if (!warnedMissingLanguageClient) {
            warnedMissingLanguageClient = true;
            vscode.window.showWarningMessage(
                `Ciel language client dependency is missing: ${error.message}`,
            );
        }
        return;
    }

    const CielLanguageClient = languageClientWithoutAutomaticSemanticTokenProvider(languageClientModule);
    const workspaceFolder = firstWorkspaceFolder();
    const serverOptions = {
        command: serverCommand.command,
        args: serverCommand.args,
        options: {
            cwd: workspaceFolder ? workspaceFolder.uri.fsPath : context.extensionPath,
            env: process.env,
        },
    };
    const clientOptions = {
        documentSelector: [{ scheme: 'file', language: 'ciel' }],
        synchronize: {
            configurationSection: 'ciel.languageServer',
        },
    };

    languageClient = new CielLanguageClient(
        'ciel-lsp',
        'Ciel Language Server',
        serverOptions,
        clientOptions,
    );
    try {
        await languageClient.start();
        if (semanticTokensProvider) {
            semanticTokensProvider.setLanguageClient(languageClient);
        }
    } catch (error) {
        languageClient = undefined;
        if (semanticTokensProvider) {
            semanticTokensProvider.setLanguageClient(undefined);
        }
        vscode.window.showWarningMessage(`Ciel language server failed to start: ${error.message}`);
    }
}

async function stopLanguageServer() {
    if (languageClientStartPromise) {
        await languageClientStartPromise;
    }
    if (!languageClient) {
        return;
    }
    const client = languageClient;
    languageClient = undefined;
    if (semanticTokensProvider) {
        semanticTokensProvider.setLanguageClient(undefined);
    }
    await client.stop();
}

function languageClientWithoutAutomaticSemanticTokenProvider(languageClientModule) {
    const { LanguageClient } = languageClientModule;
    const semanticTokensMethod =
        languageClientModule.SemanticTokensRegistrationType &&
            languageClientModule.SemanticTokensRegistrationType.method
            ? languageClientModule.SemanticTokensRegistrationType.method
            : 'textDocument/semanticTokens';

    return class CielLanguageClient extends LanguageClient {
        registerBuiltinFeatures() {
            super.registerBuiltinFeatures();
            disableLanguageClientSemanticTokenProvider(this, semanticTokensMethod);
        }
    };
}

function disableLanguageClientSemanticTokenProvider(client, method) {
    const feature = typeof client.getFeature === 'function' ? client.getFeature(method) : undefined;
    if (!feature) {
        return;
    }

    // Keep semantic-token client capabilities, but let the extension's merged
    // provider own VS Code registration so Tree-sitter tokens can render first.
    feature.registerLanguageProvider = () => [
        new vscode.Disposable(() => { }),
        {
            onDidChangeSemanticTokensEmitter: {
                fire: () => {
                    if (semanticTokensProvider) {
                        semanticTokensProvider.refreshFromLsp();
                    }
                },
            },
        },
    ];
    const originalRegister = feature.register;
    feature.register = data => {
        if (!data || !data.registerOptions || !data.registerOptions.documentSelector) {
            return;
        }
        originalRegister.call(feature, data);
    };
}

function resolveLanguageServerCommand(context) {
    if (process.platform === 'win32') {
        return undefined;
    }

    const config = vscode.workspace.getConfiguration('ciel.languageServer');
    const configuredPath = config.get('path', '').trim();
    if (configuredPath) {
        return { command: configuredPath, args: [] };
    }

    const binaryName = 'ciel-lsp';
    const candidates = [
        context.asAbsolutePath(path.join('server', binaryName)),
        path.resolve(context.extensionPath, '..', '..', 'target', 'debug', binaryName),
        path.resolve(context.extensionPath, '..', '..', 'target', 'release', binaryName),
    ];
    for (const candidate of candidates) {
        if (isExecutableFile(candidate)) {
            return { command: candidate, args: [] };
        }
    }

    const pathCommand = findExecutableOnPath(binaryName);
    if (pathCommand) {
        return { command: pathCommand, args: [] };
    }
    return undefined;
}

function firstWorkspaceFolder() {
    const folders = vscode.workspace.workspaceFolders;
    return folders && folders.length > 0 ? folders[0] : undefined;
}

function isExecutableFile(filePath) {
    try {
        fs.accessSync(filePath, fs.constants.X_OK);
        return true;
    } catch (_) {
        return false;
    }
}

function findExecutableOnPath(binaryName) {
    const pathValue = process.env.PATH || '';
    for (const entry of pathValue.split(path.delimiter)) {
        if (!entry) {
            continue;
        }
        const candidate = path.join(entry, binaryName);
        if (isExecutableFile(candidate)) {
            return candidate;
        }
    }
    return undefined;
}

class CielTreeSitterSemanticTokensProvider {
    constructor(context) {
        this.context = context;
        this.parserBundlePromise = undefined;
        this.parserBundle = undefined;
        this.languageClient = undefined;
        this.lspTokenCache = new Map();
        this.lspTokenRequests = new Map();
        this.onDidChangeSemanticTokensEmitter = new vscode.EventEmitter();
        this.onDidChangeSemanticTokens = this.onDidChangeSemanticTokensEmitter.event;
        this.warnedMissingParser = false;
        this.warnedMissingQuery = false;
        this.warnedParserError = false;
    }

    dispose() {
        this.onDidChangeSemanticTokensEmitter.dispose();
        if (this.parserBundlePromise) {
            this.parserBundlePromise.then(bundle => {
                if (bundle && bundle.parser && typeof bundle.parser.delete === 'function') {
                    bundle.parser.delete();
                }
            }, () => { });
        }
    }

    setLanguageClient(client) {
        this.languageClient = client;
        this.lspTokenCache.clear();
        this.lspTokenRequests.clear();
        this.onDidChangeSemanticTokensEmitter.fire();
    }

    refreshFromLsp() {
        this.lspTokenCache.clear();
        this.lspTokenRequests.clear();
        this.onDidChangeSemanticTokensEmitter.fire();
    }

    prewarm() {
        this.getParserBundle().then(bundle => {
            if (bundle) {
                this.onDidChangeSemanticTokensEmitter.fire();
            }
        }, () => { });
    }

    async provideDocumentSemanticTokens(document, cancellationToken) {
        if (document.languageId === 'markdown') {
            return this.provideMarkdownSemanticTokens(document, cancellationToken);
        }
        return this.provideCielSemanticTokens(document, cancellationToken);
    }

    async provideCielSemanticTokens(document, cancellationToken) {
        const bundle = this.parserBundle;
        if (!bundle || cancellationToken.isCancellationRequested) {
            this.prewarm();
            const cachedLspTokens = this.cachedLspTokens(document);
            this.requestLspTokens(document);
            return semanticTokensFromAbsolute(cachedLspTokens);
        }

        const tokens = [];
        pushTreeSitterSemanticTokens(
            tokens,
            bundle,
            document.getText(),
            cancellationToken,
            (node, semanticToken) => {
                pushDocumentNodeToken(
                    tokens,
                    document,
                    node,
                    semanticToken.type,
                    semanticToken.modifiers,
                );
            },
        );

        const cachedLspTokens = this.cachedLspTokens(document);
        this.requestLspTokens(document);
        return semanticTokensFromAbsolute(mergeTokens(tokens, cachedLspTokens));
    }

    async provideMarkdownSemanticTokens(document, cancellationToken) {
        const bundle = this.parserBundle;
        if (!bundle || cancellationToken.isCancellationRequested) {
            this.prewarm();
            return semanticTokensFromAbsolute([]);
        }

        const tokens = [];
        for (const codeBlock of cielMarkdownCodeBlocks(document)) {
            if (cancellationToken.isCancellationRequested) {
                break;
            }
            pushTreeSitterSemanticTokens(
                tokens,
                bundle,
                codeBlock.content,
                cancellationToken,
                (node, semanticToken) => {
                    pushMarkdownNodeToken(
                        tokens,
                        codeBlock,
                        node,
                        semanticToken.type,
                        semanticToken.modifiers,
                    );
                },
            );
        }
        return semanticTokensFromAbsolute(tokens);
    }

    async getParser() {
        const bundle = await this.getParserBundle();
        return bundle ? bundle.parser : undefined;
    }

    async getParserBundle() {
        if (this.parserBundle) {
            return this.parserBundle;
        }
        if (!this.parserBundlePromise) {
            this.parserBundlePromise = this.createParserBundle().then(bundle => {
                this.parserBundle = bundle;
                return bundle;
            });
        }
        return this.parserBundlePromise;
    }

    cachedLspTokens(document) {
        const cached = this.lspTokenCache.get(document.uri.toString());
        return cached && cached.version === document.version ? cached.tokens : [];
    }

    requestLspTokens(document) {
        const client = this.languageClient;
        if (!client || typeof client.sendRequest !== 'function') {
            return;
        }

        const uri = document.uri.toString();
        const version = document.version;
        const cached = this.lspTokenCache.get(uri);
        if (cached && cached.version === version) {
            return;
        }

        const requestKey = `${uri}:${version}`;
        if (this.lspTokenRequests.get(uri) === requestKey) {
            return;
        }
        this.lspTokenRequests.set(uri, requestKey);

        client.sendRequest('textDocument/semanticTokens/full', {
            textDocument: { uri },
        }).then(result => {
            if (this.lspTokenRequests.get(uri) !== requestKey) {
                return;
            }
            this.lspTokenRequests.delete(uri);
            if (!result || !Array.isArray(result.data)) {
                return;
            }
            this.lspTokenCache.set(uri, {
                version,
                tokens: decodeSemanticTokens(result.data),
            });
            this.onDidChangeSemanticTokensEmitter.fire();
        }, () => {
            if (this.lspTokenRequests.get(uri) === requestKey) {
                this.lspTokenRequests.delete(uri);
            }
        });
    }

    async createParserBundle() {
        const parserWasmPath = this.context.asAbsolutePath(
            path.join('parsers', 'tree-sitter-ciel.wasm'),
        );
        const queryPath = this.context.asAbsolutePath(
            path.join('src', 'tree_sitter', 'highlights.scm'),
        );
        if (!fs.existsSync(parserWasmPath)) {
            this.warnMissingParser(parserWasmPath);
            return undefined;
        }
        if (!fs.existsSync(queryPath)) {
            this.warnMissingQuery(queryPath);
            return undefined;
        }

        try {
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
                    locateFile: fileName => this.context.asAbsolutePath(
                        path.join('node_modules', 'web-tree-sitter', fileName),
                    ),
                });
            }

            const language = await Language.load(parserWasmPath);
            const parser = new Parser();
            if (typeof parser.setLanguage === 'function') {
                parser.setLanguage(language);
            } else {
                parser.language = language;
            }
            const query = new Query(language, fs.readFileSync(queryPath, 'utf8'));
            return { parser, query };
        } catch (error) {
            if (!this.warnedParserError) {
                this.warnedParserError = true;
                vscode.window.showWarningMessage(`Ciel Tree-sitter parser failed to load: ${error.message}`);
            }
            return undefined;
        }
    }

    warnMissingParser(parserWasmPath) {
        if (this.warnedMissingParser) {
            return;
        }
        this.warnedMissingParser = true;
        vscode.window.showWarningMessage(
            `Ciel Tree-sitter parser wasm is missing at ${parserWasmPath}. Run npm install and npm run build in editors/vscode-ciel.`,
        );
    }

    warnMissingQuery(queryPath) {
        if (this.warnedMissingQuery) {
            return;
        }
        this.warnedMissingQuery = true;
        vscode.window.showWarningMessage(
            `Ciel Tree-sitter highlight query is missing at ${queryPath}. Run npm run build in editors/vscode-ciel.`,
        );
    }
}

function token(type, modifiers = []) {
    const typeIndex = TOKEN_TYPE_INDEX.get(type);
    if (typeIndex === undefined) {
        return undefined;
    }

    let modifierBits = 0;
    for (const modifier of modifiers) {
        const index = TOKEN_MODIFIER_INDEX.get(modifier);
        if (index !== undefined) {
            modifierBits |= 1 << index;
        }
    }

    return { type: typeIndex, modifiers: modifierBits };
}

function compareCaptures(left, right) {
    return left.node.startIndex - right.node.startIndex ||
        left.node.endIndex - right.node.endIndex ||
        left.name.localeCompare(right.name);
}

function pushTreeSitterSemanticTokens(tokens, bundle, sourceText, cancellationToken, pushNodeToken) {
    let tree;
    try {
        tree = bundle.parser.parse(sourceText);
        const captures = bundle.query.captures(tree.rootNode).sort(compareCaptures);
        const pushed = new Set();
        for (const capture of captures) {
            if (cancellationToken.isCancellationRequested) {
                break;
            }

            const semanticToken = CAPTURE_TOKENS.get(capture.name);
            if (!semanticToken) {
                continue;
            }

            const node = capture.node;
            const key = `${node.startIndex}:${node.endIndex}:${semanticToken.type}:${semanticToken.modifiers}`;
            if (pushed.has(key)) {
                continue;
            }
            pushed.add(key);
            pushNodeToken(node, semanticToken);
        }
    } finally {
        if (tree && typeof tree.delete === 'function') {
            tree.delete();
        }
    }
}

function pushDocumentNodeToken(tokens, document, node, tokenType, tokenModifiers) {
    const start = node.startPosition;
    const end = node.endPosition;
    for (let line = start.row; line <= end.row && line < document.lineCount; line += 1) {
        const lineText = document.lineAt(line).text;
        const startByteColumn = line === start.row ? start.column : 0;
        const endByteColumn = line === end.row ? end.column : Buffer.byteLength(lineText, 'utf8');
        const startCharacter = utf8ByteColumnToUtf16(lineText, startByteColumn);
        const endCharacter = utf8ByteColumnToUtf16(lineText, endByteColumn);
        const length = endCharacter - startCharacter;
        if (length > 0) {
            tokens.push({
                line,
                character: startCharacter,
                length,
                tokenType,
                tokenModifiers,
            });
        }
    }
}

function pushMarkdownNodeToken(tokens, codeBlock, node, tokenType, tokenModifiers) {
    const start = node.startPosition;
    const end = node.endPosition;
    for (let line = start.row; line <= end.row && line < codeBlock.lines.length; line += 1) {
        const lineText = codeBlock.lines[line];
        const startByteColumn = line === start.row ? start.column : 0;
        const endByteColumn = line === end.row ? end.column : Buffer.byteLength(lineText, 'utf8');
        const startCharacter = utf8ByteColumnToUtf16(lineText, startByteColumn);
        const endCharacter = utf8ByteColumnToUtf16(lineText, endByteColumn);
        const length = endCharacter - startCharacter;
        if (length > 0) {
            tokens.push({
                line: codeBlock.startLine + line,
                character: codeBlock.lineCharacterOffsets[line] + startCharacter,
                length,
                tokenType,
                tokenModifiers,
            });
        }
    }
}

function cielMarkdownCodeBlocks(document) {
    const blocks = [];
    const markdownTokens = markdownParser.parse(document.getText(), {});
    for (const token of markdownTokens) {
        if (token.type !== 'fence' || !Array.isArray(token.map) || !isCielFenceInfo(token.info)) {
            continue;
        }

        const startLine = token.map[0] + 1;
        const lines = splitCodeBlockLines(token.content);
        blocks.push({
            content: token.content,
            lines,
            startLine,
            lineCharacterOffsets: markdownCodeLineOffsets(document, token, startLine, lines),
        });
    }
    return blocks;
}

function splitCodeBlockLines(content) {
    return content.split(/\r?\n/);
}

function isCielFenceInfo(info) {
    if (typeof info !== 'string') {
        return false;
    }
    const language = info.trim().split(/\s+/, 1)[0].toLowerCase();
    return language === 'ciel';
}

function markdownCodeLineOffsets(document, token, startLine, codeLines) {
    const openingLineText = safeDocumentLineText(document, token.map[0]);
    const markupIndex = token.markup ? openingLineText.indexOf(token.markup) : -1;
    const containerPrefix = markupIndex >= 0 ? openingLineText.slice(0, markupIndex) : '';
    return codeLines.map((codeLine, index) => {
        const documentLine = startLine + index;
        const documentLineText = safeDocumentLineText(document, documentLine);
        if (containerPrefix && documentLineText.startsWith(containerPrefix)) {
            return containerPrefix.length;
        }

        const contentIndex = codeLine ? documentLineText.indexOf(codeLine) : -1;
        return contentIndex >= 0 ? contentIndex : 0;
    });
}

function safeDocumentLineText(document, line) {
    if (line < 0 || line >= document.lineCount) {
        return '';
    }
    return document.lineAt(line).text;
}

function semanticTokensFromAbsolute(tokens) {
    const builder = new vscode.SemanticTokensBuilder(treeSitterLegend);
    for (const token of uniqueNonOverlappingTokens(tokens)) {
        builder.push(
            token.line,
            token.character,
            token.length,
            token.tokenType,
            token.tokenModifiers,
        );
    }
    return builder.build();
}

function mergeTokens(treeTokens, lspTokens) {
    const semanticTokens = uniqueNonOverlappingTokens(lspTokens);
    if (semanticTokens.length === 0) {
        return treeTokens;
    }

    const semanticTokensByLine = tokensByLine(semanticTokens);
    const syntaxTokens = validTokens(treeTokens).filter(token =>
        !lineTokensOverlap(token, semanticTokensByLine.get(token.line)),
    );
    return syntaxTokens.concat(semanticTokens);
}

function decodeSemanticTokens(data) {
    const tokens = [];
    let line = 0;
    let character = 0;

    if (data.every(item => typeof item === 'number')) {
        for (let index = 0; index + 4 < data.length; index += 5) {
            const deltaLine = data[index];
            const deltaStart = data[index + 1];
            const length = data[index + 2];
            line += deltaLine;
            character = deltaLine === 0 ? character + deltaStart : deltaStart;
            tokens.push({
                line,
                character,
                length,
                tokenType: data[index + 3],
                tokenModifiers: data[index + 4],
            });
        }
        return tokens;
    }

    for (const item of data) {
        if (!item || typeof item !== 'object') {
            continue;
        }
        const deltaLine = item.deltaLine ?? item.delta_line;
        const deltaStart = item.deltaStart ?? item.delta_start;
        if (typeof deltaLine !== 'number' || typeof deltaStart !== 'number') {
            continue;
        }
        line += deltaLine;
        character = deltaLine === 0 ? character + deltaStart : deltaStart;
        tokens.push({
            line,
            character,
            length: item.length,
            tokenType: item.tokenType ?? item.token_type,
            tokenModifiers: item.tokenModifiersBitset ?? item.token_modifiers_bitset ?? 0,
        });
    }
    return tokens;
}

function uniqueNonOverlappingTokens(tokens) {
    const sorted = validTokens(tokens).sort(compareAbsoluteTokens);
    const result = [];
    const seen = new Set();
    for (const token of sorted) {
        const key = tokenKey(token);
        if (seen.has(key)) {
            continue;
        }
        const previous = result[result.length - 1];
        if (previous && tokensOverlap(previous, token)) {
            continue;
        }
        seen.add(key);
        result.push(token);
    }
    return result;
}

function validTokens(tokens) {
    return tokens.filter(token =>
        token &&
        Number.isInteger(token.line) &&
        Number.isInteger(token.character) &&
        Number.isInteger(token.length) &&
        Number.isInteger(token.tokenType) &&
        Number.isInteger(token.tokenModifiers) &&
        token.line >= 0 &&
        token.character >= 0 &&
        token.length > 0 &&
        token.tokenType >= 0 &&
        token.tokenType < TOKEN_TYPES.length,
    );
}

function compareAbsoluteTokens(left, right) {
    return left.line - right.line ||
        left.character - right.character ||
        right.length - left.length ||
        left.tokenType - right.tokenType ||
        left.tokenModifiers - right.tokenModifiers;
}

function tokensOverlap(left, right) {
    if (left.line !== right.line) {
        return false;
    }
    const leftEnd = left.character + left.length;
    const rightEnd = right.character + right.length;
    return left.character < rightEnd && right.character < leftEnd;
}

function tokensByLine(tokens) {
    const byLine = new Map();
    for (const token of tokens) {
        let lineTokens = byLine.get(token.line);
        if (!lineTokens) {
            lineTokens = [];
            byLine.set(token.line, lineTokens);
        }
        lineTokens.push(token);
    }
    return byLine;
}

function lineTokensOverlap(token, lineTokens) {
    if (!lineTokens) {
        return false;
    }
    return lineTokens.some(lineToken => tokensOverlap(token, lineToken));
}

function tokenKey(token) {
    return `${token.line}:${token.character}:${token.length}:${token.tokenType}:${token.tokenModifiers}`;
}

function utf8ByteColumnToUtf16(lineText, targetByteColumn) {
    let byteColumn = 0;
    let utf16Column = 0;
    for (const character of lineText) {
        const nextByteColumn = byteColumn + Buffer.byteLength(character, 'utf8');
        if (nextByteColumn > targetByteColumn) {
            return utf16Column;
        }
        byteColumn = nextByteColumn;
        utf16Column += character.length;
    }
    return utf16Column;
}

function formatTree(node, depth = 0) {
    const indent = '  '.repeat(depth);
    const range = `${node.startPosition.row}:${node.startPosition.column}-${node.endPosition.row}:${node.endPosition.column}`;
    const childCount = getChildCount(node);
    const named = isNamed(node) ? '' : ' anonymous';
    if (childCount === 0) {
        return `${indent}(${node.type}${named} [${range}] ${JSON.stringify(node.text)})`;
    }

    const children = [];
    for (let index = 0; index < childCount; index += 1) {
        const child = getChild(node, index);
        if (child) {
            children.push(formatTree(child, depth + 1));
        }
    }
    return `${indent}(${node.type}${named} [${range}]\n${children.join('\n')}\n${indent})`;
}

function getChild(node, index) {
    return typeof node.child === 'function' ? node.child(index) : undefined;
}

function getChildCount(node) {
    return typeof node.childCount === 'number' ? node.childCount : 0;
}

function isNamed(node) {
    if (typeof node.isNamed === 'function') {
        return node.isNamed();
    }
    return Boolean(node.isNamed);
}

module.exports = {
    activate,
    deactivate,
    _test: {
        resolveLanguageServerCommand,
        findExecutableOnPath,
        CielTreeSitterSemanticTokensProvider,
        cielMarkdownCodeBlocks,
        isCielFenceInfo,
        TOKEN_TYPES,
        TOKEN_MODIFIERS,
        CAPTURE_TOKENS,
    },
};
