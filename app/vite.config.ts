import { defineConfig } from 'vite';

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
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          const path = id.replaceAll('\\', '/');
          if (!path.includes('/node_modules/')) return;
          if (path.includes('/react/') || path.includes('/react-dom/')) return 'react-vendor';
          if (path.includes('/radix-ui/')) return 'radix-vendor';
          if (path.includes('/marked/') || path.includes('/entities/')) return 'markdown-vendor';
        },
      },
      onwarn(warning, warn) {
        // lucide-react ships React Server Component directives; they are meaningless in this client bundle.
        if (warning.code === 'MODULE_LEVEL_DIRECTIVE' && warning.message.includes('"use client"')) return;
        warn(warning);
      },
    },
  },
});
