import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PeerConnection, type ConnectionState } from './peerConnection';
import { authTranscript, signalTranscript, sign, verify, saveComputer, listComputers, type Computer } from './peerCrypto';

vi.mock('./peerCrypto', async importOriginal => ({
  ...await importOriginal<typeof import('./peerCrypto')>(),
  sign: vi.fn(async () => 'browser-signature'),
  verify: vi.fn(async (_key: string, _text: string, signature: string) => signature === 'connector-signature'),
  saveComputer: vi.fn(async () => {}),
  listComputers: vi.fn(async () => [] as Computer[]),
}));
const services = vi.hoisted(() => [] as Array<{
  sessionId: string; ready: () => void; lost: () => void; close: ReturnType<typeof vi.fn>;
}>);
vi.mock('./peerService', () => ({
  PeerService: class {
    close = vi.fn();
    constructor(_pc: unknown, _channel: unknown, public sessionId: string, public ready: () => void, public lost: () => void) {
      services.push(this);
    }
  },
}));

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = Socket.OPEN;
  sent: Array<Record<string, unknown>> = [];
  onopen?: () => void;
  onmessage?: (event: MessageEvent) => void;
  onerror?: () => void;
  onclose?: () => void;
  constructor(public url: URL) { Socket.instances.push(this); }
  send(data: string) { this.sent.push(JSON.parse(data)); }
  close = vi.fn(() => { this.readyState = 3; this.onclose?.(); });
  receive(data: object) { this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(data) })); }
  signals() {
    return this.sent.filter(message => message.type === 'signal').map(message => ({
      sessionId: message.sessionId, ...JSON.parse(message.payload as string),
    }));
  }
}
class Peer extends EventTarget {
  static instances: Peer[] = [];
  connectionState = 'new';
  iceGatheringState = 'complete';
  localDescription?: RTCSessionDescriptionInit;
  constructor(public configuration: RTCConfiguration) { super(); Peer.instances.push(this); }
  createDataChannel = vi.fn(() => ({}));
  createOffer = vi.fn(async () => ({ type: 'offer' as const, sdp: 'local-sdp' }));
  setLocalDescription = vi.fn(async (description: RTCSessionDescriptionInit) => { this.localDescription = description; });
  setRemoteDescription = vi.fn(async (description: RTCSessionDescriptionInit) => { void description; });
  getStats = vi.fn(async () => new Map());
  close = vi.fn(() => { this.connectionState = 'closed'; this.dispatchEvent(new Event('connectionstatechange')); });
}
const computer: Computer = {
  machineId: 'machine', publicKey: 'pinned-connector-key',
  identity: { publicKey: 'browser-key', privateKey: {} as CryptoKey },
};
const origin = 'https://app.example.test';
const iceServers = [{ urls: ['turn:relay.example.test'], username: 'user', credential: 'password' }];
const connections: PeerConnection[] = [];
// Drain the serialized receive queue and its nested crypto/SDP promises without advancing retry timers.
async function flush() { for (let i = 0; i < 40; i++) await Promise.resolve(); }
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function setup(forceTurn = false) {
  const states: ConnectionState[] = [];
  const connection = new PeerConnection({ computer }, state => states.push(state), forceTurn);
  connections.push(connection); connection.start();
  return { connection, states, socket: Socket.instances.at(-1)! };
}
async function join(socket: Socket, sessionId = 'session-1') {
  socket.receive({ type: 'challenge', nonce: 'relay-challenge' });
  socket.receive({ type: 'session', machineId: computer.machineId, sessionId });
  await flush();
}
function ice(socket: Socket, sessionId = 'session-1') { socket.receive({ type: 'ice', sessionId, iceServers, expiresAt: Math.floor(Date.now() / 1000) + 600 }); }
function signed(socket: Socket, body: object, seq = 1, sessionId = 'session-1', signature = 'connector-signature') {
  socket.receive({ type: 'signal', sessionId, payload: JSON.stringify({ kind: 'signed', seq, body: JSON.stringify(body), signature }) });
}
function accept(socket: Socket, sessionId = 'session-1') {
  signed(socket, { kind: 'accepted', protocolVersion: 1, version: '1.2.3' }, 1, sessionId);
}

