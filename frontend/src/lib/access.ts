import { ApiError, request } from './api';
import { record } from './schema';
import type { transportRequest } from './transport';

export interface AccessStatus {
  supported: boolean; canRotate: boolean; scope: 'server' | 'computer';
  tokenLifetime: 'untilRotated'; sessionLifetime: 'untilRevoked';
}
export interface AccessState {
  status: AccessStatus | null; pending: boolean; confirming: boolean;
  unavailable: boolean; uncertain: boolean; issue: string; message: string;
}
export function parseAccessStatus(value: unknown): AccessStatus {
  if (!record(value) || typeof value.supported !== 'boolean' || typeof value.canRotate !== 'boolean' ||
      !['server', 'computer'].includes(String(value.scope)) || value.tokenLifetime !== 'untilRotated' ||
      value.sessionLifetime !== 'untilRevoked' || (value.canRotate && !value.supported)) {
    throw new Error('Access information is unreadable. Update this server or connector and try again.');
  }
  return value as unknown as AccessStatus;
}

/** One captured authenticated destination. Never persists or replays a rotation. */
export class AccessController {
  readonly state: AccessState = { status: null, pending: false, confirming: false, unavailable: false, uncertain: false, issue: '', message: '' };
  private dead = false;
  private abort = new AbortController();
  constructor(private send: typeof transportRequest, private changed: (state: AccessState) => void,
    private busy: () => () => void = () => () => {}) {}
  private publish(): void { if (!this.dead) this.changed({ ...this.state }); }
  async load(): Promise<void> {
    if (this.dead || this.state.pending) return;
    this.state.pending = true; this.state.issue = ''; this.publish();
    try {
      const status = parseAccessStatus(await request('/api/access', undefined, this.abort.signal, this.send));
      if (!this.dead) { this.state.status = status; this.state.unavailable = !status.supported; }
    } catch (error) {
      if (!this.dead) {
        this.state.status = null;
        this.state.unavailable = error instanceof ApiError && [400, 404, 405, 501].includes(error.status);
        // Do not render arbitrary server bodies: access responses must never expose credentials.
        this.state.issue = this.state.unavailable ? 'Access controls are not supported by this server or connector yet.' : 'Access information could not be loaded. Check the connection and try again.';
      }
    } finally { if (!this.dead) { this.state.pending = false; this.publish(); } }
  }
  ask(): void {
    if (this.dead || this.state.pending || !this.state.status?.canRotate || this.state.uncertain) return;
    this.state.confirming = true; this.state.issue = ''; this.state.message = ''; this.publish();
  }
  cancel(): void { if (!this.dead) { this.state.confirming = false; this.publish(); } }
  async rotate(): Promise<void> {
    if (this.dead || this.state.pending || !this.state.confirming || !this.state.status?.canRotate || this.state.uncertain) return;
    this.state.pending = true; this.state.confirming = false; this.state.issue = ''; this.state.message = ''; this.publish();
    const release = this.busy();
    try {
      const result = await request('/api/access/rotate', {}, this.abort.signal, this.send);
      if (!record(result) || result.rotated !== true) throw new Error('Unconfirmed rotation');
      if (!this.dead) this.state.message = 'Access token rotated. This browser stays signed in. Other browsers must use the new private link.';
    } catch (error) {
      if (!this.dead) {
        const rejected = error instanceof ApiError && error.status >= 400 && error.status < 500;
        this.state.uncertain = !rejected;
        this.state.issue = rejected ? 'Rotation was rejected. Reconnect and check access before trying again.' : 'Rotation could not be confirmed. It will not be retried. Check the private link on the computer before rotating again.';
      }
    } finally {
      release();
      if (!this.dead) { this.state.pending = false; this.publish(); }
    }
  }
  /**
   * The private link for another browser, fetched only on an explicit tap. It is
   * returned to the caller for the clipboard and never kept in state, where it
   * could be rendered or logged. `root` is the address this browser is using.
   */
  async link(root: string): Promise<string | null> {
    if (this.dead || this.state.pending || !this.state.status?.supported) return null;
    this.state.pending = true; this.state.issue = ''; this.state.message = ''; this.publish();
    try {
      const result = await request('/api/access/link', {}, this.abort.signal, this.send);
      if (!record(result) || typeof result.fragment !== 'string' || !/^(token|pair)=[A-Za-z0-9_-]{16,4096}$/.test(result.fragment)) {
        throw new Error('Unreadable link');
      }
      return this.dead ? null : `${root}#${result.fragment}`;
    } catch (error) {
      if (!this.dead) {
        const unsupported = error instanceof ApiError && [404, 405, 501].includes(error.status);
        // Never render the response body: it may contain the credential.
        this.state.issue = unsupported
          ? 'Copying the access link is not supported by this server or connector yet. Update it and try again.'
          : 'The access link could not be fetched. Check the connection and try again.';
      }
      return null;
    } finally { if (!this.dead) { this.state.pending = false; this.publish(); } }
  }
  unreadable(): void { if (!this.dead) { this.state.issue = 'The QR code could not be drawn. Use “Copy access link” instead.'; this.publish(); } }
  /** Outcome of a copy the component performed with a link from `link()`. */
  copied(ok: boolean): void {
    if (this.dead) return;
    this.state.issue = ok ? '' : 'This browser blocked the clipboard. Tap “Copy access link” once more.';
    this.state.message = ok ? 'Access link copied. Paste it in the other browser, and keep it private.' : '';
    this.publish();
  }
  stop(): void { this.dead = true; this.abort.abort(); }
}
