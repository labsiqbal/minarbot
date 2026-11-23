import { builtinModules } from 'node:module';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import tseslint from 'typescript-eslint';

const root = dirname(fileURLToPath(import.meta.url));
// Browser imports outside src receive the same boundary checks.
export const browserSafeModules = [
  'shared/agent-create.ts', 'shared/agent-template.ts', 'shared/agent-visibility.ts', 'shared/mcp-scopes.ts',
  'shared/skill-command.ts', 'shared/skill-files.ts', 'shared/skill-pack-schema.ts', 'shared/skill-zip.ts',
  'shared/speech-preference.ts', 'shared/speech.ts', 'shared/turn-stream.ts',
];
// yaml's default/browser export is node-free; its Node entry is not used by Vite.
const browserSafePackages = new Set(['yaml']);
const builtinPattern = `^(?:node:|(?:${[...new Set(builtinModules.map(name => name.replace(/^node:/, '').split('/')[0]))].join('|')})(?:[/?#]|$))`;
const nodeImport = new RegExp(builtinPattern);
const serverMessage = 'Server module in browser code: import its types with `import type`, or move browser-safe code into an allowlisted module.';
const nodeMessage = 'Node builtin in browser code: use a browser API, or import its types with `import type`.';
const typeOnly = node => node.importKind === 'type' || node.exportKind === 'type'
  || (node.specifiers?.length > 0 && node.specifiers.every(specifier => specifier.importKind === 'type' || specifier.exportKind === 'type'));