beforeEach(() => {
  vi.useFakeTimers(); vi.clearAllMocks();
  vi.spyOn(Math, 'random').mockReturnValue(0);
  vi.stubGlobal('location', new URL(origin));
  vi.stubGlobal('WebSocket', Socket); vi.stubGlobal('RTCPeerConnection', Peer);
  Socket.instances = []; Peer.instances = []; services.length = 0;
});
afterEach(() => {
  connections.splice(0).forEach(connection => connection.stop());
  vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

describe('PeerConnection signaling authentication and epochs', () => {
  it('persists signed native names only after ready and refreshes automatic names on reconnect', async () => {
    const { socket } = setup(); await join(socket); ice(socket);
    signed(socket, { kind: 'accepted', protocolVersion: 1, version: '1', computerName: 'native-laptop' });
    await flush(); expect(saveComputer).not.toHaveBeenCalled();
    services[0].ready(); await flush();
    expect(saveComputer).toHaveBeenLastCalledWith(expect.objectContaining({ name: 'native-laptop', reportedName: 'native-laptop' }));
    socket.onclose?.(); await vi.advanceTimersByTimeAsync(500);
    const retry = Socket.instances.at(-1)!; await join(retry, 'fresh'); ice(retry, 'fresh');
    signed(retry, { kind: 'accepted', protocolVersion: 1, version: '1', computerName: 'renamed-laptop' }, 1, 'fresh');
    await flush(); services.at(-1)!.ready(); await flush();
    expect(saveComputer).toHaveBeenLastCalledWith(expect.objectContaining({ name: 'renamed-laptop', reportedName: 'renamed-laptop' }));
  });
  it('preserves a stored custom name while updating the authenticated native name', async () => {
    vi.mocked(listComputers).mockResolvedValueOnce([{ ...computer, name: 'My laptop', reportedName: 'old-host' }]);
    const { socket } = setup(); await join(socket); ice(socket);
    signed(socket, { kind: 'accepted', protocolVersion: 1, version: '1', computerName: 'new-host' });
    await flush(); services[0].ready(); await flush();
    expect(saveComputer).toHaveBeenLastCalledWith(expect.objectContaining({ name: 'My laptop', reportedName: 'new-host' }));
  });
  it('carries authenticated current-session build only, and resets it on reconnect', async () => {
    const { socket, states } = setup(); await join(socket); ice(socket);
    const build = { version: 'native', revision: 'a'.repeat(40), commitDate: '2026-06-03T12:00:00Z', dirty: false };
    signed(socket, { kind: 'accepted', protocolVersion: 1, version: '1.2.3', build });
    await flush(); services[0].ready(); await flush();
    expect(states.filter(state => state.service).every(state => state.build?.revision === build.revision)).toBe(true);
    expect(saveComputer).toHaveBeenCalledWith(expect.not.objectContaining({ build: expect.anything() }));
    socket.onclose?.(); await vi.advanceTimersByTimeAsync(500);
    const retry = Socket.instances.at(-1)!; await join(retry, 'fresh'); ice(retry, 'fresh'); accept(retry, 'fresh');
    await flush(); services.at(-1)!.ready(); await flush();
    expect(states.at(-1)?.build).toEqual({ version: '1.2.3', revision: null, commitDate: null, dirty: null });
  });
  it('notifies revision checks only at live signaling open, not connected route updates', async () => {
    const opened = vi.fn(), states: ConnectionState[] = [];
    const connection = new PeerConnection({ computer }, state => states.push(state), false, opened);
    connections.push(connection); connection.start();
    const socket = Socket.instances.at(-1)!;
    socket.onopen?.(); expect(opened).toHaveBeenCalledOnce();
    await join(socket); ice(socket); accept(socket); await flush();
    services[0].ready(); await flush();
    expect(states.filter(state => state.service)).toHaveLength(2);
    expect(opened).toHaveBeenCalledOnce();
    socket.onclose?.(); socket.onopen?.(); expect(opened).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(500);
    const retry = Socket.instances.at(-1)!; retry.onopen?.(); expect(opened).toHaveBeenCalledTimes(2);
    connection.stop(); retry.onopen?.(); expect(opened).toHaveBeenCalledTimes(2);
  });
  it('uses main network routes and signed context while retaining authority-only browser Origin', async () => {
    vi.stubGlobal('location', new URL(origin + '/main/'));
    const { socket } = setup(); await join(socket); ice(socket); accept(socket); await flush();
    expect(socket.url.href).toBe(`${origin.replace('https:', 'wss:')}/main/relay/socket`);
    expect(location.origin).toBe(origin);
    const auth = socket.signals()[0];
    expect(sign).toHaveBeenCalledWith(computer.identity, authTranscript(origin + '/main', 'machine', 'session-1', 'browser-key', auth.nonce));
    expect(verify).toHaveBeenCalledWith(computer.publicKey, signalTranscript(origin + '/main', 'machine', 'session-1', 1, JSON.stringify({ kind: 'accepted', protocolVersion: 1, version: '1.2.3' })), 'connector-signature');
  });
  it('supports the minimum 30-second TURN lifetime without a one-second reconnect loop', async () => {
    const { socket } = setup(true); await join(socket); accept(socket);
    socket.receive({ type: 'ice', sessionId: 'session-1', iceServers, expiresAt: Math.floor(Date.now() / 1000) + 30 });
    await flush(); services[0].ready(); await flush();
    await vi.advanceTimersByTimeAsync(26_000); expect(Socket.instances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(2_000); expect(Socket.instances).toHaveLength(2);
    expect(services[0].close).toHaveBeenCalledOnce();
  });
  it('renews expiring TURN through a fresh epoch without replaying operations', async () => {
    const { socket } = setup(true); await join(socket); ice(socket); accept(socket); await flush();
    services[0].ready(); await flush();
    await vi.advanceTimersByTimeAsync(539_000); expect(Socket.instances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1_500); expect(services[0].close).toHaveBeenCalledOnce();
    expect(Socket.instances).toHaveLength(2); expect(Socket.instances[1].signals()).toHaveLength(0);
  });

  it('joins and signs session-bound resume authentication, but never offers before verified acceptance', async () => {
    const { socket, states } = setup();
    expect(socket.url.href).toBe(`${origin.replace('https:', 'wss:')}/relay/socket`);
    await join(socket); ice(socket); await flush();
    expect(socket.sent[0]).toEqual({ type: 'join', machineId: 'machine' });
    const auth = socket.signals()[0];
    expect(auth).toMatchObject({ sessionId: 'session-1', kind: 'auth', mode: 'resume', publicKey: 'browser-key', proof: 'browser-signature' });
    expect(sign).toHaveBeenCalledWith(computer.identity, authTranscript(origin, 'machine', 'session-1', 'browser-key', auth.nonce));
    expect(Peer.instances).toHaveLength(0);
    const verification = deferred<boolean>(); vi.mocked(verify).mockReturnValueOnce(verification.promise);
    accept(socket); await flush();
    expect(Peer.instances).toHaveLength(0);
    verification.resolve(true); await flush();
    expect(verify).toHaveBeenCalledWith(computer.publicKey, signalTranscript(origin, 'machine', 'session-1', 1, JSON.stringify({ kind: 'accepted', protocolVersion: 1, version: '1.2.3' })), 'connector-signature');
    expect(Peer.instances).toHaveLength(1);
    expect(socket.signals()[1]).toMatchObject({ kind: 'signed', seq: 1, body: JSON.stringify({ kind: 'offer', sdp: 'local-sdp' }) });
    expect(states.at(-1)?.status).toBe('Establishing private peer connection…');
    expect(saveComputer).not.toHaveBeenCalled();
  });

  it('rejects forged acceptance even when relay ICE is already available', async () => {
    const { socket, states } = setup(); await join(socket); ice(socket);
    signed(socket, { kind: 'accepted', protocolVersion: 1, version: 'forged', computerName: 'Forged hostname' }, 1, 'session-1', 'forged');
    await flush();
    expect(Peer.instances).toHaveLength(0); expect(socket.close).toHaveBeenCalledOnce();
    expect(states.at(-1)?.status).toBe('Connection interrupted');
    expect(saveComputer).not.toHaveBeenCalled();
  });

  it('requires session-authorized ICE after acceptance and uses its credentials and TURN policy', async () => {
    const { socket } = setup(true); await join(socket); accept(socket); await flush();
    expect(Peer.instances).toHaveLength(0);
    ice(socket); await flush();
    expect(Peer.instances[0].configuration).toEqual({ iceServers, iceTransportPolicy: 'relay' });
    expect(Peer.instances[0].createDataChannel).toHaveBeenCalledWith('herdr-control-v1', { ordered: true });
    expect(services[0].sessionId).toBe('session-1');
  });

  it('rejects ICE issued for a different session without creating an offer', async () => {
    const { socket, states } = setup(); await join(socket); accept(socket); await flush();
    ice(socket, 'other-session'); await flush();
    expect(Peer.instances).toHaveLength(0);
    expect(states.at(-1)?.status).toBe('Connection interrupted');
  });

  it('rejects a forged signed answer before applying any remote SDP', async () => {
    const { socket, states } = setup(); await join(socket); ice(socket); accept(socket); await flush();
    const peer = Peer.instances[0];
    signed(socket, { kind: 'answer', sdp: 'attacker-sdp' }, 2, 'session-1', 'forged'); await flush();
    expect(peer.setRemoteDescription).not.toHaveBeenCalled();
    expect(peer.close).toHaveBeenCalledOnce(); expect(services[0].close).toHaveBeenCalledOnce();
    expect(states.at(-1)?.status).toBe('Connection interrupted');
    expect(saveComputer).not.toHaveBeenCalled();
  });

  it('ignores an old async acceptance after retry has established a fresh session', async () => {
    const { socket, states } = setup(); await join(socket); ice(socket); await flush();
    const verification = deferred<boolean>(); vi.mocked(verify).mockReturnValueOnce(verification.promise);
    accept(socket); await flush(); socket.onerror?.();
    await vi.advanceTimersByTimeAsync(500);
    const next = Socket.instances[1]; await join(next, 'session-2'); ice(next, 'session-2'); accept(next, 'session-2'); await flush();
    const stateCount = states.length;
    verification.resolve(true); await flush();
    expect(states).toHaveLength(stateCount); expect(Peer.instances).toHaveLength(1);
    expect(services[0].sessionId).toBe('session-2'); expect(next.close).not.toHaveBeenCalled();
    expect(socket.signals()).toHaveLength(1);
  });

  it('discards a pending signed payload on disconnect and retries with new peers and sequence numbers', async () => {
    const { socket, states } = setup(); await join(socket);
    const pendingSignature = deferred<string>(); vi.mocked(sign).mockReturnValueOnce(pendingSignature.promise);
    ice(socket); accept(socket); await flush();
    const oldPeer = Peer.instances[0], oldService = services[0];
    socket.onerror?.();
    expect(oldPeer.close).toHaveBeenCalledOnce(); expect(oldService.close).toHaveBeenCalledOnce();
    expect(states.at(-1)?.error).toContain('not replayed');
    await vi.advanceTimersByTimeAsync(500);
    const next = Socket.instances[1]; await join(next, 'session-2');
    expect(next.signals()).toHaveLength(1); // Only fresh auth: no queued offer is replayed.
    ice(next, 'session-2'); accept(next, 'session-2'); await flush();
    expect(Peer.instances).toHaveLength(2); expect(services[1]).not.toBe(oldService);
    expect(next.signals()[1]).toMatchObject({ sessionId: 'session-2', kind: 'signed', seq: 1 });
    const sent = next.sent.length, stateCount = states.length;
    pendingSignature.resolve('stale-signature'); await flush();
    expect(socket.signals()).toHaveLength(1); expect(next.sent).toHaveLength(sent);
    expect(states).toHaveLength(stateCount); expect(next.close).not.toHaveBeenCalled();
    signed(next, { kind: 'answer', sdp: 'fresh-answer' }, 2, 'session-2'); await flush();
    expect(Peer.instances[1].setRemoteDescription).toHaveBeenCalledWith({ type: 'answer', sdp: 'fresh-answer' });
    expect(oldPeer.setRemoteDescription).not.toHaveBeenCalled();
  });
});
