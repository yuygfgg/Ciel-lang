const fs = require('fs');
const path = require('path');
const vscode = require('vscode');

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
];

const TOKEN_TYPE_INDEX = new Map(TOKEN_TYPES.map((type, index) => [type, index]));
const TOKEN_MODIFIER_INDEX = new Map(TOKEN_MODIFIERS.map((modifier, index) => [modifier, index]));

const legend = new vscode.SemanticTokensLegend(TOKEN_TYPES, TOKEN_MODIFIERS);

const RESERVED_KEYWORDS = new Set([
    'as',
    'break',
    'case',
    'const',
    'continue',
    'default',
    'defer',
    'else',
    'enum',
    'export',
    'extern',
    'false',
    'for',
    'if',
    'impl',
    'import',
    'interface',
    'never',
    'noescape',
    'null',
    'opaque',
    'return',
    'struct',
    'switch',
    'true',
    'type',
    'unsafe',
    'void',
    'while',
    '#if',
    '#elif',
    '#else',
    '#endif',
    '#c_include',
]);

const CONTEXTUAL_KEYWORDS = new Set([
    'async',
    'await',
    'biased',
    'select',
    'fn',
]);

const OPERATORS = new Set([
    '=',
    '+',
    '-',
    '*',
    '/',
    '%',
    '!',
    '~',
    '&',
    '|',
    '^',
    '==',
    '!=',
    '<',
    '<=',
    '>',
    '>=',
    '<<',
    '>>',
    '&&',
    '||',
    '->',
    '?',
    '?*',
    '*const',
    '?*const',
]);

