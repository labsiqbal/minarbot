export type ColorMode = 'light' | 'dark';
export type ColorTheme = 'minarbot' | 'github' | 'catppuccin' | 'everforest' | 'solarized' | 'midnight';

type Palette = {
  background: string; foreground: string; card: string; muted: string;
  mutedForeground: string; popover: string; primary: string;
  primaryForeground: string; secondary: string; accent: string;
  border: string; ring: string; destructive: string;
  destructiveForeground: string; sidebarBackground: string;
  userBubble: string;
};

const minarbot = {
  light: { background: '#FCFCFC', foreground: '#1C1C1C', card: '#FFFFFF', muted: '#F1F1F1', mutedForeground: '#5A5A5A', popover: '#FFFFFF', primary: '#4F55D9', primaryForeground: '#FFFFFF', secondary: '#E9E9E9', accent: '#E7E8FB', border: '#C9C9C9', ring: '#4F55D9', destructive: '#C62835', destructiveForeground: '#FFFFFF', sidebarBackground: '#F5F5F5', userBubble: '#E2E3FA', },
  dark: { background: '#121212', foreground: '#F5F5F5', card: '#161616', muted: '#282828', mutedForeground: '#B8B8B8', popover: '#1F1F1F', primary: '#6B71F2', primaryForeground: '#121212', secondary: '#303030', accent: '#282B5C', border: '#555555', ring: '#9DA1FF', destructive: '#FF6B6B', destructiveForeground: '#121212', sidebarBackground: '#0E0E0E', userBubble: '#30315A', },
} satisfies Record<ColorMode, Palette>;

// Palettes and sRGB contrast helpers adapted from Hermes Agent (MIT, NOTICE.hermes).
// Semantic projection and independent appearance follow T3 (MIT, NOTICE.t3).
// See provenance.json for pinned revisions and source paths.
const presets: Record<Exclude<ColorTheme, 'minarbot'>, Record<ColorMode, Palette>> = {
  github: {
    light: { background: '#FFFFFF', foreground: '#1F2328', card: '#F6F8FA', muted: '#F6F6F6', mutedForeground: '#656D76', popover: '#FFFFFF', primary: '#196D31', primaryForeground: '#FFFFFF', secondary: '#DFEBE2', accent: '#E3EDE6', border: '#D0D7DE', ring: '#196D31', destructive: '#CF222E', destructiveForeground: '#FFFFFF', sidebarBackground: '#F6F8FA', userBubble: '#DBE7E2', },
    dark: { background: '#0D1117', foreground: '#E6EDF3', card: '#010409', muted: '#1A1E24', mutedForeground: '#7D8590', popover: '#161B22', primary: '#4F9E5E', primaryForeground: '#FFFFFF', secondary: '#1F382B', accent: '#192A24', border: '#30363D', ring: '#4F9E5E', destructive: '#F85149', destructiveForeground: '#FFFFFF', sidebarBackground: '#010409', userBubble: '#0F2018', },
  },
  catppuccin: {
    light: { background: '#EFF1F5', foreground: '#4C4F69', card: '#E6E9EF', muted: '#E8EBEF', mutedForeground: '#4C4F69', popover: '#E6E9EF', primary: '#6D2EBF', primaryForeground: '#FFFFFF', secondary: '#DDD6ED', accent: '#DFDAEF', border: '#ACB0BE', ring: '#6D2EBF', destructive: '#D20F39', destructiveForeground: '#FFFFFF', sidebarBackground: '#E6E9EF', userBubble: '#D7D3E9', },
    dark: { background: '#1E1E2E', foreground: '#CDD6F4', card: '#181825', muted: '#29293A', mutedForeground: '#CDD6F4', popover: '#181825', primary: '#CBA6F7', primaryForeground: '#1E1E2E', secondary: '#4E4466', accent: '#3D3652', border: '#585B70', ring: '#CBA6F7', destructive: '#F38BA8', destructiveForeground: '#1E1E2E', sidebarBackground: '#181825', userBubble: '#38324B', },
  },
  everforest: {
    light: { background: '#FDF6E3', foreground: '#5C6A72', card: '#FDF6E3', muted: '#F7F0DE', mutedForeground: '#68766D', popover: '#FDF6E3', primary: '#586B35', primaryForeground: '#FFFFFF', secondary: '#E6E3CB', accent: '#E9E5CE', border: '#C9C7B8', ring: '#586B35', destructive: '#C94D4D', destructiveForeground: '#FFFFFF', sidebarBackground: '#F7F0DE', userBubble: '#E9E5CE', },
    dark: { background: '#2D353B', foreground: '#D3C6AA', card: '#2D353B', muted: '#373E42', mutedForeground: '#A6AFA4', popover: '#2D353B', primary: '#A7C080', primaryForeground: '#2D353B', secondary: '#4F5C4E', accent: '#434E47', border: '#5B665F', ring: '#A7C080', destructive: '#DA6362', destructiveForeground: '#2D353B', sidebarBackground: '#252D32', userBubble: '#434E47', },
  },
  solarized: {
    light: { background: '#FDF6E3', foreground: '#1F1F1F', card: '#EEE8D5', muted: '#F4EDDB', mutedForeground: '#586E75', popover: '#EEE8D5', primary: '#675E34', primaryForeground: '#FFFFFF', secondary: '#E8E1CB', accent: '#EBE4CE', border: '#C8C0AA', ring: '#675E34', destructive: '#B23A48', destructiveForeground: '#FFFFFF', sidebarBackground: '#EEE8D5', userBubble: '#C6BEA7', },
    dark: { background: '#002B36', foreground: '#839496', card: '#002B36', muted: '#08313C', mutedForeground: '#93A1A1', popover: '#001F26', primary: '#6EA1C4', primaryForeground: '#002B36', secondary: '#1F4C5E', accent: '#144050', border: '#4B6870', ring: '#6EA1C4', destructive: '#E35957', destructiveForeground: '#002B36', sidebarBackground: '#001F26', userBubble: '#144050', },
  },
  midnight: {
    // Hermes ships only this dark palette. Light is an original neutral derivation.
    light: { background: '#F7F5FF', foreground: '#272443', card: '#FFFFFF', muted: '#EBE9F7', mutedForeground: '#5B5876', popover: '#FFFFFF', primary: '#5549AA', primaryForeground: '#FFFFFF', secondary: '#DEDAF5', accent: '#E7E2FA', border: '#C7C1E4', ring: '#5549AA', destructive: '#A32350', destructiveForeground: '#FFFFFF', sidebarBackground: '#F0EFFA', userBubble: '#E1DCF7', },
    dark: { background: '#08081C', foreground: '#DDD6FF', card: '#0D0D28', muted: '#13133A', mutedForeground: '#A5A1D2', popover: '#0F0F2E', primary: '#8B80E8', primaryForeground: '#08081C', secondary: '#1A1A4A', accent: '#1A1A44', border: '#4A4A7A', ring: '#8B80E8', destructive: '#D25A83', destructiveForeground: '#08081C', sidebarBackground: '#06061A', userBubble: '#14143A', },
  },
};

