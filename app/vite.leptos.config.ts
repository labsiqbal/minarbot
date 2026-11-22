import { fileURLToPath } from 'node:url';
import { AlertTriangle, ArrowLeft, ArrowRight, ArrowUp, Blocks, BookOpen, Bot, CalendarClock, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, CircleAlert, Copy, Database, Download, Ellipsis, ExternalLink, FileText, Files, Filter, Folder, FolderOpen, GitFork, Hash, Info, KeyRound, Link, MessageCircle, MessageSquare, Mic, Monitor, Package, PanelLeft, PanelRight, Plug, Plus, Receipt, RefreshCw, Search, Send, Server, Settings, ShieldCheck, Smartphone, Square, Users, Volume2, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
import { CircleDot, ListTodo, Siren } from 'lucide-react';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { defineConfig, mergeConfig, type Plugin } from 'vite';
import { mcpScopeCatalog } from './shared/mcp-scopes';
import { settingsGroups } from './src/settings-groups';
import { colorThemes, themeStyles } from './src/ui/themes';
import app from './vite.config';

const entrypoints = process.env.MINARBOT_LEPTOS_INPUTS;
if (!entrypoints) throw new Error('Run cargo xtask leptos to build the Leptos clients.');
const input: Record<string, string> = JSON.parse(entrypoints);
const conversation = fileURLToPath(new URL('./src/leptos-conversation.ts', import.meta.url));

const svg = (icon: LucideIcon, size = 16) => renderToStaticMarkup(createElement(icon, { size, 'aria-hidden': true }));

const appearanceConfig: Plugin = {
  name: 'minarbot-appearance-config',
  generateBundle() {
    this.emitFile({ type: 'asset', fileName: 'themes.css', source: themeStyles() });
    const shell = {
      colors: colorThemes,
      workspaceIcons: {
        panel: svg(PanelLeft), filter: svg(Filter, 15), settings: svg(Settings),
        routines: svg(CalendarClock), workflows: svg(Workflow), calendar: svg(CalendarDays),
        search: svg(Search, 15), quickSearch: svg(Search, 16), book: svg(BookOpen, 16), info: svg(Info, 14), voice: svg(Volume2),
        monitor: svg(Monitor), plug: svg(Plug, 15), plus: svg(Plus, 15), send: svg(Send, 15), check: svg(Check, 15),
        folder: svg(Folder), folderOpen: svg(FolderOpen), hash: svg(Hash), chevron: svg(ChevronRight),
      },
    };
    const systemIcons = {
          alert: svg(CircleAlert), info: svg(Info), warn: svg(AlertTriangle), close: svg(X),
          monitor: svg(Monitor), monitorEmpty: svg(Monitor, 18), download: svg(Download, 17),
          smartphone: svg(Smartphone), database: svg(Database), databaseEmpty: svg(Database, 18),
          key: svg(KeyRound), keyEmpty: svg(KeyRound, 18), receipt: svg(Receipt, 18),
          plus: svg(Plus, 14), down: svg(ChevronDown, 14), left: svg(ChevronLeft), right: svg(ChevronRight),
          arrow: svg(ArrowRight, 13), copy: svg(Copy, 15), check: svg(Check, 15),
          more: svg(Ellipsis), refresh: svg(RefreshCw, 15),
    };
    const connectionIcons = {
          'arrow-left': ArrowLeft, blocks: Blocks, book: BookOpen, bot: Bot,
          'chevron-down': ChevronDown, 'chevron-left': ChevronLeft, 'chevron-right': ChevronRight,
          close: X, copy: Copy, ellipsis: Ellipsis, error: CircleAlert, 'external-link': ExternalLink,
          file: FileText, info: Info, key: KeyRound, link: Link, message: MessageCircle, package: Package, plug: Plug, plus: Plus,
          search: Search, server: Server, shield: ShieldCheck, warn: AlertTriangle,
    };
    const iconMap = (icons: Record<string, LucideIcon>) => Object.fromEntries(Object.entries(icons)
      .map(([name, icon]) => [name, svg(icon, ['arrow-left', 'external-link', 'plus', 'search'].includes(name) ? 14 : name === 'server' ? 20 : 16)]));
    const configs = {
      appearance: {
        ...shell,
        groups: settingsGroups.map(({ label, items }) => ({
          label, items: items.map(({ id, label, icon }) => ({ id, label, svg: svg(icon) })),
        })),
        searchIcon: svg(Search, 14),
        closeIcon: svg(X),
        errorIcon: svg(CircleAlert),
        mcpScopes: mcpScopeCatalog,
        systemIcons,
        connectionIcons: iconMap(connectionIcons),
      },
      workflows: {
        ...shell,
        errorIcon: svg(CircleAlert),
        systemIcons,
        connectionIcons: iconMap({ ...connectionIcons, workflow: Workflow, calendar: CalendarDays,
          slack: Hash, linear: ListTodo, sentry: Siren, github: CircleDot, schedule: CalendarClock }),
      },
      conversation: {
        ...shell,
        conversationIcons: {
          attach: svg(Plus), send: svg(ArrowUp), mic: svg(Mic, 14), stop: svg(Square, 14),
          chevron: svg(ChevronDown, 14), panel: svg(PanelRight), search: svg(Search, 15),
          copy: svg(Copy, 14), fork: svg(GitFork, 14),
          inspect: svg(Wrench, 14), close: svg(X),
          chats: svg(MessageSquare, 14), files: svg(Files, 14), file: svg(FileText, 16), filter: svg(Filter, 15),
          settings: svg(Settings, 16),
          computer: svg(Monitor, 14), members: svg(Users, 14),
          shield: svg(ShieldCheck, 18), more: svg(Ellipsis, 16), external: svg(ExternalLink, 14),
          folder: svg(Folder), next: svg(ChevronRight, 14), alert: svg(CircleAlert, 14),
        },
      },
    };
    for (const [group, config] of Object.entries(configs)) {
      this.emitFile({ type: 'asset', fileName: `${group}-config.json`, source: JSON.stringify(config) });
    }
  },
};

export default defineConfig(mergeConfig(app, {
  base: '/_leptos/',
  publicDir: false,
  resolve: { alias: {
    'minarbot-conversation-adapter': conversation,
    'minarbot-workflow-markdown': fileURLToPath(new URL('./src/workflow-markdown.ts', import.meta.url)),
  } },
  plugins: [appearanceConfig],
  build: {
    reportCompressedSize: false,
    rollupOptions: {
      input,
      output: {
        hashCharacters: 'hex',
        entryFileNames: 'assets/[name].[hash:16].js',
        chunkFileNames: 'assets/[name].[hash:16].js',
        assetFileNames: 'assets/[name].[hash:16][extname]',
      },
    },
  },
}));
