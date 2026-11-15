import { channelBase, storageKey } from './channel';
// Relay v1 wire encodings and transcripts are deliberately independent of transport.
export interface Pairing {
  v: 1;
  machineId: string;
  publicKey: string;
  secret: string;
  /** Unix seconds, matching the connector capability. */
  expiresAt: number;
}
export interface BrowserIdentity { publicKey: string; privateKey: CryptoKey }
export interface Computer {
  machineId: string;
  /** Pinned remote (connector) key, not the browser key. */
  publicKey: string;
  identity: BrowserIdentity;
  version?: string;
  name?: string;
  /** Last authenticated native Core name; separate from a user's custom name. */
  reportedName?: string;
}
/** Single-line, bounded display text, including names loaded from older storage. */
export function sanitizeComputerName(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  // eslint-disable-next-line no-control-regex -- Remove terminal/control and directional formatting from display names.
  const name = Array.from(value.slice(0, 4096).replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/g, '').replace(/\s+/g, ' ').trim()).slice(0, 128).join('');
  return name || undefined;
}
export function computerName(computer?: Pick<Computer, 'name' | 'reportedName'>, fallback = 'Computer'): string {
  return sanitizeComputerName(computer?.name) || sanitizeComputerName(computer?.reportedName) || sanitizeComputerName(fallback) || 'Computer';
}
/** Only pass a reported name from verified connector metadata. */
export function withComputerName(computer: Computer, reported: unknown): Computer {
  const reportedName = sanitizeComputerName(reported);
  const name = sanitizeComputerName(computer.name);
  const previous = sanitizeComputerName(computer.reportedName);
  return { ...computer,
    ...(reportedName ? { reportedName } : {}),
    ...((!name || name === previous) && reportedName ? { name: reportedName } : name ? { name } : {}),
  };
}
const encoder = new TextEncoder();
const MAX_PAIRING_LENGTH = 2048;

export function base64Encode(bytes: Uint8Array): string {
  let text = '';
  for (let i = 0; i < bytes.length; i += 8192) text += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(text);
}
export function base64Decode(text: string): Uint8Array {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(text)) throw new Error('Invalid base64');
  const bytes = Uint8Array.from(atob(text), char => char.charCodeAt(0));
  if (base64Encode(bytes) !== text) throw new Error('Noncanonical base64');
  return bytes;
}
export function base64urlEncode(bytes: Uint8Array): string {
  return base64Encode(bytes).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
export function base64urlDecode(text: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]*$/.test(text) || text.length % 4 === 1) throw new Error('Invalid base64url');
  const normal = text.replace(/-/g, '+').replace(/_/g, '/');
  return base64Decode(normal + '='.repeat((4 - normal.length % 4) % 4));
}
// Short aliases are convenient for protocol consumers.
export { base64urlEncode as encodeBase64url, base64urlDecode as decodeBase64url,
  base64Encode as encodeBase64, base64Decode as decodeBase64 };

