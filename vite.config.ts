import { defineConfig } from 'vite';

export default defineConfig({
  base: './',
  // Use Vite's TSX transform without React Refresh's inline script preamble.
  // This keeps the same strict script CSP in development and packaged builds.
  esbuild: { jsx: 'automatic' },
  server: { host: '127.0.0.1', port: 5173, strictPort: true },
  build: { outDir: 'dist', emptyOutDir: true },
});
