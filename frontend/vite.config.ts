import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  base: './',
  plugins: [svelte()],
  server: {
    // Keep fetch, EventSource and WebSocket same-origin. The backend must also
    // accept the browser Origin when using this development proxy.
    proxy: {
      '/api': { target: process.env.HERDR_API_URL || 'http://127.0.0.1:8080', ws: true },
    },
  },
});
