<script lang="ts">
  import { onMount } from 'svelte';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import '@xterm/xterm/css/xterm.css';
  import { InputGate, binaryInput, inputChunks, retryDelay, validSize } from './lib/input';
  import { record } from './lib/schema';
  import { terminalUrl, touchKeys } from './lib/terminal';
  import { terminalTheme } from './lib/theme';

  let { address, title, reported = 'unreported', active, onfocus, compact = false, showKeys = false }: { address: string; title: string; reported?: string; active: boolean; onfocus: () => void; compact?: boolean; showKeys?: boolean } = $props();
  let host: HTMLDivElement;
  let status = $state('Connecting');
  let detail = $state('Connecting to the terminal.');
  let attached = $state(false);
  let reconnect: () => void = () => {};
  let focusTerminal: () => void = () => onfocus();
  let sendKey: (event: MouseEvent, data: string) => void = () => {};

  onMount(() => {
    const appearance = matchMedia('(prefers-color-scheme: dark)');
    const term = new Terminal({
      cursorBlink: false, disableStdin: true, convertEol: false,
      fontFamily: '"Cascadia Code", "SFMono-Regular", Consolas, monospace',
      fontSize: 13, lineHeight: 1.15, scrollback: 5000,
      theme: terminalTheme(appearance.matches),
      allowProposedApi: false,
    });
    const updateTheme = () => { term.options.theme = terminalTheme(appearance.matches); };
    appearance.addEventListener('change', updateTheme);
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    const gate = new InputGate();
    let socket: WebSocket | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    let generation = 0;
    let disposed = false;
    let automaticInput = false;
    let resizeNotifications = false;
    let raf = 0;
    let viewportBounds = '';
    const send = (value: unknown) => { if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value)); };
    const sendInput = (data: string, binary = false) => {
      if (!attached) return;
      const type = gate.classify(data);
      if (type === 'input') {
        for (const chunk of inputChunks(data)) {
          if (binary) socket?.send(binaryInput(chunk)); else send({ type, data: chunk });
        }
      } else if (automaticInput) {
        for (const chunk of inputChunks(data)) send({ type: 'automatic', data: chunk });
      }
    };
    const dataSub = term.onData(data => sendInput(data));
    const binarySub = term.onBinary(data => sendInput(data, true));
    term.attachCustomKeyEventHandler(event => { gate.mark(event); return attached; });
    const mark = (event: Event) => gate.mark(event);
    // Capture precedes xterm's DOM handlers, including mobile composition/paste.
    host.addEventListener('keydown', mark, true);
    host.addEventListener('beforeinput', mark, true);
    const paste = (event: ClipboardEvent) => gate.markPaste({ isTrusted: event.isTrusted, text: event.clipboardData?.getData('text/plain') || '' });
    const input = (event: Event) => { if (event instanceof InputEvent) gate.markInput(event); };
    host.addEventListener('paste', paste, true);
    host.addEventListener('input', input, true);
    const composition = (event: CompositionEvent) => gate.markComposition(event);
    host.addEventListener('compositionend', composition, true);
    focusTerminal = () => { onfocus(); if (attached) term.focus(); };
    host.addEventListener('click', focusTerminal);
    sendKey = (event, data) => {
      if (!attached || !event.isTrusted) return;
      gate.mark(event); sendInput(data); focusTerminal();
    };
    const focused = () => { onfocus(); if (attached) send({ type: 'focus', focused: true }); };
    const blurred = () => { gate.clear(); if (attached) send({ type: 'focus', focused: false }); };
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
      socket?.close();
      gate.clear();
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
      const url = terminalUrl(address, size?.cols || term.cols, size?.rows || term.rows, location.href, appearance.matches ? 'dark' : 'light');
      const ws = new WebSocket(url);
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
        attached = false; gate.clear(); term.options.disableStdin = true;
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
      disposed = true; generation++; clearTimeout(timer); cancelAnimationFrame(raf);
      observer.disconnect(); socket?.close(); gate.clear(); dataSub.dispose(); binarySub.dispose();
      host.removeEventListener('keydown', mark, true); host.removeEventListener('beforeinput', mark, true); host.removeEventListener('paste', paste, true);
      host.removeEventListener('input', input, true); host.removeEventListener('compositionend', composition, true);
      host.removeEventListener('click', focusTerminal);
      host.removeEventListener('focusin', focused); host.removeEventListener('focusout', blurred);
      appearance.removeEventListener('change', updateTheme);
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
  {#if !compact || showKeys}<nav class="terminal-keys" aria-label="Terminal keyboard shortcuts">
    {#each touchKeys as key (key.name)}
      <button aria-label={key.name} disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => sendKey(event, key.data)}>{key.label}</button>
    {/each}
  </nav>{/if}
  {#if !attached}<footer class="terminal-detail" role="status">{detail}{#if compact}<button class="quiet" onclick={() => reconnect()}>Reconnect</button>{/if}</footer>{/if}
</section>
