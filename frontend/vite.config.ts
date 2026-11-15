import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { readFileSync } from 'node:fs';
import { sourceBuild } from './buildInfo.ts';
const source = sourceBuild(process.env);
const appVersion = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version;

export default defineConfig({
  base: './',
  define: {
    __HERDR_SOURCE_REVISION__: JSON.stringify(source.revision),
    __HERDR_SOURCE_COMMIT_DATE__: JSON.stringify(source.commitDate),
    __HERDR_SOURCE_DIRTY__: JSON.stringify(source.dirty),
    __HERDR_APP_VERSION__: JSON.stringify(appVersion),
  },
  plugins: [svelte()],
  server: {
    // Keep fetch, EventSource and WebSocket same-origin. The backend must also
    // accept the browser Origin when using this development proxy.
    proxy: {
      '/api': { target: process.env.HERDR_API_URL || 'http://127.0.0.1:8080', ws: true },
    },
  },
});