export const clientServerBoundary = {
  meta: { type: 'problem', schema: [], messages: { server: serverMessage, node: nodeMessage, literal: 'Use a literal module path so the browser/server boundary can be checked.' } },
  create(context) {
    // ponytail: no local import aliases today; resolve them here if Vite adds aliases.
    function check(node, source, dynamic = false) {
      const specifier = source?.type === 'TemplateLiteral' && source.expressions.length === 0 ? source.quasis[0].value.cooked : source?.value;
      if (typeof specifier !== 'string') { context.report({ node, messageId: 'literal' }); return; }
      // Static builtins are covered by no-restricted-imports (including allowTypeImports).
      if (nodeImport.test(specifier)) { if (dynamic) context.report({ node, messageId: 'node' }); return; }
      if (specifier.startsWith('.') || specifier.startsWith('/')) {
        const path = specifier.split(/[?#]/)[0];
        const target = relative(root, path.startsWith('/') ? resolve(root, '.' + path) : resolve(dirname(context.filename), path)).replaceAll('\\', '/');
        if (target.startsWith('src/')) return;
        if (browserSafeModules.some(file => target === file || target === file.slice(0, -3))) return;
      } else if (!specifier.startsWith('file:') && (relative(root, context.filename).replaceAll('\\', '/').startsWith('src/') || browserSafePackages.has(specifier))) return;
      context.report({ node, messageId: 'server' });
    }
    return {
      ImportDeclaration(node) { if (!typeOnly(node)) check(node, node.source); },
      ExportNamedDeclaration(node) { if (node.source && !typeOnly(node)) check(node, node.source); },
      ExportAllDeclaration(node) { if (!typeOnly(node)) check(node, node.source); },
      ImportExpression(node) { check(node, node.source, true); },
      CallExpression(node) { if (node.callee.type === 'Identifier' && node.callee.name === 'require') check(node, node.arguments[0], true); },
      TSImportEqualsDeclaration(node) { if (node.importKind !== 'type' && node.moduleReference.type === 'TSExternalModuleReference') check(node, node.moduleReference.expression, true); },
    };
  },
};

export const comments = {
  meta: {
    type: 'suggestion', schema: [],
    messages: {
      shape: 'Keep comments on one line, at most 120 characters including delimiters; move rationale to an issue or commit.',
      justification: 'Fix the root cause, or open a GitHub issue and reference it with TODO(#123).',
    },
  },
  create(context) {
    return {
      Program() {
        const source = context.sourceCode, all = source.getAllComments();
        const firstToken = source.getFirstToken(source.ast)?.range[0] ?? source.text.length;
        const headers = all.filter(comment => comment.range[1] <= firstToken);
        if (headers.some(comment => /(?:@generated\b|\bauto-generated\b|\bCode generated\b.*\bDO NOT EDIT\b)/i.test(comment.value))) return;
        for (const comment of all) {
          const header = headers.includes(comment) && /\b(?:copyright|SPDX-License-Identifier|MIT|provenance|adapted from T3)\b/i.test(comment.value);
          if (!header && (comment.loc.start.line !== comment.loc.end.line || [...source.getText(comment)].length > 120))
            context.report({ node: comment, messageId: 'shape' });
          const text = comment.value.replace(/\bTODO\(#[1-9]\d*\)/g, '');
          if (/\b(?:workaround|for\s+now|temporary|hack|in\s+case|quick\s+fix|TODO|FIXME)\b/i.test(text))
            context.report({ node: comment, messageId: 'justification' });
        }
      },
    };
  },
};

const foundation = [
  { ignores: ['**/dist/**', '**/node_modules/**'] },
  {
    files: ['**/*.{ts,tsx,mjs,js}'],
    languageOptions: { parser: tseslint.parser },
    plugins: { minarbot: { rules: { 'client-server-boundary': clientServerBoundary, comments } } },
    rules: { 'no-debugger': 'error', 'no-unreachable': 'error' },
  },
  {
    files: ['src/**/*.{ts,tsx,mjs,js}'],
    rules: { 'no-restricted-syntax': ['error', {
      selector: 'CallExpression[callee.type="MemberExpression"]:matches([callee.property.name="json"][callee.computed=false], [callee.property.value="json"][callee.computed=true])',
      message: 'Use readJson from src/http.ts to handle proxy and API errors.',
    }] },
  },
  {
    files: ['**/*.test.{ts,tsx,mjs,js}', 'test.ts', 'test-support/*.ts'],
    ignores: ['test-support/database.ts'],
    rules: { 'no-restricted-syntax': ['error', {
      selector: "NewExpression[callee.name='DatabaseSync']",
      message: 'Open test databases with testDatabase() so tests wait for server write locks.',
    }] },
  },
  // #39: product comments; test bodies and lint fixtures remain outside product scope.
  {
    files: ['**/*.{ts,tsx,mjs,js}'],
    ignores: ['**/*.test.{ts,tsx,mjs,js}', 'test.ts', 'lint/check.mjs', '**/generated/**', '**/*.generated.{ts,tsx,mjs,js}'],
    rules: { 'minarbot/comments': 'error' },
  },
  {
    files: ['src/**/*.{ts,tsx,mjs,js}', ...browserSafeModules],
    plugins: { '@typescript-eslint': tseslint.plugin },
    rules: {
      '@typescript-eslint/no-restricted-imports': ['error', { patterns: [{ regex: builtinPattern, allowTypeImports: true, message: nodeMessage }] }],
      'minarbot/client-server-boundary': 'error',
    },
  },
];

// #38: timer and explicit-any ratchets.
export const timerAllowlist = [];
const timerMessage = 'Use usePolling from src/hooks/usePolling for refresh loops or use a focused browser timer helper.';
export const lifecycleRatchets = [
  {
    files: ['**/*.{ts,tsx,mjs,js}'], ignores: ['**/*.test.ts', '**/test.ts', '**/*-it.ts'],
    plugins: { '@typescript-eslint': tseslint.plugin },
    rules: { '@typescript-eslint/no-explicit-any': 'error' },
  },
  {
    files: ['src/**/*.{ts,tsx,mjs,js}'], ignores: ['src/hooks/**', ...timerAllowlist],
    languageOptions: { globals: { window: 'readonly', self: 'readonly', globalThis: 'readonly' } },
    rules: {
      'no-restricted-globals': ['error', { globals: ['setTimeout', 'setInterval', 'requestAnimationFrame', 'requestIdleCallback'].map(name => ({ name, message: timerMessage })), checkGlobalObject: true }],
    },
  },
];

export default [...foundation, ...lifecycleRatchets];
