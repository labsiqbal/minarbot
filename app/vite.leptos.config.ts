import { fileURLToPath } from 'node:url';
import { AlertTriangle, ArrowLeft, ArrowRight, Blocks, BookOpen, Bot, CalendarClock, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, CircleAlert, Copy, Database, Download, Ellipsis, ExternalLink, FileText, Filter, Folder, FolderOpen, Hash, Info, KeyRound, Link, MessageCircle, Monitor, Package, PanelLeft, Plug, Plus, Receipt, RefreshCw, Search, Send, Server, Settings, ShieldCheck, Smartphone, Volume2, Workflow, X, type LucideIcon } from 'lucide-react';
import { createElement } from 'react';
import { CircleDot, ListTodo, Siren } from 'lucide-react';
import { renderToStaticMarkup } from 'react-dom/server';
import { defineConfig, mergeConfig, type Plugin } from 'vite';
import { mcpScopeCatalog } from './shared/mcp-scopes';
import { settingsGroups } from './src/settings-groups';
import { colorThemes, themeStyles } from './src/ui/themes';
import app from './vite.config';

const start = process.env.MINARBOT_LEPTOS_START;
if (!start) throw new Error('Run cargo xtask leptos to build Appearance.');

const svg = (icon: LucideIcon, size = 16) => renderToStaticMarkup(createElement(icon, { size, 'aria-hidden': true }));

const appearanceConfig: Plugin = {
  name: 'minarbot-appearance-config',
  generateBundle() {
    this.emitFile({ type: 'asset', fileName: 'themes.css', source: themeStyles() });
    this.emitFile({
      type: 'asset', fileName: 'appearance-config.json', source: JSON.stringify({
        colors: colorThemes,
        groups: settingsGroups.map(({ label, items }) => ({
          label, items: items.map(({ id, label, icon }) => ({ id, label, svg: svg(icon) })),
        })),
        searchIcon: svg(Search, 14),
        closeIcon: svg(X),
        errorIcon: svg(CircleAlert),
        mcpScopes: mcpScopeCatalog,
        systemIcons: {
          alert: svg(CircleAlert), info: svg(Info), warn: svg(AlertTriangle), close: svg(X),
          monitor: svg(Monitor), monitorEmpty: svg(Monitor, 18), download: svg(Download, 17),
          smartphone: svg(Smartphone), database: svg(Database), databaseEmpty: svg(Database, 18),
          key: svg(KeyRound), keyEmpty: svg(KeyRound, 18), receipt: svg(Receipt, 18),
          plus: svg(Plus, 14), down: svg(ChevronDown, 14), left: svg(ChevronLeft), right: svg(ChevronRight),
          arrow: svg(ArrowRight, 13), copy: svg(Copy, 15), check: svg(Check, 15),
          more: svg(Ellipsis), refresh: svg(RefreshCw, 15),
        },
        connectionIcons: Object.fromEntries(Object.entries({
          'arrow-left': ArrowLeft, blocks: Blocks, book: BookOpen, bot: Bot,
          'chevron-down': ChevronDown, 'chevron-left': ChevronLeft, 'chevron-right': ChevronRight,
          close: X, copy: Copy, ellipsis: Ellipsis, error: CircleAlert, 'external-link': ExternalLink,
          file: FileText, info: Info, key: KeyRound, link: Link, message: MessageCircle, package: Package, plug: Plug, plus: Plus,
          search: Search, server: Server, shield: ShieldCheck, warn: AlertTriangle, workflow: Workflow, calendar: CalendarDays,
          slack: Hash, linear: ListTodo, sentry: Siren, github: CircleDot, schedule: CalendarClock,
        }).map(([name, icon]) => [name, svg(icon, ['arrow-left', 'external-link', 'plus', 'search'].includes(name) ? 14 : name === 'server' ? 20 : 16)])),
        workspaceIcons: {
          panel: svg(PanelLeft), filter: svg(Filter, 15), settings: svg(Settings),
          routines: svg(CalendarClock), workflows: svg(Workflow), calendar: svg(CalendarDays),
          search: svg(Search, 15), info: svg(Info, 14), voice: svg(Volume2),
          monitor: svg(Monitor), plug: svg(Plug, 15), plus: svg(Plus, 15), send: svg(Send, 15), check: svg(Check, 15),
          folder: svg(Folder), folderOpen: svg(FolderOpen), hash: svg(Hash), chevron: svg(ChevronRight),
        },
      }),
    });
  },
};

export default defineConfig(mergeConfig(app, {
  base: '/_leptos/',
  publicDir: false,
  resolve: { alias: { 'minarbot-workflow-markdown': fileURLToPath(new URL('./src/workflow-markdown.ts', import.meta.url)) } },
  plugins: [appearanceConfig],
  build: {
    reportCompressedSize: false,
    rollupOptions: {
      input: { start, appearance: fileURLToPath(new URL('../crates/web/appearance.css', import.meta.url)) },
      output: {
        hashCharacters: 'hex',
        entryFileNames: 'assets/[name].[hash:16].js',
        chunkFileNames: 'assets/[name].[hash:16].js',
        assetFileNames: 'assets/[name].[hash:16][extname]',
      },
    },
  },
}));
