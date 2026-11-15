import { createHash, createHmac, createPublicKey, verify as nodeVerify, webcrypto } from 'node:crypto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  authTranscript, base64Decode, base64Encode, base64urlDecode, base64urlEncode,
  forgetComputer, generateIdentity, listComputers, machineIdForKey, pairProof, parsePairing, randomNonce,
  saveComputer, signalTranscript, sign, takePairingFragment, verify, computerName, sanitizeComputerName, withComputerName, type Computer,
} from './peerCrypto';

vi.stubGlobal('crypto', webcrypto);
afterEach(() => { vi.restoreAllMocks(); });
const text = new TextEncoder();
async function pairing() {
  const identity = await generateIdentity();
  return { v: 1 as const, publicKey: identity.publicKey, machineId: await machineIdForKey(identity.publicKey),
    secret: base64urlEncode(new Uint8Array(32).fill(11)), expiresAt: Math.floor(Date.now() / 1000) + 300 };
}
function fragment(data: unknown): string { return '#' + base64urlEncode(text.encode(JSON.stringify(data))); }

describe('friendly computer names', () => {
  const computer = { machineId: 'random-internal-id' } as Computer;
  it('uses names without displaying internal IDs, with a caller-supplied fallback', () => {
    expect(computerName(computer)).toBe('Computer');
    expect(computerName(computer, 'Computer 2')).toBe('Computer 2');
    expect(computerName({ reportedName: 'native' })).toBe('native');
    expect(computerName({ name: 'Custom', reportedName: 'native' })).toBe('Custom');
  });
  it('bounds and sanitizes both reported and stored display text', () => {
    expect(sanitizeComputerName(null)).toBeUndefined();
    expect(sanitizeComputerName(' \u001b\u202eLaptop\n\u0000 ')).toBe('Laptop');
    expect(sanitizeComputerName('a'.repeat(200))).toHaveLength(128);
    expect(computerName({ name: '\u0000', reportedName: 'native' })).toBe('native');
  });
  it('fills legacy nameless records, refreshes automatic names and preserves custom/legacy names', () => {
    const first = withComputerName(computer, 'first');
    expect(first).toMatchObject({ name: 'first', reportedName: 'first' });
    expect(withComputerName(first, 'second')).toMatchObject({ name: 'second', reportedName: 'second' });
    expect(withComputerName({ ...first, name: 'My laptop' }, 'second')).toMatchObject({ name: 'My laptop', reportedName: 'second' });
    expect(withComputerName({ ...computer, name: 'Legacy custom' }, 'second')).toMatchObject({ name: 'Legacy custom', reportedName: 'second' });
    expect(withComputerName(first, undefined)).toEqual(first);
  });
});

describe('strict wire encodings', () => {
  it('matches RFC4648 base64/base64url vectors including sliced byte arrays', () => {
    for (const [plain, encoded] of [['', ''], ['f', 'Zg=='], ['fo', 'Zm8='], ['foo', 'Zm9v'], ['foobar', 'Zm9vYmFy']]) {
      expect(base64Encode(text.encode(plain))).toBe(encoded);
      expect(base64Decode(encoded)).toEqual(text.encode(plain));
      expect(base64urlDecode(encoded.replace(/=/g, ''))).toEqual(text.encode(plain));
    }
    expect(base64urlEncode(new Uint8Array([0, 251, 255]).subarray(1))).toBe('-_8');
    expect(base64urlDecode('-_8')).toEqual(new Uint8Array([251, 255]));
  });
  it('rejects noncanonical trailing bits, padding, alphabet and whitespace', () => {
    for (const invalid of ['A', 'Zh', 'Zm9', 'Zg==', 'Zg=', 'Zg\n', '+/8', 'é']) {
      expect(() => base64urlDecode(invalid)).toThrow();
    }
    for (const invalid of ['Zg', 'Zh==', 'Zm9=', '-_8=', ' Zg==', 'Zg===']) {
      expect(() => base64Decode(invalid)).toThrow();
    }
  });
});

