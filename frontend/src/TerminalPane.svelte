<script lang="ts">
  import { onMount, getContext } from 'svelte';
  import { reloadContext } from './lib/mainReload';
  import type { ReloadSafety } from './lib/mainReload';
  const reloadSafety = getContext<ReloadSafety | undefined>(reloadContext);
  import { transportSocket } from './lib/transport';
  import type { SocketLike } from './lib/transport';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { WebLinksAddon } from '@xterm/addon-web-links';
  import { openTerminalLink } from './lib/terminalLinks';
  import { terminalInput } from './lib/terminalInput';
  import type { InputSource } from './lib/terminalInput';
  import { TerminalSubmission } from './lib/terminalSubmission';
  import '@xterm/xterm/css/xterm.css';
  import { binaryInput, inputChunks, retryDelay, validSize } from './lib/input';
  import { record } from './lib/schema';
  import { terminalUrl, touchKeys } from './lib/terminal';
  import { terminalTheme } from './lib/theme';
  import { initializeAppearance, watchAppearance } from './lib/appearance';
  import { touchScroll } from './lib/touchScroll';
  import { initializeFontSize, watchFontSize } from './lib/fontSize';
  import { imageAccept, imageReference, uploadImage } from './lib/attachments';
  import { errorMessage } from './lib/api';
  import CommandPalette from './CommandPalette.svelte';
  import { isCommandText } from './lib/commands';
  import type { AgentKind } from './lib/commands';

  let { address, title, reported = 'unreported', active, onfocus, compact = false, showKeys = false, agent }: { address: string; title: string; reported?: string; active: boolean; onfocus: () => void; compact?: boolean; showKeys?: boolean; agent?: AgentKind } = $props();
  let host: HTMLDivElement;
  let status = $state('Connecting');
  let detail = $state('Connecting to the terminal.');
  let attached = $state(false);
  let reconnect: () => void = () => {};
  let focusTerminal: () => void = () => onfocus();
  let sendKey: (event: MouseEvent, data: string) => void = () => {};
  let prepareImageInsertion: (event: Event) => (path: string) => boolean = () => () => false;
  let imagePicker = $state<HTMLInputElement>();
  let uploadBusy = $state(false);
  let uploadStatus = $state('');
  let uploadError = $state('');
  let uploadController: AbortController | undefined;
  let paletteAgent = $state<AgentKind>();
  let connectionEpoch = $state(0);
  let paletteEpoch = $state(0);
  let openCommands: (event: MouseEvent) => void = () => {};
  let insertCommand = $state<(event: MouseEvent, text: string, run: boolean) => boolean>(() => false);
  async function chooseImage(file: File | undefined, event: Event) {
    if (!file || !event.isTrusted || uploadBusy || !attached) return;
    const insert = prepareImageInsertion(event);
    const finishUpload = reloadSafety?.busy();
    uploadController = new AbortController();
    uploadBusy = true; uploadError = ''; uploadStatus = `Uploading ${file.name}…`;
    try {
      const path = await uploadImage(address, file, uploadController.signal);
      if (uploadController.signal.aborted) return;
      if (!insert(path)) {
        uploadStatus = ''; uploadError = 'Image uploaded, but the terminal connection changed. Attach it again when connected.';
        return;
      }
      uploadStatus = 'Image reference inserted. Add your message, then send it in Pi.';
    } catch (e) {
      if (!uploadController.signal.aborted) { uploadError = errorMessage(e); uploadStatus = ''; }
    } finally { uploadBusy = false; finishUpload?.(); }
  }

  onMount(() => {
    let appearance = initializeAppearance();
    const term = new Terminal({
      cursorBlink: false, disableStdin: true, convertEol: false,
      fontFamily: '"Cascadia Code", "SFMono-Regular", Consolas, monospace',
      fontSize: initializeFontSize(), lineHeight: 1.15, scrollback: 5000,
      theme: terminalTheme(appearance.dark),
      allowProposedApi: false,
      linkHandler: { activate: openTerminalLink, allowNonHttpProtocols: false },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon(openTerminalLink));
    term.open(host);
    const submission = new TerminalSubmission();
    let socket: SocketLike | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    let generation = 0;
    let disposed = false;
    let automaticInput = false;
    let resizeNotifications = false;
    let raf = 0;
    let viewportBounds = '';
    const send = (value: unknown) => { if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value)); };
    const stopAppearance = watchAppearance(value => {
      const changed = appearance.dark !== value.dark;
      appearance = value;
      term.options.theme = terminalTheme(value.dark);
      if (attached && changed) send({ type: 'theme', theme: value.dark ? 'dark' : 'light' });
    });
    const stopFontSize = watchFontSize(size => {
      if (term.options.fontSize === size) return;
      term.options.fontSize = size;
      // Font metrics change usable rows/columns without changing the host box.
      // Propose the new grid through native ownership; do not reconnect or focus.
      viewportBounds = '';
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(resize);
    });
    const sendInput = (data: string, binary = false, type: InputSource = 'input') => {
      if (!attached || socket?.readyState !== WebSocket.OPEN) return;
      if (type === 'input' && !binary && !submission.allow(data)) return;
      if (type === 'input') {
        for (const chunk of inputChunks(data)) {
          if (binary) socket?.send(binaryInput(chunk)); else send({ type, data: chunk });
        }
      } else if (automaticInput) {
        for (const chunk of inputChunks(data)) send({ type: 'automatic', data: chunk });
      }
    };
    const inputSub = terminalInput(term, (data, source, binary) => sendInput(data, binary, source));
    term.attachCustomKeyEventHandler(event => { if (event.type === 'keydown') submission.markKey(event); return attached; });
    const scrolling = touchScroll(host, term, { attached: () => attached, mark: () => {} });
    // Capture mobile line-break and paste intent before xterm's handlers. A
    // compositionend, focus, redraw or resize is never submission permission.
    const beforeInput = (event: Event) => { if (event instanceof InputEvent) submission.markInput(event); };
    const paste = (event: ClipboardEvent) => submission.markPaste({ isTrusted: event.isTrusted, text: event.clipboardData?.getData('text/plain') || '' });
    host.addEventListener('beforeinput', beforeInput, true);
    host.addEventListener('paste', paste, true);
    focusTerminal = () => { onfocus(); if (attached) term.focus(); };
    const clickTerminal = (event: MouseEvent) => {
      // Link activation already belongs to this gesture; don't additionally
      // focus the textarea and summon the phone keyboard while navigating.
      if (event.defaultPrevented || host.querySelector('.xterm-cursor-pointer')) return;
      focusTerminal();
    };
    host.addEventListener('click', clickTerminal);
    sendKey = (event, data) => {
      if (!attached || !event.isTrusted) return;
      if (data === '\r') submission.markEnter(event);
      sendInput(data); focusTerminal();
    };
    prepareImageInsertion = event => {
      const trusted = event.isTrusted, current = generation, target = socket;
      return path => {
        // Upload completion is not a new gesture. Never queue across a reconnect,
        // insert into a replacement pane, or summon the keyboard asynchronously.
        if (!trusted || disposed || !attached || generation !== current || socket !== target || target?.readyState !== WebSocket.OPEN) return false;
        const reference = imageReference(path);
        submission.markPaste({ isTrusted: trusted, text: reference });
        term.paste(reference); // Same trusted file selection; no Enter or focus.
        return true;
      };
    };
    let commandTarget: { generation: number; socket: SocketLike; agent: AgentKind } | undefined;
    openCommands = event => {
      if (!event.isTrusted || !attached || !agent || socket?.readyState !== WebSocket.OPEN) return;
      commandTarget = { generation, socket, agent };
      paletteEpoch = connectionEpoch;
      paletteAgent = agent;
    };
    insertCommand = (event, text, run) => {
      if (!event.isTrusted || !isCommandText(text) || disposed || !attached || !commandTarget || commandTarget.generation !== generation || commandTarget.socket !== socket || socket?.readyState !== WebSocket.OPEN || commandTarget.agent !== agent) return false;
      // Never clear an existing draft or emulate an exclusive control lock.
      // Run is an explicit user action; ordinary Insert never sends Enter.
      submission.markPaste({ isTrusted: event.isTrusted, text });
      term.paste(text);
      if (run) { submission.markEnter(event); sendInput('\r'); }
      focusTerminal();
      return true;
    };
    const focused = () => { onfocus(); if (attached) send({ type: 'focus', focused: true }); };
    const blurred = () => { submission.clear(); if (attached) send({ type: 'focus', focused: false }); };
    host.addEventListener('focusin', focused);
    host.addEventListener('focusout', blurred);
    function resize() {
      if (!attached || host.clientWidth < 20 || host.clientHeight < 20) return;
      const box = host.getBoundingClientRect();
      const bounds = `${box.width}x${box.height}`;
      // Scrollbars and remote grid changes must not re-promote this viewer.
      // Only a real change to our pane geometry proposes another viewport.
      if (bounds === viewportBounds) return;
      const size = fit.proposeDimensions();
      if (!size || !validSize(size.cols, size.rows)) return;
      viewportBounds = bounds;
      // Core arbitrates the applicable client size. Render its authoritative
      // notifications rather than locally fighting another client's viewport.
      send({ type: 'resize', cols: size.cols, rows: size.rows });
      if (!resizeNotifications) term.resize(size.cols, size.rows);
    }
    const observer = new ResizeObserver(() => { cancelAnimationFrame(raf); raf = requestAnimationFrame(resize); });
    observer.observe(host);
    function connect() {
      clearTimeout(timer);
      const current = ++generation;
      connectionEpoch = current;
      socket?.close();
      submission.clear(); scrolling.reset();
      attached = false;
      automaticInput = false;
      resizeNotifications = false;
      viewportBounds = '';
      term.options.disableStdin = true;
      term.reset();
      term.clear();
      status = 'Connecting';
      detail = 'Connecting to the terminal.';
      const size = fit.proposeDimensions();
      const url = terminalUrl(address, size?.cols || term.cols, size?.rows || term.rows, location.href, appearance.dark ? 'dark' : 'light');
      const ws = transportSocket(url);
      socket = ws;
      ws.binaryType = 'arraybuffer';
      let final = false;
      ws.onmessage = event => {
        if (disposed || generation !== current) return;
        if (event.data instanceof ArrayBuffer) {
          if (!attached) return;
          const bytes = new Uint8Array(event.data);
          // Only parsed bytes replenish credit, and never on a newer socket.
          term.write(bytes, () => { if (!disposed && generation === current && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'credit', bytes: bytes.byteLength })); });
          return;
        }
        try {
          const msg: unknown = JSON.parse(event.data);
          if (!record(msg)) throw new Error('Invalid terminal message.');
          if (msg.type === 'attached') {
            if (msg.observe !== false) throw new Error('The server did not open an interactive terminal.');
            attached = true;
            automaticInput = msg.automatic_input === true;
            resizeNotifications = msg.resize_notifications === true;
            term.options.disableStdin = false;
            const terminal = record(msg.terminal) ? msg.terminal : {};
            if (validSize(terminal.cols, terminal.rows)) term.resize(terminal.cols, terminal.rows as number);
            status = 'Connected';
            detail = '';
            attempt = 0;
            send({ type: 'credit', bytes: 262144 });
            send({ type: 'theme', theme: appearance.dark ? 'dark' : 'light' });
            resize(); // Do not summon a phone keyboard or steal focus on attach.
          } else if (msg.type === 'resize') {
            if (validSize(msg.cols, msg.rows)) term.resize(msg.cols, msg.rows as number);
          } else if (msg.type === 'closed' || msg.type === 'error') {
            final = msg.type === 'closed';
            attached = false;
            term.options.disableStdin = true;
            status = msg.type === 'closed' ? 'Closed' : 'Terminal error';
            detail = typeof msg.message === 'string' ? msg.message : 'The terminal stream ended.';
            ws.close();
          }
        } catch (error) {
          final = true; attached = false; term.options.disableStdin = true;
          status = 'Terminal error'; detail = error instanceof Error ? error.message : 'Unreadable terminal message.';
          ws.close();
        }
      };
      ws.onerror = () => { if (generation === current && !final) detail = 'The terminal connection failed. Check your connection or sign in again.'; };
      ws.onclose = () => {
        if (disposed || generation !== current) return;
        attached = false; submission.clear(); scrolling.reset(); term.options.disableStdin = true;
        if (final) return;
        const delay = retryDelay(attempt++);
        status = 'Disconnected';
        detail = `Reconnecting in ${Math.round(delay / 1000)}s… Input is paused until connected.`;
        timer = setTimeout(connect, delay);
      };
    }
    reconnect = () => { attempt = 0; connect(); };
    connect();
    return () => {
      disposed = true; generation++; uploadController?.abort(); clearTimeout(timer); cancelAnimationFrame(raf);
      observer.disconnect(); socket?.close(); submission.clear(); scrolling.dispose(); inputSub.dispose();
      host.removeEventListener('beforeinput', beforeInput, true); host.removeEventListener('paste', paste, true);
      host.removeEventListener('click', clickTerminal);
      host.removeEventListener('focusin', focused); host.removeEventListener('focusout', blurred);
      stopAppearance(); stopFontSize();
      term.dispose();
    };
  });
