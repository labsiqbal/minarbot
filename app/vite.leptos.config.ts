import { fileURLToPath } from 'node:url';
import { ArrowUp, CalendarClock, CalendarDays, Check, ChevronDown, ChevronRight, CircleAlert, Copy, Files, Filter, Folder, FolderOpen, GitFork, Hash, Info, MessageSquare, Mic, Monitor, PanelLeft, PanelRight, Plug, Plus, Search, Send, Settings, Square, Users, Volume2, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { defineConfig, mergeConfig, type Plugin } from 'vite';
import { settingsGroups } from './src/settings-groups';
import { colorThemes, themeStyles } from './src/ui/themes';
import app from './vite.config';

const start = process.env.MINARBOT_LEPTOS_START;
const conversationStart = process.env.MINARBOT_LEPTOS_CONVERSATION_START;
if (!start || !conversationStart) throw new Error('Run cargo xtask leptos to build Appearance.');
const conversation = fileURLToPath(new URL('./src/leptos-conversation.ts', import.meta.url));

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
        conversationIcons: {
          attach: svg(Plus), send: svg(ArrowUp), mic: svg(Mic, 14), stop: svg(Square, 14),
          chevron: svg(ChevronDown, 14), panel: svg(PanelRight), search: svg(Search, 15),
          copy: svg(Copy, 14), fork: svg(GitFork, 14),
          inspect: svg(Wrench, 14), close: svg(X),
          chats: svg(MessageSquare, 14), files: svg(Files, 14), filter: svg(Filter, 15),
          computer: svg(Monitor, 14), members: svg(Users, 14),
        },
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
  resolve: { alias: { 'minarbot-conversation-adapter': conversation } },
  plugins: [appearanceConfig],
  build: {
    reportCompressedSize: false,
    rollupOptions: {
      input: {
        start,
        'conversation-start': conversationStart,
        appearance: fileURLToPath(new URL('../crates/web/appearance.css', import.meta.url)),
        conversation: fileURLToPath(new URL('../crates/web/conversation.css', import.meta.url)),
      },
      output: {
        hashCharacters: 'hex',
        entryFileNames: 'assets/[name].[hash:16].js',
        chunkFileNames: 'assets/[name].[hash:16].js',
        assetFileNames: 'assets/[name].[hash:16][extname]',
      },
    },
  },
}));
