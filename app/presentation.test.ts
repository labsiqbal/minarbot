import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { colorThemes, themeTokenValues } from '../crates/web/assets/ui/themes.ts';

test('all six color themes in light and dark keep text, status, code, viewer and control contrast', () => {
  const css = readFileSync(new URL('../crates/web/assets/ui/tokens.css', import.meta.url), 'utf8');
  const tokens = (selector: string) => {
    const block = css.slice(css.indexOf(selector));
    return Object.fromEntries([...block.slice(0, block.indexOf('}')).matchAll(/--([a-z0-9-]+):\s*(#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?);/g)].map(match => [match[1], match[2]]));
  };
  const light = tokens(':root {'), dark = { ...light, ...tokens(":root[data-theme='dark'] {") };
  const rgba = (color: string) => { const [r, g, b, a = 255] = color.slice(1).match(/../g)!.map(value => parseInt(value, 16)); return [r, g, b, a / 255]; };
  const over = (color: string, base: number[]) => { const [r, g, b, a] = rgba(color); return [r * a + base[0] * (1 - a), g * a + base[1] * (1 - a), b * a + base[2] * (1 - a)]; };
  const luminance = (rgb: number[]) => rgb.map(value => value / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
  // Translucent overlays (hover, selection, fills) sit on the theme's opaque surfaces, so measure them over each one.
  const opaque = ['color-bg-chrome', 'color-bg-pane', 'color-bg-sidebar', 'color-bg-elevated'];
  let palette: Record<string, string> = {};
  const ratio = (foreground: string, background: string) => Math.min(...(rgba(background)[3] < 1 ? opaque.map(name => over(palette[name], [255, 255, 255])) : [[255, 255, 255]]).map(under => {
    const base = over(background, under), a = luminance(over(foreground, base)), b = luminance(base);
    return (Math.max(a, b) + .05) / (Math.min(a, b) + .05);
  }));
  const surfaces = ['color-bg-chrome', 'color-bg-pane', 'color-bg-sidebar', 'color-bg-elevated', 'color-bg-tertiary', 'color-bg-active', 'color-bg-hover', 'color-bg-secondary-hover', 'color-bubble-user', 'color-code-bg'];
  for (const { id } of colorThemes) for (const mode of ['light', 'dark'] as const) {
    palette = { ...(mode === 'light' ? light : dark), ...Object.fromEntries(Object.entries(themeTokenValues(id, mode)).map(([name, value]) => [name.slice(2), value])) };
    const name = `${id}/${mode}`;
    for (const text of ['color-text-primary', 'color-text-secondary', 'color-text-tertiary', 'color-text-quaternary', 'color-icon-secondary', 'color-danger-text', 'color-warn-text', 'color-success-text', 'color-brand-text', 'color-link'])
      for (const surface of surfaces) assert(ratio(palette[text], palette[surface]) >= 4.5, `${name}: ${text} on ${surface}: ${ratio(palette[text], palette[surface])}`);
    for (const [text, surface] of [['color-on-primary', 'color-primary'], ['color-on-primary', 'color-bg-primary-hover'], ['bot-on-action', 'color-brand'], ['bot-on-action', 'color-brand-hover'], ['color-on-danger', 'color-danger'], ['color-on-warn', 'color-warn'], ['color-code-fg', 'color-code-bg'], ['color-danger-text', 'color-danger-bg'], ['color-warn-text', 'color-warn-bg'], ['color-success-text', 'color-success-bg'], ['color-text-secondary', 'color-danger-bg'], ['color-text-secondary', 'color-success-bg'], ['color-viewer-text', 'color-viewer-bg']])
      assert(ratio(palette[text], palette[surface]) >= 4.5, `${name}: ${text} on ${surface}: ${ratio(palette[text], palette[surface])}`);
    for (const surface of surfaces) for (const indicator of ['color-border-focus', 'color-border-primary']) assert(ratio(palette[indicator], palette[surface]) >= 3, `${name}: ${indicator} on ${surface}`);
  }
});