</script>

<section class:focused={active} class:compact class="terminal-pane" aria-label={title}>
  {#if !compact}<header class="terminal-header">
    <button class="pane-name" onclick={() => focusTerminal()} title="Focus terminal and open keyboard"><span class:live={attached} class="dot"></span>{title}</button>
    <span class={`reported-state state-${reported}`} role="status" title="Status reported by the terminal"><span class="dot"></span>{reported === 'unreported' ? 'No reported status' : reported}</span>
    <span class="terminal-status" class:connected={attached} role="status">{status}</span>
    {#if !attached}<button class="quiet" onclick={() => reconnect()}>Reconnect</button>{/if}
  </header>{/if}
  <div class="terminal-host" bind:this={host}></div>
  {#if showKeys}<div class="terminal-accessories"><nav class="terminal-keys" aria-label="Terminal keyboard shortcuts">
    {#each touchKeys as key (key.name)}
      <button aria-label={key.name} disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => sendKey(event, key.data)}>{key.label}</button>
    {/each}
  </nav>
    <div class="terminal-attachments">
      <input class="image-picker" bind:this={imagePicker} type="file" accept={imageAccept} aria-label="Choose image to attach"
        onchange={event => { void chooseImage(event.currentTarget.files?.[0], event); event.currentTarget.value = ''; }} />
      <span class:error={!!uploadError} class="attachment-status" role={uploadError ? 'alert' : 'status'}>{uploadError || uploadStatus}</span>
      {#if agent}<button class="agent-commands-button" aria-label="Agent commands" title={`${agent} commands and favorites`} aria-haspopup="dialog" disabled={!attached} onclick={event => openCommands(event)}>/</button>{/if}
      <button class="attach-image" aria-label={uploadBusy ? 'Uploading image' : 'Attach image'} aria-busy={uploadBusy} disabled={!attached || uploadBusy} onclick={() => imagePicker?.click()} title="Attach image · PNG, JPEG, GIF or WebP · up to 8 MiB">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="m8 13 6-6a3 3 0 0 1 4 4l-8 8a5 5 0 0 1-7-7l9-9a7 7 0 0 1 10 10l-9 9"/></svg>
      </button>
    </div>
  </div>{/if}
  {#if !attached}<footer class="terminal-detail" role="status">{detail}{#if compact}<button class="quiet" onclick={() => reconnect()}>Reconnect</button>{/if}</footer>{/if}
</section>
{#if paletteAgent}<CommandPalette {address} agent={paletteAgent} connected={attached && agent === paletteAgent && connectionEpoch === paletteEpoch} close={() => paletteAgent = undefined} insert={insertCommand} />{/if}
