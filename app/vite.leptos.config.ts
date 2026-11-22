import { fileURLToPath } from 'node:url';
import { CalendarClock, CalendarDays, CircleAlert, Filter, Info, PanelLeft, Search, Settings, Volume2, Workflow, X, type LucideIcon } from 'lucide-react';
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
        workspaceIcons: {
          panel: svg(PanelLeft), filter: svg(Filter, 15), settings: svg(Settings),
          routines: svg(CalendarClock), workflows: svg(Workflow), calendar: svg(CalendarDays),
          search: svg(Search, 15), info: svg(Info, 14), voice: svg(Volume2),
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