export const colorThemes: { id: ColorTheme; label: string }[] = [
  { id: 'minarbot', label: 'Minarbot' },
  { id: 'github', label: 'GitHub' },
  { id: 'catppuccin', label: 'Catppuccin' },
  { id: 'everforest', label: 'Everforest' },
  { id: 'solarized', label: 'Solarized' },
  { id: 'midnight', label: 'Midnight' },
];

const hex = (value: string) => value.replace('#', '').match(/../g)!.map(part => Number.parseInt(part, 16));
const color = (rgb: number[]) => `#${rgb.map(channel => Math.max(0, Math.min(255, Math.round(channel))).toString(16).padStart(2, '0')).join('')}`;
const luminance = (value: string) => hex(value).map(channel => channel / 255).map(channel => channel <= .03928 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4).reduce((sum, channel, index) => sum + channel * [.2126, .7152, .0722][index]!, 0);
const contrast = (a: string, b: string) => (Math.max(luminance(a), luminance(b)) + .05) / (Math.min(luminance(a), luminance(b)) + .05);
const mix = (value: string, target: string, amount: number) => color(hex(value).map((channel, index) => channel + (hex(target)[index]! - channel) * amount));
function readable(value: string, backgrounds: string[], minimum = 4.5) {
  if (backgrounds.every(background => contrast(value, background) >= minimum)) return value;
  const target = ['#000000', '#FFFFFF'].sort((a, b) => Math.min(...backgrounds.map(bg => contrast(b, bg))) - Math.min(...backgrounds.map(bg => contrast(a, bg))))[0]!;
  for (let amount = .05; amount <= 1; amount += .05) {
    const candidate = mix(value, target, amount);
    if (backgrounds.every(background => contrast(candidate, background) >= minimum)) return candidate;
  }
  return target;
}

