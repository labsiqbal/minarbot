import { fileURLToPath } from 'node:url';
import { AlertTriangle, ArrowLeft, Blocks, BookOpen, Bot, CalendarClock, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, CircleAlert, Copy, Ellipsis, ExternalLink, FileText, Filter, Folder, FolderOpen, Hash, Info, KeyRound, Link, MessageCircle, Monitor, Package, PanelLeft, Plug, Plus, Search, Send, Server, Settings, ShieldCheck, Volume2, Workflow, X, type LucideIcon } from 'lucide-react';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { defineConfig, mergeConfig, type Plugin } from 'vite';
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
        connectionIcons: Object.fromEntries(Object.entries({
          'arrow-left': ArrowLeft, blocks: Blocks, book: BookOpen, bot: Bot,
          'chevron-down': ChevronDown, 'chevron-left': ChevronLeft, 'chevron-right': ChevronRight,
          close: X, copy: Copy, ellipsis: Ellipsis, error: CircleAlert, 'external-link': ExternalLink,
          file: FileText, info: Info, key: KeyRound, link: Link, message: MessageCircle, package: Package, plug: Plug, plus: Plus,
          search: Search, server: Server, shield: ShieldCheck, warn: AlertTriangle,
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