describe('pairing capability', () => {
  it('accepts reusable permanent capabilities using the compatible v1 safe-integer expiry', async () => {
    const p = { ...await pairing(), expiresAt: Number.MAX_SAFE_INTEGER };
    expect(await parsePairing(fragment(p))).toEqual(p);
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 100 * 365 * 86400 * 1000);
    expect(await parsePairing(fragment(p))).toEqual(p);
    expect(await parsePairing(fragment(p))).toEqual(p);
  });
  it('validates canonical key hashes and Unix-second expiry', async () => {
    const p = await pairing();
    expect(await parsePairing(fragment(p))).toEqual(p);
    expect(await parsePairing('pair=' + fragment(p).slice(1))).toEqual(p);
    const expected = createHash('sha256').update(base64urlDecode(p.publicKey)).digest('base64url');
    expect(p.machineId).toBe(expected);
  });
  it('binds main pairing capabilities to their channel while retaining legacy root pairings', async () => {
    const p = await pairing();
    const main = { ...p, channel: 'main', basePath: '/main/' };
    try {
      vi.stubGlobal('location', new URL('https://relay.test/'));
      expect(await parsePairing(fragment(p))).toEqual(p);
      await expect(parsePairing(fragment(main))).rejects.toThrow('different relay channel');
      vi.stubGlobal('location', new URL('https://relay.test/main/'));
      expect(await parsePairing(fragment(main))).toEqual(p);
      await expect(parsePairing(fragment(p))).rejects.toThrow('different relay channel');
      await expect(parsePairing(fragment({ ...main, basePath: '/' }))).rejects.toThrow('different relay channel');
    } finally { vi.unstubAllGlobals(); vi.stubGlobal('crypto', webcrypto); }
  });
  it('rejects expired, malformed, oversized, wrong-key and wrong-secret payloads', async () => {
    const p = await pairing();
    const other = await pairing();
    for (const value of [null, [], 1, {}, { ...p, v: 2 }, { ...p, expiresAt: 0 },
      { ...p, expiresAt: Date.now() / 1000 - 1 }, { ...p, expiresAt: 1.5 },
      { ...p, expiresAt: '9999999999' }, { ...p, machineId: other.machineId },
      { ...p, publicKey: base64urlEncode(new Uint8Array(65).fill(4)) },
      { ...p, publicKey: base64urlEncode(new Uint8Array(33).fill(2)) },
      { ...p, secret: base64urlEncode(new Uint8Array(31)) }, { ...p, secret: p.secret + '=' }]) {
      await expect(parsePairing(fragment(value))).rejects.toThrow();
    }
    for (const invalid of ['#' + 'a'.repeat(2049), '#_w', '#e30=', '#not-json']) {
      await expect(parsePairing(invalid)).rejects.toThrow();
    }
  });
  it('removes pairing capability synchronously, without disturbing local tokens', () => {
    const replaceState = vi.fn();
    vi.stubGlobal('history', { state: { navigation: 1 }, replaceState });
    vi.stubGlobal('location', { hash: '#secret', pathname: '/app', search: '?x=1' });
    expect(takePairingFragment()).toBe('#secret');
    expect(replaceState).toHaveBeenCalledWith({ navigation: 1 }, '', '/app?x=1');
    vi.stubGlobal('location', { hash: '#token=local' });
    expect(takePairingFragment()).toBeNull();
    expect(replaceState).toHaveBeenCalledTimes(1);
  });
});