export type ThemeTokenValues = Record<string, string>;
export function themeTokenValues(theme: ColorTheme, mode: ColorMode): ThemeTokenValues {
  const palette = theme === 'minarbot' ? minarbot[mode] : presets[theme][mode];
  const surfaces = [palette.background, palette.card, palette.sidebarBackground, palette.popover, palette.muted, palette.accent, palette.secondary, palette.userBubble,
    // Opaque equivalents of the translucent hover/selection overlays emitted below.
    ...[palette.background, palette.card, palette.sidebarBackground, palette.popover].map(base => mix(base, palette.foreground, 0x14 / 255))];
  const text = readable(palette.foreground, surfaces), secondary = readable(palette.mutedForeground, surfaces);
  const brand = palette.primary, brandText = readable(brand, surfaces), onBrand = readable(palette.primaryForeground, [brand]);
  const link = readable(mode === 'dark' ? '#8AB4F8' : '#185ABC', surfaces);
  const focus = readable(palette.ring, surfaces, 3);
  const danger = readable(palette.destructive, surfaces), warn = readable(mode === 'dark' ? '#F0B84B' : '#8A5A00', surfaces), success = readable(mode === 'dark' ? '#55D68A' : '#087443', surfaces);
  // Bot HQ uses translucent neutral overlays for hover, selection and fills; opaque accent tints read as heavy blocks.
  return {
    '--color-bg-chrome': palette.background, '--color-bg-sidebar': palette.sidebarBackground, '--color-bg-pane': palette.card, '--color-bg-elevated': palette.popover,
    '--color-bg-active': `${palette.foreground}14`, '--color-bg-tertiary': `${palette.foreground}0A`, '--color-bg-hover': `${palette.foreground}0F`, '--color-bg-secondary-hover': palette.secondary, '--color-bg-secondary-active': palette.accent,
    '--color-bg-elevated-hover': palette.secondary, '--color-bg-active-solid': palette.secondary, '--color-bg-subtle': palette.muted, '--color-bg-primary-disabled': palette.border,
    '--color-link': link, '--color-text-primary': text, '--color-text-secondary': secondary, '--color-text-tertiary': secondary, '--color-text-quaternary': secondary, '--color-icon-secondary': secondary,
    '--color-border-secondary': `${palette.foreground}${mode === 'dark' ? '1A' : '1F'}`, '--color-border-tertiary': `${palette.foreground}${mode === 'dark' ? '0F' : '14'}`, '--color-border-primary': readable(palette.border, surfaces, 3), '--color-border-focus': focus,
    '--color-primary': text, '--color-on-primary': readable(palette.background, [text]), '--color-bg-primary-hover': text, '--color-ink': text, '--color-brand': brand, '--color-brand-hover': brand, '--color-brand-text': brandText, '--color-brand-soft': palette.accent, '--color-focus-ring': palette.accent,
    '--color-bubble-agent': palette.muted, '--color-bubble-user': palette.userBubble, '--bot-bubble': brand, '--bot-action': brand, '--bot-action-hover': brand, '--bot-action-soft': palette.accent, '--bot-action-text': brandText, '--bot-on-action': onBrand,
    '--color-on-danger': readable(palette.destructiveForeground, [danger]), '--color-on-warn': readable(palette.primaryForeground, [warn]), '--color-danger': danger, '--color-danger-hover': danger, '--color-danger-text': danger, '--color-danger-bg': mix(palette.background, danger, .1), '--color-warn': warn, '--color-warn-text': warn, '--color-warn-bg': mix(palette.background, warn, .1), '--color-success': success, '--color-success-text': success, '--color-success-bg': mix(palette.background, success, .1),
    '--color-code-bg': palette.muted, '--color-code-fg': text,
  };
}

export function applyColorTheme(root: HTMLElement, theme: ColorTheme) {
  const document = root.ownerDocument;
  if (!document.getElementById('minarbot-color-themes')) {
    const style = document.createElement('style');
    style.id = 'minarbot-color-themes';
    style.nonce = document.querySelector('meta[name="csp-nonce"]')?.getAttribute('content') ?? '';
    style.textContent = themeStyles();
    document.head.append(style);
  }
  root.dataset.colorTheme = theme;
}

export function themeStyles() {
  return colorThemes.map(({ id }) => {
    const rule = (mode: ColorMode, preference: string) => `:root[data-color-theme='${id}'][data-theme='${preference}'], .theme-preview[data-color-theme='${id}'][data-theme='${preference}'] { ${Object.entries(themeTokenValues(id, mode)).map(([name, value]) => `${name}: ${value};`).join(' ')} }`;
    return `${rule('light', 'light')} ${rule('dark', 'dark')} ${rule('light', 'system')} @media (prefers-color-scheme: dark) { ${rule('dark', 'system')} }`;
  }).join('\n');
}
