import { fileURLToPath } from 'node:url';
import { AlertTriangle, ArrowRight, Check, ChevronDown, ChevronLeft, ChevronRight, Copy, Database, Download, Ellipsis, KeyRound, Monitor, Plus, Receipt, RefreshCw, Smartphone, CalendarClock, CalendarDays, CircleAlert, Filter, Info, PanelLeft, Search, Settings, Volume2, Workflow, X, type LucideIcon } from 'lucide-react';
import { createElement } from 'react';
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