describe('authenticated transcripts', () => {
  it('uses exact wire bytes and keeps signed body whitespace and newlines', () => {
    expect(authTranscript('https://relay', 'machine', 'session', 'key', 'nonce'))
      .toBe('herdr-auth-v1\nhttps://relay\nmachine\nsession\nkey\nnonce');
    expect(signalTranscript('https://relay', 'm', 's', 12, '{ "sdp": "x" }\n'))
      .toBe('herdr-signal-v1\nhttps://relay\nm\ns\n12\n{ "sdp": "x" }\n');
    for (const seq of [0, -1, 1.1, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => signalTranscript('o', 'm', 's', seq, '{}')).toThrow();
    }
    expect(() => authTranscript('o\nx', 'm', 's', 'k', 'n')).toThrow();
  });
  it('uses nonextractable P-256 private keys and 64-byte IEEE-P1363 signatures', async () => {
    const identity = await generateIdentity();
    expect(identity.privateKey.extractable).toBe(false);
    await expect(crypto.subtle.exportKey('pkcs8', identity.privateKey)).rejects.toThrow();
    const transcript = authTranscript('https://relay', 'm', 's', identity.publicKey, randomNonce());
    const signature = await sign(identity, transcript);
    expect(base64urlDecode(signature)).toHaveLength(64);
    expect(await verify(identity.publicKey, transcript, signature)).toBe(true);
    const raw = base64urlDecode(identity.publicKey);
    const publicKey = createPublicKey({ format: 'jwk', key: {
      kty: 'EC', crv: 'P-256', x: base64urlEncode(raw.slice(1, 33)), y: base64urlEncode(raw.slice(33)),
    } });
    expect(nodeVerify('sha256', Buffer.from(transcript), { key: publicKey, dsaEncoding: 'ieee-p1363' }, base64urlDecode(signature))).toBe(true);
    expect(await verify(identity.publicKey, transcript + 'x', signature)).toBe(false);
    expect(await verify((await generateIdentity()).publicKey, transcript, signature)).toBe(false);
    const tampered = base64urlDecode(signature); tampered[0] ^= 1;
    expect(await verify(identity.publicKey, transcript, base64urlEncode(tampered))).toBe(false);
    expect(await verify(identity.publicKey, transcript, 'AA')).toBe(false);
    expect(await verify('invalid', transcript, signature)).toBe(false);
  });
  it('matches independently computed HMAC-SHA256 and detects changed transcripts', async () => {
    const secret = new Uint8Array(32).fill(11);
    const expected = createHmac('sha256', secret).update('Hi There').digest('base64url');
    expect(await pairProof(base64urlEncode(secret), 'Hi There')).toBe(expected);
    expect(await pairProof(base64urlEncode(secret), 'Hi There!')).not.toBe(expected);
    await expect(pairProof('AA', 'Hi There')).rejects.toThrow();
    const nonces = new Set(Array.from({ length: 32 }, () => randomNonce()));
    expect(nonces.size).toBe(32);
    for (const nonce of nonces) expect(base64urlDecode(nonce)).toHaveLength(32);
  });
  it('explicitly saves cloneable nonextractable keys and lists/forgets computers', async () => {
    // Minimal asynchronous IDB contract double; structuredClone exercises CryptoKey persistence.
    const records = new Map<string, unknown>();
    const close = vi.fn();
    const db = {
      close,
      transaction: vi.fn(() => {
        const transaction = {
          oncomplete: null as (() => void) | null,
          objectStore: () => ({
            put: (value: { machineId: string }) => {
              records.set(value.machineId, structuredClone(value));
              return { result: value.machineId };
            },
            getAll: () => ({ result: structuredClone([...records.values()]) }),
            delete: (key: string) => { records.delete(key); return { result: undefined }; },
          }),
        };
        queueMicrotask(() => transaction.oncomplete?.());
        return transaction;
      }),
    };
    const open = vi.fn(() => {
      const request = { result: db, onsuccess: null as (() => void) | null };
      queueMicrotask(() => request.onsuccess?.());
      return request;
    });
    vi.stubGlobal('indexedDB', { open });
    const p = await pairing();
    const identity = await generateIdentity();
    const record = { machineId: p.machineId, publicKey: p.publicKey, identity, name: 'Laptop', version: '1.2.3' };
    await saveComputer(record);
    // Loaded Computer state and its identity become deep reactive Proxies in Svelte.
    await saveComputer(new Proxy({ ...record, identity: new Proxy(identity, {}) }, {}));
    const saved = await listComputers();
    expect(saved).toHaveLength(1);
    expect(saved[0]).toMatchObject({ machineId: p.machineId, publicKey: p.publicKey, name: 'Laptop', version: '1.2.3' });
    expect(saved[0].identity.privateKey.extractable).toBe(false);
    expect(await verify(identity.publicKey, 'resume', await sign(saved[0].identity, 'resume'))).toBe(true);
    await forgetComputer(p.machineId);
    expect(await listComputers()).toEqual([]);
    expect(close).toHaveBeenCalledTimes(5);
    expect(db.transaction.mock.calls).toHaveLength(5);
  });
  it('never touches persistence during pairing, identity generation or signing', async () => {
    const open = vi.fn(() => { throw new Error('must not save before ready'); });
    vi.stubGlobal('indexedDB', { open });
    const p = await pairing();
    await parsePairing(fragment(p));
    const identity = await generateIdentity();
    await sign(identity, 'hello');
    expect(open).not.toHaveBeenCalled();
    await expect(saveComputer({ machineId: p.machineId, publicKey: p.publicKey, identity })).rejects.toThrow('must not save before ready');
    expect(open).toHaveBeenCalledOnce();
  });
});
