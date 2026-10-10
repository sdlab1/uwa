import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const PROXY = {
  '/v1': 'http://127.0.0.1:8080',
  '/admin': 'http://127.0.0.1:8080',
  '/api': 'http://127.0.0.1:8080',
  '/healthz': 'http://127.0.0.1:8080',
  '/readyz': 'http://127.0.0.1:8080',
};

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    cssCodeSplit: false,
    rollupOptions: {
      input: 'src/main.ts',
      output: {
        entryFileNames: 'main.js',
        chunkFileNames: 'chunks/[name]-[hash].js',
        // The single CSS bundle must be main.css: index.html hardcodes
        // /static/dist/main.css.
        assetFileNames: 'main[extname]',
        format: 'es',
        inlineDynamicImports: true,
      },
    },
  },
  server: { port: 5173, proxy: PROXY },
});
