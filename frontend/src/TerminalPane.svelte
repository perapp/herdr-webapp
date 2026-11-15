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
  import { ctrlJKey, terminalUrl, touchKeys } from './lib/terminal';
  import { terminalTheme } from './lib/theme';
  import { initializeAppearance, watchAppearance } from './lib/appearance';
  import { touchScroll } from './lib/touchScroll';
  import { initializeFontSize, watchFontSize } from './lib/fontSize';
  import { imageAccept, imageReference, uploadImage } from './lib/attachments';
  import { errorMessage } from './lib/api';
  import AgentIndicator from './AgentIndicator.svelte';
  import ProgramIcon from './ProgramIcon.svelte';
  import type { ProgramAppearance } from './lib/programs';
  import CommandPalette from './CommandPalette.svelte';
  import { isCommandText } from './lib/commands';
  import type { AgentKind } from './lib/commands';
  import { applyModifiers, noModifiers } from './lib/modifiers';
  import { screenText } from './lib/terminalText';
  import { canReadClipboard, writeClipboard } from './lib/clipboard';

  let { address, title, reported = 'unreported', active, onfocus, compact = false, showKeys = false, visible = true, holdGrid = false, agent, program }: { address: string; title: string; reported?: string; active: boolean; onfocus: () => void; compact?: boolean; showKeys?: boolean; visible?: boolean; holdGrid?: boolean; agent?: AgentKind; program?: ProgramAppearance } = $props();
  let host: HTMLDivElement;
  let status = $state('Connecting');
  let detail = $state('Connecting to the terminal.');
  let attached = $state(false);
  let reconnect: () => void = () => {};
  let focusTerminal: () => void = () => onfocus();
  let sendKey: (event: MouseEvent, data: string) => void = () => {};
  let prepareInsertion: (event: Event) => (text: string) => boolean = () => () => false;
  let copyScreen: (event: MouseEvent) => Promise<void> = async () => {};
  let jumpBottom: () => void = () => {};
  // A phone pane kept mounted while another is shown: still attached and
  // receiving output, but not laid out. See lib/keptPanes.ts.
  let conceal: () => void = () => {};
  let reveal: () => void = () => {};
  $effect(() => { if (visible) reveal(); else conceal(); });
  // While the on-screen keyboard is up the grid keeps its full size: a resize
  // makes full-screen programs redraw and reflow, and agents that redraw in
  // place leave duplicated text in history. The pane shows the part of the
  // unchanged grid that holds the cursor instead.
  let held = $state(false);
  let regrid: () => void = () => {};
  $effect(() => { void holdGrid; regrid(); });
  // Latched Ctrl/Alt apply to the next key only, then release.
  let mods = $state(noModifiers());
  let scrolledUp = $state(false);
  let canPaste = $state(false);
  // Where a long-press selection ended, relative to the stage: the action bar anchors there.
  let selectionAt = $state<{ left: number; top: number }>();
  let stage = $state<HTMLDivElement>();
  let clearSelection: () => void = () => {};
  let repeatTimer: ReturnType<typeof setTimeout> | undefined;
  let repeated = false;
  function stopRepeat() { clearTimeout(repeatTimer); repeatTimer = undefined; }
  /** Held arrow keys repeat like hardware keys. The originating trusted press
   * authorizes each repeat; release, cancel, leave or disconnect stops it.
   */
  function pressKey(event: PointerEvent, key: { data: string; repeat?: boolean }) {
    event.preventDefault(); stopRepeat(); repeated = false;
    if (!key.repeat || !event.isTrusted) return;
    const tick = () => {
      if (!attached) { stopRepeat(); return; }
      repeated = true; sendKey(event, key.data);
      repeatTimer = setTimeout(tick, 70);
    };
    repeatTimer = setTimeout(tick, 400);
  }
  function releaseKey(event: MouseEvent, data: string) {
    stopRepeat();
    // The click ending a repeating hold already sent its keys.
    if (repeated) { repeated = false; return; }
    sendKey(event, data);
  }
  function toggleModifier(event: MouseEvent, key: 'ctrl' | 'alt') {
    if (!event.isTrusted || !attached) return;
    mods[key] = !mods[key];
    focusTerminal();
  }
  async function pasteClipboard(event: MouseEvent) {
    if (!event.isTrusted || !attached) return;
    const insert = prepareInsertion(event);
    try {
      const text = await navigator.clipboard.readText();
      uploadStatus = ''; uploadError = '';
      if (!text) uploadStatus = 'The clipboard has no text.';
      else if (!insert(text)) uploadError = 'The terminal connection changed before the paste. Try again.';
    } catch { uploadStatus = ''; uploadError = 'Clipboard access was denied. Allow it for this site, or paste from the keyboard.'; }
  }
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
    const insert = prepareInsertion(event);
    const finishUpload = reloadSafety?.busy();
    uploadController = new AbortController();
    uploadBusy = true; uploadError = ''; uploadStatus = `Uploading ${file.name}…`;
    try {
      const path = await uploadImage(address, file, uploadController.signal);
      if (uploadController.signal.aborted) return;
      if (!insert(imageReference(path))) {
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
    // GPU drawing: scrolling redraws every visible row, which the default DOM
    // renderer does by rebuilding elements on the main thread. Any failure, or
    // the browser reclaiming the context, falls back to that renderer.
    // Loaded on demand: the add-on needs browser globals at import time.
    let unmounted = false;
    void import('@xterm/addon-webgl').then(({ WebglAddon }) => {
      if (unmounted) return;
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    }).catch(() => { /* No WebGL2 here: the DOM renderer remains. */ });
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
    const modified = (data: string) => {
      if (!mods.ctrl && !mods.alt) return data;
      // One key consumes the latch. A latched Ctrl+M/Ctrl+J is still a line
      // ending and stays subject to the submission guard: use Enter or Ctrl J.
      const next = applyModifiers(data, mods);
      mods = noModifiers();
      return next;
    };
    const inputSub = terminalInput(term, (data, source, binary) => sendInput(source === 'input' && !binary ? modified(data) : data, binary, source));
    let fullHeight = 0, placing = 0;
    const xterm = host.querySelector<HTMLElement>('.xterm');
    /** Slide the full-height grid so the cursor row stays in the visible part,
     * preferring the bottom of the screen where prompts and editors live.
     */
    const place = () => {
      placing = 0;
      if (!xterm) return;
      if (!held) { xterm.style.height = ''; xterm.style.transform = ''; return; }
      const hidden = Math.max(0, fullHeight - host.clientHeight);
      const buffer = term.buffer.active;
      const rowHeight = (host.querySelector('.xterm-screen')?.getBoundingClientRect().height || fullHeight) / term.rows;
      // Reading history: the cursor is off screen, so keep the newest rows.
      const cursorTop = buffer.viewportY < buffer.baseY ? hidden : Math.max(0, buffer.cursorY - 1) * rowHeight;
      xterm.style.height = `${fullHeight}px`;
      xterm.style.transform = `translateY(${-Math.round(Math.min(hidden, cursorTop))}px)`;
    };
    const schedulePlace = () => { if (held && !placing) placing = requestAnimationFrame(place); };
    const cursorSub = term.onCursorMove(schedulePlace);
    regrid = () => { cancelAnimationFrame(raf); raf = requestAnimationFrame(resize); };
    const updateScrolled = () => { scrolledUp = term.buffer.active.viewportY < term.buffer.active.baseY; schedulePlace(); };
    const scrollSub = term.onScroll(updateScrolled);
    const viewport = host.querySelector('.xterm-viewport');
    viewport?.addEventListener('scroll', updateScrolled, { passive: true });
    jumpBottom = () => { term.scrollToBottom(); updateScrolled(); };
    let parked: { bottom: boolean; line: number } | undefined;
    conceal = () => {
      const buffer = term.buffer.active;
      parked ||= { bottom: buffer.viewportY >= buffer.baseY, line: buffer.viewportY };
      stopRepeat(); scrolling.reset();
    };
    reveal = () => {
      const was = parked; parked = undefined;
      if (!was) return;
      // Showing a pane proposes this viewport again, exactly as a fresh attach
      // would, and restores where the reader was; the grid may have changed.
      viewportBounds = '';
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        resize();
        if (was.bottom) term.scrollToBottom(); else term.scrollToLine(was.line);
        term.refresh(0, term.rows - 1); updateScrolled();
      });
    };
    canPaste = canReadClipboard();
    copyScreen = async event => {
      if (!event.isTrusted) return;
      const selection = term.getSelection();
      const text = selection || screenText(term.buffer.active, term.rows);
      uploadStatus = ''; uploadError = '';
      if (!text) { uploadStatus = 'Nothing on screen to copy.'; return; }
      try { await writeClipboard(text); uploadStatus = selection ? 'Selection copied.' : 'Visible screen copied.'; }
      catch { uploadError = 'The browser blocked copying. Tap the terminal first, then try Copy again.'; }
    };
    term.attachCustomKeyEventHandler(event => { if (event.type === 'keydown') submission.markKey(event); return attached; });
    const scrolling = touchScroll(host, term, { attached: () => attached, mark: () => {}, selected: at => {
      const box = stage?.getBoundingClientRect();
      if (!box || !term.hasSelection()) return;
      selectionAt = { left: Math.max(84, Math.min(box.width - 84, at.x - box.left)), top: Math.max(4, at.y - box.top - 64) };
    } });
    const selectionSub = term.onSelectionChange(() => { if (!term.hasSelection()) selectionAt = undefined; });
    clearSelection = () => { term.clearSelection(); selectionAt = undefined; };
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
      const bytes = modified(data);
      submission.markShortcut(event, bytes);
      sendInput(bytes); focusTerminal();
    };
    prepareInsertion = event => {
      const trusted = event.isTrusted, current = generation, target = socket;
      return text => {
        // An upload or clipboard read completing is not a new gesture. Never queue
        // across a reconnect, insert into a replacement pane, or summon the keyboard.
        if (!trusted || disposed || !attached || generation !== current || socket !== target || target?.readyState !== WebSocket.OPEN) return false;
        submission.markPaste({ isTrusted: trusted, text });
        term.paste(text); // Same trusted gesture; no Enter or focus.
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
      // The keyboard took height from an established grid: keep the grid.
      if (holdGrid && fullHeight > 0 && host.clientHeight < fullHeight) { held = true; cancelAnimationFrame(placing); place(); return; }
      if (held) { held = false; cancelAnimationFrame(placing); place(); }
      const box = host.getBoundingClientRect();
      const bounds = `${box.width}x${box.height}`;
      // Scrollbars and remote grid changes must not re-promote this viewer.
      // Only a real change to our pane geometry proposes another viewport.
      if (bounds === viewportBounds) return;
      const size = fit.proposeDimensions();
      if (!size || !validSize(size.cols, size.rows)) return;
      viewportBounds = bounds; fullHeight = host.clientHeight;
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
      submission.clear(); scrolling.reset(); stopRepeat();
      attached = false; mods = noModifiers(); scrolledUp = false;
      automaticInput = false;
      resizeNotifications = false;
      viewportBounds = '';
      term.options.disableStdin = true;
      term.reset();
      term.clear();
      status = 'Connecting';
      detail = 'Connecting to the terminal.';
      // A reconnect under the keyboard attaches with the held grid, not the visible part.
      const size = held ? undefined : fit.proposeDimensions();
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
      unmounted = true; stopRepeat(); scrollSub.dispose(); cursorSub.dispose(); selectionSub.dispose(); cancelAnimationFrame(placing); viewport?.removeEventListener('scroll', updateScrolled);
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
    <button class="pane-name" onclick={() => focusTerminal()} title="Focus terminal and open keyboard">{#if program}<ProgramIcon {program} />{/if}{title}</button>
    <span class="reported-state"><AgentIndicator status={reported} showIdle={false} /></span>
    <span class="terminal-status" class:connected={attached} role="status">{status}</span>
    {#if !attached}<button class="quiet" onclick={() => reconnect()}>Reconnect</button>{/if}
  </header>{/if}
  <div class="terminal-stage" bind:this={stage}>
    {#if selectionAt}<div class="selection-actions" role="toolbar" aria-label="Selection" style:left={`${selectionAt.left}px`} style:top={`${selectionAt.top}px`}>
      <button onpointerdown={event => event.preventDefault()} onclick={event => { void copyScreen(event).then(() => clearSelection()); }}>Copy</button>
      <button aria-label="Clear selection" onpointerdown={event => event.preventDefault()} onclick={() => clearSelection()}>×</button>
    </div>{/if}
    <div class="terminal-host" bind:this={host}></div>
    {#if scrolledUp}<button class="jump-bottom" aria-label="Jump to latest output" title="Jump to latest output" onpointerdown={event => event.preventDefault()} onclick={() => jumpBottom()}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 4v15 M5 13l7 7 7-7"/></svg>
    </button>{/if}
  </div>
  {#if showKeys}<div class="terminal-accessories"><nav class="terminal-keys" aria-label="Terminal keyboard shortcuts">
    {#each touchKeys.slice(0, 2) as key (key.name)}{@render accessoryKey(key)}{/each}
    <button class="modifier-key" aria-label="Control modifier" aria-pressed={mods.ctrl} title="Ctrl · applies to the next key" disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => toggleModifier(event, 'ctrl')}>Ctrl</button>
    <button class="modifier-key" aria-label="Alt modifier" aria-pressed={mods.alt} title="Alt · applies to the next key" disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => toggleModifier(event, 'alt')}>Alt</button>
    {#each touchKeys.slice(2) as key (key.name)}{@render accessoryKey(key)}{/each}
  </nav>
    <div class="terminal-attachments">
      <button class="ctrl-j-key" aria-label={ctrlJKey.name} title="Ctrl+J · line feed" disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => sendKey(event, ctrlJKey.data)}>{ctrlJKey.label}</button>
      {#if canPaste}<button class="text-key" aria-label="Paste from clipboard" title="Paste clipboard text" disabled={!attached} onpointerdown={event => event.preventDefault()} onclick={event => void pasteClipboard(event)}>Paste</button>{/if}
      <button class="text-key" aria-label="Copy selection or visible screen" title="Copy the selection, or the visible screen when nothing is selected" onpointerdown={event => event.preventDefault()} onclick={event => void copyScreen(event)}>Copy</button>
      <input class="image-picker" bind:this={imagePicker} type="file" accept={imageAccept} aria-label="Choose image to attach"
        onchange={event => { void chooseImage(event.currentTarget.files?.[0], event); event.currentTarget.value = ''; }} />
      <span class:error={!!uploadError} class="attachment-status" role={uploadError ? 'alert' : 'status'}>{uploadError || uploadStatus}</span>
      {#if agent}<button class="agent-commands-button" aria-label="Agent commands" title={`${agent} commands and favorites`} aria-haspopup="dialog" disabled={!attached} onclick={event => openCommands(event)}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m13 2-10 12h9l-1 8 10-12h-9l1-8Z"/></svg>
      </button>{/if}
      <button class="attach-image" aria-label={uploadBusy ? 'Uploading image' : 'Attach image'} aria-busy={uploadBusy} disabled={!attached || uploadBusy} onclick={() => imagePicker?.click()} title="Attach image · PNG, JPEG, GIF or WebP · up to 8 MiB">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="m8 13 6-6a3 3 0 0 1 4 4l-8 8a5 5 0 0 1-7-7l9-9a7 7 0 0 1 10 10l-9 9"/></svg>
      </button>
    </div>
  </div>{/if}
  {#if !attached}<footer class="terminal-detail" role="status">{detail}{#if compact}<button class="quiet" onclick={() => reconnect()}>Reconnect</button>{/if}</footer>{/if}
</section>
{#snippet accessoryKey(key: (typeof touchKeys)[number])}
  <button aria-label={key.name} disabled={!attached} onpointerdown={event => pressKey(event, key)} onpointerup={stopRepeat} onpointercancel={stopRepeat} onpointerleave={stopRepeat} onclick={event => releaseKey(event, key.data)}>{key.label}</button>
{/snippet}
{#if paletteAgent}<CommandPalette {address} agent={paletteAgent} connected={attached && agent === paletteAgent && connectionEpoch === paletteEpoch} close={() => paletteAgent = undefined} insert={insertCommand} />{/if}
