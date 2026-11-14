import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  base: './',
  define: { __HERDR_SOURCE_REVISION__: JSON.stringify(/^[a-f0-9]{40}$/.test(process.env.HERDR_BUILD_REVISION || '') ? process.env.HERDR_BUILD_REVISION : '') },
  plugins: [svelte()],
  server: {
    // Keep fetch, EventSource and WebSocket same-origin. The backend must also
    // accept the browser Origin when using this development proxy.
    proxy: {
      '/api': { target: process.env.HERDR_API_URL || 'http://127.0.0.1:8080', ws: true },
    },
  },
});
