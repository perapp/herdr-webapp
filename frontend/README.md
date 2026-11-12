# Herdr browser frontend

Browser-native Svelte 5 + TypeScript, Vite and xterm.js. Node 24 recommended (minimum 22.12).

```sh
npm ci
npm run check
npm run lint
npm test
npm run build
```

The backend serves `dist/`. All API requests and terminal sockets are same-origin; no access tokens are stored. A `#token=…` fragment is removed synchronously before login. Session cookies are managed by the backend.

For separate development, set `HERDR_API_URL` to the backend URL and run `npm run dev -- --port 9002`. Vite proxies `/api` including WebSockets without rewriting the browser Origin; configure the backend's allowed public origin appropriately. Do not bypass Origin validation.

## Behavior

- Machine documents follow Rust mirror/layout serialization (flattened block/row/column/stack nodes). Unsupported mods/context/agent/floating panels are marked unavailable and never executed. There are no guessed conversation transcripts or file uploads.
- Explicit names win. Unnamed spaces prefer the desktop's published `labels.where` checkout/worktree name, then the first terminal's foreground folder, then the launch folder. Unnamed tabs use their position, as on desktop.
- Browser selection, stack selection, focus and zoom use localStorage only. Shared changes go exclusively through versioned `/api/topology`; no document is rewritten for navigation.
- Terminals attach interactively, including automatic reconnects, without Take/Release control steps. Disconnected input is disabled and never replayed. Unmounted panes and sign-out close their streams.
- Desktop uses native-style compact chrome and recursive multi-pane layouts. Mobile mounts only one selected pane below a 44 px bar; its modal burger drawer navigates computers → workspaces → tabs → panes, including inactive stack children. Terminal accessory keys are available on demand.
- Output is raw VT bytes; the browser grants 262144 bytes after attachment and acknowledges each chunk only after xterm's write callback. Core resize notifications are authoritative; only genuine pane geometry changes propose another viewport. Light/dark terminal colors follow system appearance and are advertised to core.
- xterm input defaults to `automatic`. Trusted key events or matching trusted input/paste/IME payloads identify intentional input, including across microtask boundaries and asynchronous composition completion. Parser replies never receive a blanket timed permission. Large input is chunked without splitting Unicode pairs.

## Dependency security

Vite 8 / Svelte plugin 7 and Vitest 5 replace vulnerable older tooling. A scoped minimatch override uses the patched 11.0.1+ API used by typescript-estree; remove the override when upstream's range is patched. Run `npm audit` for the current registry report. Any unpatched development-only advisories must be reported, not silently suppressed.

Unit tests cover schema defaults, recursive layout limits, malformed documents, desktop naming, plain-text label safety, user/automatic classification, Unicode paste chunking, dimensions and retry backoff. Real browser visual/terminal ownership integration testing is a separate verification step.