function activate(context) {
    const provider = new CielSemanticTokensProvider(context);
    context.subscriptions.push(
        vscode.languages.registerDocumentSemanticTokensProvider(
            { language: 'ciel' },
            provider,
            legend,
        ),
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('ciel.showSyntaxTree', async () => {
            const editor = vscode.window.activeTextEditor;
            if (!editor || editor.document.languageId !== 'ciel') {
                vscode.window.showInformationMessage('Open a Ciel source file first.');
                return;
            }

            const parser = await provider.getParser();
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
}

function deactivate() { }

class CielSemanticTokensProvider {
    constructor(context) {
        this.context = context;
        this.parserPromise = undefined;
        this.warnedMissingParser = false;
        this.warnedParserError = false;
    }

    async provideDocumentSemanticTokens(document, cancellationToken) {
        const builder = new vscode.SemanticTokensBuilder(legend);
        const parser = await this.getParser();
        if (!parser || cancellationToken.isCancellationRequested) {
            return builder.build();
        }

        const tree = parser.parse(document.getText());
        visitLeaves(tree.rootNode, node => {
            if (cancellationToken.isCancellationRequested) {
                return;
            }

            const token = classifyLeaf(node);
            if (!token) {
                return;
            }

            pushNodeToken(builder, document, node, token.type, token.modifiers);
        });

        return builder.build();
    }

    async getParser() {
        if (!this.parserPromise) {
            this.parserPromise = this.createParser();
        }
        return this.parserPromise;
    }

    async createParser() {
        const parserWasmPath = this.context.asAbsolutePath(
            path.join('parsers', 'tree-sitter-ciel.wasm'),
        );
        if (!fs.existsSync(parserWasmPath)) {
            this.warnMissingParser(parserWasmPath);
            return undefined;
        }

        try {
            const treeSitter = require('web-tree-sitter');
            const Parser = treeSitter.Parser || treeSitter.default || treeSitter;
            const Language = treeSitter.Language || Parser.Language;

            if (!Parser || typeof Parser !== 'function') {
                throw new Error('web-tree-sitter did not expose a Parser constructor');
            }
            if (!Language || typeof Language.load !== 'function') {
                throw new Error('web-tree-sitter did not expose Language.load');
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
            return parser;
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
}

function classifyLeaf(node) {
    const type = node.type;
    const text = node.text;
    if (!text || type === 'ERROR') {
        return undefined;
    }

    if (type === 'comment') {
        return token('comment');
    }
    if (type === 'string_literal' || type === 'char_literal') {
        return token('string');
    }
    if (type === 'integer_literal' || type === 'float_literal') {
        return token('number');
    }
    if (
        type === 'primitive_type' ||
        type === 'never_type' ||
        type === 'void_type' ||
        type === 'type_hole'
    ) {
        return token('type', ['defaultLibrary']);
    }
    if (type === 'bool_literal' || type === 'null_literal') {
        return token('keyword');
    }
    if (parentType(node) === 'config_call') {
        return token('function', ['defaultLibrary']);
    }
    const contextualIdentifier = contextualIdentifierNodeForLeaf(node);
    if (contextualIdentifier) {
        return classifyIdentifier(contextualIdentifier);
    }
    if (isKeywordToken(node)) {
        return token('keyword');
    }
    if (OPERATORS.has(text)) {
        return token('operator');
    }
    if (type === 'identifier') {
        return classifyIdentifier(node);
    }
    if (
        (type === 'regular_identifier' || type === 'contextual_identifier') &&
        node.parent &&
        node.parent.type === 'identifier'
    ) {
        return classifyIdentifier(node.parent);
    }

    return undefined;
}

function classifyIdentifier(node) {
    const parent = node.parent;
    if (!parent) {
        return token('variable');
    }

    if (parent.type === 'module_path') {
        return token('namespace');
    }

    if (parent.type === 'binding_name') {
        return classifyBindingName(parent);
    }

    if (parent.type === 'generic_parameter') {
        return token('typeParameter', ['declaration']);
    }

    if (parent.type === 'named_type') {
        return token('type');
    }

    if (parent.type === 'interface_term' || parent.type === 'constraint_term') {
        return token('interface');
    }

    if (parent.type === 'field_initializer') {
        return token('property');
    }

    if (parent.type === 'field_declaration') {
        return token('property', ['declaration']);
    }

    if (parent.type === 'variant_declaration') {
        return token('enumMember', ['declaration']);
    }

    if (parent.type === 'variant_pattern') {
        return token('enumMember');
    }

    if (parent.type === 'field_expression' || parent.type === 'arrow_expression') {
        return token('property');
    }

    if (parent.type === 'select_arm') {
        return token('variable', ['declaration']);
    }

    if (parent.type === 'qualified_name') {
        return classifyQualifiedNamePart(node, parent);
    }

    if (isDirectChild(parent, node)) {
        switch (parent.type) {
            case 'function_signature':
            case 'interface_signature':
            case 'impl_declaration':
                return token('function', ['declaration']);
            case 'type_alias_declaration':
            case 'opaque_struct_declaration':
                return token('type', ['declaration']);
            case 'struct_declaration':
                return token('struct', ['declaration']);
            case 'enum_declaration':
                return token('enum', ['declaration']);
            case 'interface_alias_declaration':
                return token('interface', ['declaration']);
            case 'import_declaration':
                return token('namespace', ['declaration']);
            default:
                break;
        }
    }

    if (isCallFunctionIdentifier(node)) {
        return token('function');
    }

    return token('variable');
}

function classifyBindingName(bindingNameNode) {
    const owner = bindingNameNode.parent;
    if (!owner) {
        return token('variable');
    }

    switch (owner.type) {
        case 'parameter':
        case 'closure_parameter':
            return token('parameter', ['declaration']);
        case 'var_declaration_clause':
            return token('variable', ['declaration']);
        case 'pattern':
        case 'variant_pattern':
            return token('variable', ['declaration']);
        default:
            return token('variable');
    }
}

function classifyQualifiedNamePart(node, qualifiedNameNode) {
    const identifiers = directIdentifierChildren(qualifiedNameNode);
    const lastIdentifier = identifiers[identifiers.length - 1];
    if (!sameNode(node, lastIdentifier)) {
        return token('namespace');
    }

    const owner = semanticOwnerForQualifiedName(qualifiedNameNode);
    if (owner && owner.type === 'call_expression') {
        return token('function');
    }
    if (owner && owner.type === 'named_type') {
        return token('type');
    }
    if (owner && (owner.type === 'interface_term' || owner.type === 'constraint_term')) {
        return token('interface');
    }
    if (owner && owner.type === 'impl_declaration') {
        return token('function', ['declaration']);
    }
    if (owner && owner.type === 'variant_pattern') {
        return token('enumMember');
    }
    return token('variable');
}

function isCallFunctionIdentifier(node) {
    const parent = node.parent;
    if (parent && parent.type === 'expression') {
        return isCallFunctionNode(parent.parent, parent);
    }
    return parent && parent.type === 'call_expression' && isCallFunctionNode(parent, node);
}

function isCallFunctionNode(callNode, candidate) {
    if (!callNode || callNode.type !== 'call_expression') {
        return false;
    }
    return sameNode(firstNamedChild(callNode), candidate);
}

function semanticOwnerForQualifiedName(qualifiedNameNode) {
    const parent = qualifiedNameNode.parent;
    if (!parent) {
        return undefined;
    }

    if (parent.type === 'expression' && isCallFunctionNode(parent.parent, parent)) {
        return parent.parent;
    }
    return parent;
}

function isKeywordToken(node) {
    const text = node.text;
    if (!RESERVED_KEYWORDS.has(text) && !CONTEXTUAL_KEYWORDS.has(text)) {
        return false;
    }

    return node.type === text;
}

function contextualIdentifierNodeForLeaf(node) {
    const contextualNode = node.parent;
    const identifierNode = contextualNode && contextualNode.parent;
    if (
        CONTEXTUAL_KEYWORDS.has(node.text) &&
        contextualNode &&
        contextualNode.type === 'contextual_identifier' &&
        identifierNode &&
        identifierNode.type === 'identifier'
    ) {
        return identifierNode;
    }
    return undefined;
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

function visitLeaves(node, onLeaf) {
    const childCount = getChildCount(node);
    if (childCount === 0) {
        onLeaf(node);
        return;
    }

    for (let index = 0; index < childCount; index += 1) {
        const child = getChild(node, index);
        if (child) {
            visitLeaves(child, onLeaf);
        }
    }
}

function pushNodeToken(builder, document, node, tokenType, tokenModifiers) {
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
            builder.push(line, startCharacter, length, tokenType, tokenModifiers);
        }
    }
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

function directIdentifierChildren(node) {
    const identifiers = [];
    const childCount = getChildCount(node);
    for (let index = 0; index < childCount; index += 1) {
        const child = getChild(node, index);
        if (child && child.type === 'identifier') {
            identifiers.push(child);
        }
    }
    return identifiers;
}

function firstNamedChild(node) {
    const childCount = getChildCount(node);
    for (let index = 0; index < childCount; index += 1) {
        const child = getChild(node, index);
        if (child && isNamed(child)) {
            return child;
        }
    }
    return undefined;
}

function isDirectChild(parent, node) {
    const childCount = getChildCount(parent);
    for (let index = 0; index < childCount; index += 1) {
        if (sameNode(getChild(parent, index), node)) {
            return true;
        }
    }
    return false;
}

function sameNode(left, right) {
    if (!left || !right) {
        return false;
    }
    return left.id === right.id ||
        (
            left.type === right.type &&
            left.startIndex === right.startIndex &&
            left.endIndex === right.endIndex
        );
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

function parentType(node) {
    return node.parent ? node.parent.type : undefined;
}

module.exports = {
    activate,
    deactivate,
    _test: {
        classifyLeaf,
        TOKEN_TYPES,
        TOKEN_MODIFIERS,
    },
};
