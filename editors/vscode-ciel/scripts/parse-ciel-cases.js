const fs = require('fs');
const path = require('path');

const extensionRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(extensionRoot, '..', '..');
const casesRoot = path.join(repoRoot, 'tests', 'cases');
const additionalPositiveRoots = [
    path.join(repoRoot, 'std'),
    path.join(repoRoot, 'examples'),
];
const parserWasmPath = path.join(extensionRoot, 'parsers', 'tree-sitter-ciel.wasm');

const REQUIRED_PARSE_KINDS = new Set([
    'compile',
    'run',
    'host',
    'dependency',
    'manual',
    'known-fail-compile',
    'known-fail-cc',
    'known-fail-run',
]);

const SKIPPED_KINDS = new Set([
    'error',
    'known-fail-accepts',
]);

async function main() {
    const parser = await createParser();
    const cases = collectCielFiles(casesRoot)
        .map(filePath => parseCase(filePath))
        .sort((left, right) => left.relativePath.localeCompare(right.relativePath));
    const sourceFiles = additionalPositiveRoots
        .flatMap(root => collectCielFiles(root))
        .map(filePath => parsePositiveSource(filePath))
        .sort((left, right) => left.relativePath.localeCompare(right.relativePath));

    const unknownKinds = cases.filter(testCase =>
        !REQUIRED_PARSE_KINDS.has(testCase.kind) && !SKIPPED_KINDS.has(testCase.kind)
    );
    if (unknownKinds.length > 0) {
        throw new Error(unknownKinds.map(testCase =>
            `${testCase.relativePath}: unknown ciel-test kind \`${testCase.kind}\``
        ).join('\n'));
    }

    const requiredFixtureCases = cases.filter(testCase => REQUIRED_PARSE_KINDS.has(testCase.kind));
    const requiredCases = [
        ...requiredFixtureCases,
        ...sourceFiles,
    ];
    const failures = [];
    for (const testCase of requiredCases) {
        const tree = parser.parse(testCase.source);
        const problems = [];
        collectParseProblems(tree.rootNode, problems);
        if (problems.length > 0) {
            failures.push(`${testCase.relativePath} (${testCase.kind})\n${problems.join('\n')}`);
        }
    }

    if (failures.length > 0) {
        throw new Error(
            `${failures.length} Ciel fixture file(s) produced Tree-sitter parse errors:\n\n` +
            failures.join('\n\n')
        );
    }

    const skipped = cases.length - requiredFixtureCases.length;
    console.log(
        `Tree-sitter parsed ${requiredCases.length} positive Ciel file(s); skipped ${skipped} negative fixture file(s).`
    );
}

async function createParser() {
    if (!fs.existsSync(parserWasmPath)) {
        throw new Error(`missing parser wasm at ${parserWasmPath}; run npm run build first`);
    }

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
    return parser;
}

function collectCielFiles(dir) {
    const result = [];
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const entryPath = path.join(dir, entry.name);
        if (entry.isDirectory()) {
            result.push(...collectCielFiles(entryPath));
        } else if (entry.isFile() && path.extname(entry.name) === '.ciel') {
            result.push(entryPath);
        }
    }
    return result;
}

function parseCase(filePath) {
    const source = fs.readFileSync(filePath, 'utf8');
    let kind;
    for (const line of source.split(/\r?\n/)) {
        const comment = line.trimStart();
        if (!comment.startsWith('//')) {
            continue;
        }
        const metadata = comment.slice(2).trimStart();
        const separator = metadata.indexOf(':');
        if (separator < 0) {
            continue;
        }
        const key = metadata.slice(0, separator).trim();
        const value = metadata.slice(separator + 1).trim();
        if (key === 'ciel-test') {
            if (kind !== undefined) {
                throw new Error(`${relativeToRepo(filePath)}: duplicate ciel-test metadata`);
            }
            kind = value;
        }
    }

    if (kind === undefined) {
        throw new Error(`${relativeToRepo(filePath)}: missing // ciel-test: ... metadata`);
    }

    return {
        filePath,
        relativePath: relativeToRepo(filePath),
        kind,
        source,
    };
}

function parsePositiveSource(filePath) {
    return {
        filePath,
        relativePath: relativeToRepo(filePath),
        kind: 'source',
        source: fs.readFileSync(filePath, 'utf8'),
    };
}

function collectParseProblems(node, problems) {
    if (node.type === 'ERROR' || isMissing(node)) {
        problems.push(formatProblem(node));
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

function formatProblem(node) {
    const start = node.startPosition;
    const end = node.endPosition;
    const location = `${start.row + 1}:${start.column + 1}-${end.row + 1}:${end.column + 1}`;
    const text = node.text ? ` ${JSON.stringify(truncate(node.text, 80))}` : '';
    return `  ${node.type} at ${location}${text}`;
}

function truncate(value, maxLength) {
    if (value.length <= maxLength) {
        return value;
    }
    return `${value.slice(0, maxLength - 3)}...`;
}

function relativeToRepo(filePath) {
    return path.relative(repoRoot, filePath).split(path.sep).join('/');
}

main().catch(error => {
    console.error(error.message);
    process.exit(1);
});
