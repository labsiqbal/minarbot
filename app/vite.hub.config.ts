import { defineConfig, mergeConfig } from 'vite';
import app from './vite.config';

// Keep density/style order independent of the local-owner application.
export default defineConfig(mergeConfig(app, {
  build: { outDir: 'dist/hub', manifest: true, rollupOptions: { input: 'hub.html' } },
}));
