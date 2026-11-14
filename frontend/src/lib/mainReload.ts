export const reloadContext = Symbol('main revision reload');
export interface ReloadState { ready: boolean; critical: boolean; dirty: boolean }
export interface ReloadSafety {
  busy(): () => void;
  draft(key: object, dirty: boolean): void;
  detach(key: object): void;
}
const sha = (value: unknown): value is string => typeof value === 'string' && /^[a-f0-9]{40}$/.test(value);
export function mainRevision(value: unknown): string | undefined {
  if (!value || typeof value !== 'object') return;
  const mode = value as Record<string, unknown>;
  if (mode.mode !== 'relay' || mode.channel !== 'main' || mode.basePath !== '/main/' || !sha(mode.revision)) return;
  if (mode.buildRevision !== undefined && mode.buildRevision !== mode.revision) return;
  return mode.revision;
}
export interface ReloadOptions {
  mode: unknown; sourceRevision?: string; main: boolean;
  fetch: (signal: AbortSignal) => Promise<{ ok: boolean; json(): Promise<unknown> }>;
  storage: Pick<Storage, 'getItem' | 'setItem'>;
  reload: () => void; changed: (state: ReloadState) => void;
  visible: () => boolean; timeout?: number;
}
/** One page-lifetime source revision; probes never acknowledge a deployment. */
export class MainReload implements ReloadSafety {
  private readonly loaded: string | undefined;
  private target: string | undefined;
  private drafts = new Set<object>();
  private latched = false;
  private critical = 0;
  private dead = false;
  private reloading = false;
  private request: AbortController | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  constructor(private options: ReloadOptions) {
    const initial = options.main ? mainRevision(options.mode) : undefined;
    this.loaded = initial ? (sha(options.sourceRevision) ? options.sourceRevision : initial) : undefined;
  }
  busy(): () => void {
    this.critical++; this.publish(); let done = false;
    return () => { if (done) return; done = true; this.critical--; this.publish(); };
  }
  draft(key: object, dirty: boolean): void { if (dirty) this.drafts.add(key); else this.drafts.delete(key); this.publish(); }
  detach(key: object): void { if (this.drafts.delete(key)) this.latched = true; this.publish(); }
  private state(): ReloadState { return { ready: !!this.target, critical: this.critical > 0, dirty: this.latched || this.drafts.size > 0 }; }
  private publish(): void {
    if (this.dead) return;
    const state = this.state(); this.options.changed(state);
    if (state.ready && !state.critical && !state.dirty && this.options.visible()) this.perform(false);
  }
  manual(): void { if (!this.dead && !this.state().critical) this.perform(true); }
  private perform(manual: boolean): void {
    if (!this.target || this.reloading) return;
    const key = `herdr.main:reload:${this.target}`;
    if (!manual) {
      try {
        if (this.options.storage.getItem(key)) return;
        this.options.storage.setItem(key, '1');
        if (this.options.storage.getItem(key) !== '1') return;
      } catch { return; }
    } else { try { this.options.storage.setItem(key, '1'); } catch { /* Explicit user action is never looped. */ } }
    this.reloading = true; this.options.reload();
  }
  async check(): Promise<void> {
    if (this.dead || !this.loaded || this.request || !this.options.visible()) return;
    const controller = new AbortController(); this.request = controller;
    this.timer = setTimeout(() => { controller.abort(); if (this.request === controller) this.request = undefined; }, this.options.timeout ?? 5000);
    try {
      const response = await this.options.fetch(controller.signal);
      if (!response.ok) return;
      const revision = mainRevision(await response.json());
      if (this.dead || controller.signal.aborted || this.request !== controller) return;
      if (revision && revision !== this.loaded) this.target = revision;
      else if (revision === this.loaded) this.target = undefined;
      this.publish();
    } catch { /* A network error is not evidence of a deployment. */ }
    finally { if (this.request === controller) { clearTimeout(this.timer); this.request = undefined; } }
  }
  stop(): void { this.dead = true; clearTimeout(this.timer); this.request?.abort(); this.request = undefined; }
}

/** Non-terminal form values are local drafts; xterm input has already gone remote. */
export function trackReloadActivity(doc: Document, safety: ReloadSafety): { capture: () => void; stop: () => void } {
  const fields = new Set<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>();
  let endComposition: (() => void) | undefined;
  let compositionTarget: Node | undefined;
  let stopped = false;
  const composing = new Map<ReturnType<typeof setTimeout>, () => void>();
  const scan = () => {
    if (stopped) return;
    for (const field of doc.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>('input, textarea, select')) {
      // Commands registers its actual draft explicitly; its search/filter state
      // is not unsent input, and explicit Insert/Cancel acknowledges its draft.
      if (field.closest('.xterm, .command-palette') || (field instanceof HTMLInputElement && ['button', 'submit', 'checkbox', 'radio', 'hidden', 'range'].includes(field.type))) continue;
      if (field instanceof HTMLSelectElement) continue;
      fields.add(field);
      safety.draft(field, field instanceof HTMLInputElement && field.type === 'file' ? !!field.files?.length : !!field.value);
    }
    for (const field of fields) if (!field.isConnected) { safety.detach(field); fields.delete(field); }
    if (compositionTarget && !compositionTarget.isConnected) {
      // A removed composing editor cannot emit compositionend. Its unfinished text
      // is lost locally, so require explicit discard rather than auto-reloading.
      const lost = {}; safety.draft(lost, true); safety.detach(lost);
      compositionTarget = undefined;
      const release = endComposition; endComposition = undefined; release?.();
    }
  };
  const start = (event: Event) => { compositionTarget = event.target as Node; endComposition ||= safety.busy(); };
  const end = () => {
    scan(); compositionTarget = undefined;
    const release = endComposition; endComposition = undefined;
    // compositionend capture runs before xterm's handler and the final input
    // event. Never navigate away before those events have delivered the text.
    if (release) { const timer = setTimeout(() => { composing.delete(timer); release(); }, 0); composing.set(timer, release); }
  };
  const edited = () => { scan(); queueMicrotask(scan); };
  const observer = new MutationObserver(scan);
  observer.observe(doc.documentElement, { childList: true, subtree: true });
  doc.addEventListener('input', edited, true); doc.addEventListener('change', edited, true);
  doc.addEventListener('compositionstart', start, true); doc.addEventListener('compositionend', end, true);
  scan();
  return { capture: scan, stop: () => { stopped = true; for (const [timer, release] of composing) { clearTimeout(timer); release(); } composing.clear(); observer.disconnect(); doc.removeEventListener('input', edited, true); doc.removeEventListener('change', edited, true); doc.removeEventListener('compositionstart', start, true); doc.removeEventListener('compositionend', end, true); } };
}
