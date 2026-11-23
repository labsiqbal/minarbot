import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';
import { mcpScopeCatalog } from './shared/mcp-scopes';
import { settingsGroups } from '../crates/web/assets/settings-groups';
import { colorThemes, themeStyles } from '../crates/web/assets/ui/themes';
import icons from '../crates/web/assets/lucide.json';

const {
  AlertTriangle, ArrowDownToLine, ArrowLeft, ArrowRight, ArrowUp, Blocks, BookOpen, Bot, CalendarClock,
  CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, ChevronUp, CircleAlert, Copy, Database, Download,
  Ellipsis, ExternalLink, FileText, Files, Filter, Folder, FolderOpen, GitFork, Hash, Info, KeyRound, Link,
  MessageCircle, MessageSquare, Mic, Monitor, Package, PanelLeft, PanelRight, Pencil, Plug, Plus, Receipt,
  RefreshCw, RotateCcw, Search, Send, Server, Settings, ShieldCheck, Smartphone, Square, Trash2, Users, Volume2,
  Workflow, Wrench, X, Brain, CircleDot, Clock, Eye, EyeOff, FileDiff, GitBranch, Globe, History, Inbox,
  ListRestart, ListTodo, LogOut, Pause, Play, Redo2, Siren, Undo2, WrapText,
} = icons;

const entrypoints = process.env.MINARBOT_LEPTOS_INPUTS;
if (!entrypoints) throw new Error('Run cargo xtask leptos to build the Leptos clients.');
const input: Record<string, string> = JSON.parse(entrypoints);
const conversation = fileURLToPath(new URL('./src/leptos-conversation.ts', import.meta.url));