async function importPublicKey(publicKey: string): Promise<CryptoKey> {
  const bytes = base64urlDecode(publicKey);
  if (bytes.length !== 65 || bytes[0] !== 4) throw new Error('Invalid P-256 public key');
  return crypto.subtle.importKey('raw', bytes, { name: 'ECDSA', namedCurve: 'P-256' }, false, ['verify']);
}
export async function machineIdForKey(publicKey: string): Promise<string> {
  await importPublicKey(publicKey);
  return base64urlEncode(new Uint8Array(await crypto.subtle.digest('SHA-256', base64urlDecode(publicKey))));
}
export async function parsePairing(fragment: string): Promise<Pairing> {
  if (fragment.length > MAX_PAIRING_LENGTH) throw new Error('Pairing fragment too large');
  let encoded = fragment.startsWith('#') ? fragment.slice(1) : fragment;
  if (encoded.startsWith('pair=')) encoded = encoded.slice(5);
  const data: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(base64urlDecode(encoded)));
  if (!data || typeof data !== 'object' || Array.isArray(data)) throw new Error('Invalid pairing');
  const p = data as Record<string, unknown>;
  if (channelBase() === '/main/'
    ? p.channel !== 'main' || p.basePath !== '/main/'
    : (p.channel !== undefined && p.channel !== 'stable') || (p.basePath !== undefined && p.basePath !== '/')) {
    throw new Error('Pairing belongs to a different relay channel; scan its QR at the matching channel URL');
  }
  if (p.v !== 1 || typeof p.machineId !== 'string' || typeof p.publicKey !== 'string' ||
      typeof p.secret !== 'string' || typeof p.expiresAt !== 'number' ||
      !Number.isSafeInteger(p.expiresAt) || p.expiresAt <= Date.now() / 1000) throw new Error('Invalid or expired pairing');
  if (base64urlDecode(p.secret).length !== 32 || base64urlDecode(p.machineId).length !== 32 ||
      await machineIdForKey(p.publicKey) !== p.machineId) throw new Error('Invalid pairing identity');
  return { v: 1, machineId: p.machineId, publicKey: p.publicKey, secret: p.secret, expiresAt: p.expiresAt };
}
/** Capture before asynchronous validation, so even invalid capabilities leave no URL secret. */
export function takePairingFragment(): string | null {
  const hash = location.hash;
  if (!hash || new URLSearchParams(hash.slice(1)).has('token')) return null;
  history.replaceState(history.state, '', location.pathname + location.search);
  return hash;
}
export async function generateIdentity(): Promise<BrowserIdentity> {
  const keys = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify']);
  return { publicKey: base64urlEncode(new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey))), privateKey: keys.privateKey };
}
function line(value: string): string {
  if (/[\r\n]/.test(value)) throw new Error('Invalid transcript field');
  return value;
}
export function authTranscript(origin: string, machineId: string, sessionId: string, publicKey: string, nonce: string): string {
  return ['herdr-auth-v1', origin, machineId, sessionId, publicKey, nonce].map(line).join('\n');
}
export function signalTranscript(origin: string, machineId: string, sessionId: string, seq: number, body: string): string {
  if (!Number.isSafeInteger(seq) || seq <= 0) throw new Error('Invalid signal sequence');
  return ['herdr-signal-v1', origin, machineId, sessionId, String(seq)].map(line).join('\n') + '\n' + body;
}
export async function sign(identity: BrowserIdentity, text: string): Promise<string> {
  const signature = new Uint8Array(await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, identity.privateKey, encoder.encode(text)));
  if (signature.length !== 64) throw new Error('Unsupported ECDSA signature encoding');
  return base64urlEncode(signature);
}
export async function verify(publicKey: string, text: string, signature: string): Promise<boolean> {
  try {
    const bytes = base64urlDecode(signature);
    return bytes.length === 64 && await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, await importPublicKey(publicKey), bytes, encoder.encode(text));
  } catch { return false; }
}
export async function pairProof(secret: string, text: string): Promise<string> {
  const bytes = base64urlDecode(secret);
  if (bytes.length !== 32) throw new Error('Invalid pairing secret');
  const key = await crypto.subtle.importKey('raw', bytes, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  return base64urlEncode(new Uint8Array(await crypto.subtle.sign('HMAC', key, encoder.encode(text))));
}
export function randomNonce(): string { return base64urlEncode(crypto.getRandomValues(new Uint8Array(32))); }

const DB_NAME = 'herdr-relay-identities-v1';
const STORE = 'computers';
function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(storageKey(DB_NAME), 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: 'machineId' });
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error('Computer database blocked'));
    request.onsuccess = () => resolve(request.result);
  });
}
async function operation<T>(mode: IDBTransactionMode, action: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await openDatabase();
  try {
    return await new Promise<T>((resolve, reject) => {
      const transaction = db.transaction(STORE, mode);
      const request = action(transaction.objectStore(STORE));
      transaction.oncomplete = () => resolve(request.result);
      transaction.onabort = () => reject(transaction.error ?? new Error('Computer database transaction aborted'));
      transaction.onerror = () => reject(transaction.error ?? request.error);
    });
  } finally { db.close(); }
}
/** Call only after the authenticated control channel's ready message. No implicit saves. */
export async function saveComputer(computer: Computer): Promise<void> {
  if (await machineIdForKey(computer.publicKey) !== computer.machineId) throw new Error('Invalid computer identity');
  const key = computer.identity.privateKey;
  if (key.type !== 'private' || key.extractable || key.algorithm.name !== 'ECDSA' ||
      (key.algorithm as EcKeyAlgorithm).namedCurve !== 'P-256' || !key.usages.includes('sign')) throw new Error('Invalid browser identity');
  // Ensure the stored public key actually corresponds to the nonextractable key.
  const probe = randomNonce();
  if (!await verify(computer.identity.publicKey, probe, await sign(computer.identity, probe))) throw new Error('Mismatched browser identity');
  // Svelte's deep reactive state wraps loaded records/identity objects in Proxy.
  // IndexedDB cannot clone a Proxy: snapshot primitives while retaining the native CryptoKey.
  const saved: Computer = {
    machineId: computer.machineId, publicKey: computer.publicKey,
    identity: { publicKey: computer.identity.publicKey, privateKey: key },
    ...(computer.version !== undefined ? { version: computer.version } : {}),
    ...(sanitizeComputerName(computer.name) ? { name: sanitizeComputerName(computer.name) } : {}),
    ...(sanitizeComputerName(computer.reportedName) ? { reportedName: sanitizeComputerName(computer.reportedName) } : {}),
  };
  await operation('readwrite', store => store.put(saved));
}
export async function listComputers(): Promise<Computer[]> {
  return operation('readonly', store => store.getAll());
}
export async function forgetComputer(machineId: string): Promise<void> {
  await operation('readwrite', store => store.delete(machineId));
}
