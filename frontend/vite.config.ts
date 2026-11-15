import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { sourceBuild } from './buildInfo.ts';
const source = sourceBuild(process.env);
const appVersion = source.version;

// The dev listener is public, but API access remains same-origin and authenticated.
const devOrigin = new URL(process.env.HERDR_DEV_ORIGIN || 'http://localhost:9001');
const apiTarget = new URL(process.env.HERDR_API_URL || 'http://127.0.0.1:9002');
if (apiTarget.protocol !== 'http:' || apiTarget.hostname !== '127.0.0.1'
    || apiTarget.username || apiTarget.password || apiTarget.pathname !== '/'
    || apiTarget.search || apiTarget.hash) {
  throw new Error('HERDR_API_URL must be an HTTP loopback backend origin');
}
if (devOrigin.protocol !== 'http:' || devOrigin.username || devOrigin.password
    || devOrigin.pathname !== '/' || devOrigin.search || devOrigin.hash) {
  throw new Error('HERDR_DEV_ORIGIN must be an HTTP origin');
}

export default defineConfig({
  base: './',
  define: {
    __HERDR_SOURCE_REVISION__: JSON.stringify(source.revision),
    __HERDR_SOURCE_COMMIT_DATE__: JSON.stringify(source.commitDate),
    __HERDR_SOURCE_DIRTY__: JSON.stringify(source.dirty),
    __HERDR_APP_VERSION__: JSON.stringify(appVersion),
    __HERDR_RELEASE_VERSION__: JSON.stringify(source.releaseVersion),
  },
  plugins: [svelte()],
  server: {
    host: '0.0.0.0',
    port: Number(process.env.HERDR_DEV_PORT || 9001),
    strictPort: true,
    allowedHosts: [devOrigin.hostname],
    cors: false,
    hmr: { host: devOrigin.hostname, clientPort: Number(devOrigin.port || 80), protocol: 'ws' },
    // http-proxy streams SSE and WebSocket data without response buffering.
    // Preserve the browser Origin, cookies and auth headers: never rewrite them
    // to make a foreign origin appear trusted by the backend.
    proxy: {
      '/api': { target: apiTarget.origin, ws: true, changeOrigin: false, rewriteWsOrigin: false },
    },
  },
});