const svg = (icon: string, size = 16) => icon.replace('width="16" height="16"', `width="${size}" height="${size}"`);

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
        message: svg(MessageSquare), close: svg(X),
        settingsChoices: settingsGroups.flatMap(({ items }) => items.map(({ id, label, icon }) => ({ id, label, svg: svg(icons[icon]) }))),
        folder: svg(Folder), folderOpen: svg(FolderOpen), hash: svg(Hash), chevron: svg(ChevronRight),
      },
    };
    const systemIcons = {
          alert: svg(CircleAlert), compactAlert: svg(CircleAlert, 15), info: svg(Info), warn: svg(AlertTriangle), close: svg(X),
          monitor: svg(Monitor), monitorEmpty: svg(Monitor, 18), download: svg(Download, 17), update: svg(ArrowDownToLine, 15),
          smartphone: svg(Smartphone), database: svg(Database), databaseEmpty: svg(Database, 18),
          key: svg(KeyRound), keyEmpty: svg(KeyRound, 18), receipt: svg(Receipt, 18),
          plus: svg(Plus, 14), down: svg(ChevronDown, 14), left: svg(ChevronLeft), right: svg(ChevronRight),
          arrow: svg(ArrowRight, 13), copy: svg(Copy, 15), check: svg(Check, 15),
          more: svg(Ellipsis), refresh: svg(RefreshCw, 15),
    };
    const connectionIcons = {
          'arrow-left': ArrowLeft, blocks: Blocks, book: BookOpen, bot: Bot, brain: Brain, history: History,
          'chevron-down': ChevronDown, 'chevron-left': ChevronLeft, 'chevron-right': ChevronRight,
          close: X, copy: Copy, ellipsis: Ellipsis, error: CircleAlert, 'external-link': ExternalLink, globe: Globe, check: Check,
          file: FileText, info: Info, key: KeyRound, link: Link, message: MessageCircle, package: Package, plug: Plug, plus: Plus, 'plus-md': Plus,
          download: Download, edit: Pencil, inbox: Inbox, eye: Eye, 'eye-off': EyeOff, trash: Trash2,
          folder: Folder, 'git-branch': GitBranch, refresh: RefreshCw,
          search: Search, server: Server, shield: ShieldCheck, warn: AlertTriangle,
    };
    const iconMap = (icons: Record<string, string>) => Object.fromEntries(Object.entries(icons)
      .map(([name, icon]) => [name, svg(icon, ['arrow-left', 'external-link', 'plus', 'search'].includes(name) ? 14 : name === 'plus-md' ? 15 : ['server', 'blocks'].includes(name) ? 20 : 16)]));
    const conversationIcons = {
      attach: svg(Plus), send: svg(ArrowUp), mic: svg(Mic, 14), stop: svg(Square, 14),
      chevron: svg(ChevronDown, 14), panel: svg(PanelRight), search: svg(Search, 15),
      copy: svg(Copy, 14), fork: svg(GitFork, 14),
      restore: svg(RotateCcw, 14), edit: svg(Pencil, 14), listen: svg(Volume2, 14), up: svg(ChevronUp, 16),
      inspect: svg(Wrench, 14), close: svg(X),
      chats: svg(MessageSquare, 14), files: svg(Files, 14), file: svg(FileText, 16), filter: svg(Filter, 15),
      settings: svg(Settings, 16),
      computer: svg(Monitor, 14), members: svg(Users, 14),
      shield: svg(ShieldCheck, 18), more: svg(Ellipsis, 16), external: svg(ExternalLink, 14),
      folder: svg(Folder), next: svg(ChevronRight, 14), alert: svg(CircleAlert, 14),
      back: svg(ArrowLeft, 15), download: svg(Download, 15), globe: svg(Globe, 14), diff: svg(FileDiff, 14),
      branch: svg(GitBranch, 14), wrap: svg(WrapText, 15),
      book: svg(BookOpen, 15), summary: svg(ListRestart, 15),
    };
    const configs = {
      appearance: {
        ...shell,
        groups: settingsGroups.map(({ label, items }) => ({
          label, items: items.map(({ id, label, icon }) => ({ id, label, svg: svg(icons[icon]) })),
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
        conversationIcons,
        connectionIcons: iconMap({ ...connectionIcons, workflow: Workflow, calendar: CalendarDays, undo: Undo2, redo: Redo2,
          slack: Hash, linear: ListTodo, sentry: Siren, github: CircleDot, schedule: CalendarClock, pause: Pause, play: Play }),
      },
      conversation: {
        ...shell,
        systemIcons,
        connectionIcons: iconMap({ ...connectionIcons, clock: Clock, workflow: Workflow, schedule: CalendarClock,
          undo: Undo2, redo: Redo2 }),
        conversationIcons,
      },
      hub: {
        colors: colorThemes,
        icons: { ...iconMap({ bot: Bot, folder: FolderOpen, monitor: Monitor, logout: LogOut, users: Users,
          hash: Hash, refresh: RefreshCw, ellipsis: Ellipsis, send: ArrowUp, close: X, 'external-link': ExternalLink }),
          brand: svg(Bot, 24), plus: svg(Plus, 15), 'chevron-down': svg(ChevronDown, 14) },
      },
    };
    for (const [group, config] of Object.entries(configs)) {
      this.emitFile({ type: 'asset', fileName: `${group}-config.json`, source: JSON.stringify(config) });
    }
  },
};

export default defineConfig({
  css: {
    postcss: { plugins: [{
      postcssPlugin: 'minarbot-relative-type',
      Declaration(declaration) {
        // SVG diagram coordinates and physical panel widths stay in pixels.
        if (['font', 'font-size', 'line-height'].includes(declaration.prop) || declaration.prop.startsWith('--text-'))
          declaration.value = declaration.value.replace(/(\d*\.?\d+)px\b/g, (_, pixels) => `${Number(pixels) / 16}rem`);
      },
    }] },
  },
  base: '/_leptos/',
  publicDir: false,
  resolve: { alias: {
    'minarbot-conversation-adapter': conversation,
    'minarbot-workflow-markdown': fileURLToPath(new URL('./src/workflow-markdown.ts', import.meta.url)),
    'minarbot-workflows-css': fileURLToPath(new URL('../crates/web/workflow-content.css', import.meta.url)),
  } },
  plugins: [appearanceConfig],
  build: {
    reportCompressedSize: false,
    manifest: true,
    rollupOptions: {
      input,
      output: {
        manualChunks(id) {
          const path = id.replaceAll('\\', '/');
          if (path.includes('/node_modules/marked/') || path.includes('/node_modules/entities/')) return 'markdown-vendor';
        },
        hashCharacters: 'hex',
        entryFileNames: 'assets/[name].[hash:16].js',
        chunkFileNames: 'assets/[name].[hash:16].js',
        assetFileNames: 'assets/[name].[hash:16][extname]',
      },
    },
  },
});
