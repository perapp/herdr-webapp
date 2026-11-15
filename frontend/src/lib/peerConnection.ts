import { channelContext, channelPath } from './channel';
import { authTranscript, signalTranscript, generateIdentity, randomNonce, pairProof, sign, verify, saveComputer, listComputers, withComputerName } from './peerCrypto';
import type { Pairing, Computer, BrowserIdentity } from './peerCrypto';
import { PeerService } from './peerService';
import { normalizeBuild, versionText } from './versions';
import type { BuildInfo } from './versions';

export interface ConnectionState { status: string; service?: PeerService; version?: string; build?: BuildInfo; route?: string; error?: string }
export interface PeerTarget { computer?: Computer; pairing?: Pairing }
/** A reconnect creates a new signaling session, PC, service and all channel epochs. */
export class PeerConnection {
  private epoch = 0;
  private stopped = false;
  private cleanup: (() => void) | undefined;
  private retry: ReturnType<typeof setTimeout> | undefined;
  private attempts = 0;
  private identity: BrowserIdentity | undefined;
  constructor(private target: PeerTarget, private update: (state: ConnectionState) => void, private forceTurn = false, private onSignalingOpen?: () => void) {}
  start(): void { this.stopped = false; void this.connect(); }
  stop(): void { this.stopped = true; this.epoch++; clearTimeout(this.retry); this.cleanup?.(); this.cleanup = undefined; }
  private async connect(): Promise<void> {
    const epoch = ++this.epoch;
    const current = () => !this.stopped && this.epoch === epoch;
    this.update({ status: this.attempts ? 'Reconnecting securely…' : 'Connecting securely…' });
    let socket: WebSocket | undefined;
    let pc: RTCPeerConnection | undefined;
    let service: PeerService | undefined;
    let ended = false;
    let deadline: ReturnType<typeof setTimeout> | undefined;
    let renewal: ReturnType<typeof setTimeout> | undefined;
    let iceExpiresAt: number | undefined;
    const dispose = () => { ended = true; clearTimeout(deadline); clearTimeout(renewal); service?.close(); pc?.close(); socket?.close(); };
    this.cleanup = dispose;
    const fail = () => {
      if (!current() || ended) return;
      dispose(); this.epoch++;
      this.update({ status: 'Connection interrupted', error: 'Reconnecting automatically. Input and pending operations are not replayed. If access was rotated, use the computer’s new private link.' });
      const delay = Math.min(15_000, 500 * 2 ** Math.min(this.attempts++, 5)) + Math.random() * 250;
      this.retry = setTimeout(() => { if (!this.stopped) void this.connect(); }, delay);
    };
    const guard = () => { if (!current() || ended) throw new Error('Stale connection epoch'); };
    try {
      const pairing = this.target.pairing;
      const computer = this.target.computer;
      const machineId = pairing?.machineId || computer?.machineId;
      const remoteKey = pairing?.publicKey || computer?.publicKey;
      if (!machineId || !remoteKey) throw new Error('Choose a computer');
      if (pairing && pairing.expiresAt <= Date.now() / 1000) {
        dispose(); this.update({ status: 'Pairing expired', error: 'Scan a fresh QR code from your computer.' }); return;
      }
      const identity = this.identity ||= computer?.identity || await generateIdentity(); guard();
      const origin = channelContext();
      const endpoint = new URL(channelPath('/relay/socket'), location.origin); endpoint.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      socket = new WebSocket(endpoint);
      socket.onopen = () => { if (current() && !ended) this.onSignalingOpen?.(); };
      let sessionId = '';
      let incoming = 0;
      let outgoing = 0;
      let joined = false;
      let accepted = false;
      let offered = false;
      let answered = false;
      let connectorVersion = '';
      let connectorName: unknown;
      let connectorBuild: BuildInfo | undefined;
      let ice: RTCIceServer[] | undefined;
      const send = (data: object) => { guard(); if (socket?.readyState !== WebSocket.OPEN) throw new Error('Signaling disconnected'); socket.send(JSON.stringify(data)); };
      const signed = async (body: object) => {
        const bodyText = JSON.stringify(body); const seq = ++outgoing;
        const signature = await sign(identity, signalTranscript(origin, machineId, sessionId, seq, bodyText)); guard();
        send({ type: 'signal', sessionId, payload: JSON.stringify({ kind: 'signed', seq, body: bodyText, signature }) });
      };
      const offer = async () => {
        if (!accepted || !ice || offered) return;
        offered = true;
        pc = new RTCPeerConnection({ iceServers: ice, iceTransportPolicy: this.forceTurn ? 'relay' : 'all' });
        pc.addEventListener('connectionstatechange', () => { if (pc && ['failed', 'disconnected', 'closed'].includes(pc.connectionState)) fail(); });
        const control = pc.createDataChannel('herdr-control-v1', { ordered: true });
        service = new PeerService(pc, control, sessionId, () => { void (async () => {
          guard();
          // Re-pairing must not erase a previously saved custom name either.
          const previous = (await listComputers()).find(saved => saved.machineId === machineId && saved.publicKey === remoteKey) || computer;
          guard();
          const saved = withComputerName({ ...previous, machineId, publicKey: remoteKey, identity, version: connectorVersion }, connectorName);
          await saveComputer(saved); guard();
          this.target = { computer: saved }; // Capability is dropped only after authenticated control ready.
          this.attempts = 0; clearTimeout(deadline);
          this.update({ status: 'Connected', service, version: connectorVersion, build: connectorBuild, route: this.forceTurn ? 'TURN requested' : 'Peer connection' });
          // A fresh authenticated epoch renews expiring TURN credentials. Never replay input.
          if (iceExpiresAt) {
            const remaining = iceExpiresAt * 1000 - Date.now();
            const margin = Math.min(60_000, Math.max(1000, remaining / 10));
            renewal = setTimeout(fail, Math.max(1000, remaining - margin));
          }
          let stats: RTCStatsReport;
          try { stats = await pc!.getStats(); } catch { return; }
          guard();
          let route = 'Peer connection';
          stats.forEach(report => {
            if (report.type === 'candidate-pair' && report.state === 'succeeded' && report.nominated) {
              const local = stats.get(report.localCandidateId); const remote = stats.get(report.remoteCandidateId);
              route = local?.candidateType === 'relay' || remote?.candidateType === 'relay' ? 'Relayed (TURN)' : 'Direct';
            }
          });
          this.update({ status: 'Connected', service, version: connectorVersion, build: connectorBuild, route });
          // Direct sessions do not need TURN credentials or periodic interruption.
          // TURN refresh deliberately reconnects; in-place ICE renegotiation is not implemented.
          if (route === 'Direct') clearTimeout(renewal);
        })().catch(fail); }, fail);
        await pc.setLocalDescription(await pc.createOffer()); guard();
        await gatherIce(pc); guard();
        if (!pc.localDescription?.sdp) throw new Error('Missing local description');
        await signed({ kind: 'offer', sdp: pc.localDescription.sdp });
      };
      const process = async (event: MessageEvent) => {
        guard();
        if (typeof event.data !== 'string' || event.data.length > 256 * 1024) throw new Error('Invalid signaling message');
        const msg = JSON.parse(event.data);
        if (msg.type === 'challenge') {
          if (joined || typeof msg.nonce !== 'string') throw new Error('Duplicate challenge');
          joined = true; send({ type: 'join', machineId });
        } else if (msg.type === 'session') {
          if (!joined || sessionId || typeof msg.sessionId !== 'string' || !msg.sessionId || msg.sessionId.length > 256 || /[\r\n]/.test(msg.sessionId) || msg.machineId !== machineId) throw new Error('Invalid session');
          sessionId = msg.sessionId;
          const nonce = randomNonce();
          const transcript = authTranscript(origin, machineId, sessionId, identity.publicKey, nonce);
          const proof = pairing ? await pairProof(pairing.secret, transcript) : await sign(identity, transcript); guard();
          send({ type: 'signal', sessionId, payload: JSON.stringify({ kind: 'auth', mode: pairing ? 'pair' : 'resume', publicKey: identity.publicKey, nonce, proof }) });
        } else if (msg.type === 'ice') {
          if (!sessionId || msg.sessionId !== sessionId || ice || !Array.isArray(msg.iceServers) || msg.iceServers.length > 16) throw new Error('Invalid ICE credentials');
          ice = msg.iceServers.map((server: RTCIceServer) => {
            if (!Array.isArray(server.urls) || !server.urls.length || server.urls.some(url => typeof url !== 'string' || !/^(stun|stuns|turn|turns):/.test(url))) throw new Error('Invalid ICE server');
            return { urls: server.urls, username: server.username, credential: server.credential };
          });
          if (ice?.some(server => (server.urls as string[]).some(url => /^turns?:/.test(url)))) {
            if (!Number.isSafeInteger(msg.expiresAt) || msg.expiresAt <= Date.now() / 1000 || msg.expiresAt > Date.now() / 1000 + 86400) throw new Error('Invalid TURN expiry');
            iceExpiresAt = msg.expiresAt;
          }
          await offer();
        } else if (msg.type === 'signal') {
          if (!sessionId || msg.sessionId !== sessionId || typeof msg.payload !== 'string' || msg.payload.length > 256 * 1024) throw new Error('Invalid signaling envelope');
          const envelope = JSON.parse(msg.payload);
          if (envelope.kind !== 'signed' || !Number.isSafeInteger(envelope.seq) || envelope.seq <= incoming || typeof envelope.body !== 'string' || typeof envelope.signature !== 'string') throw new Error('Invalid signed envelope');
          if (!await verify(remoteKey, signalTranscript(origin, machineId, sessionId, envelope.seq, envelope.body), envelope.signature)) throw new Error('Invalid connector signature'); guard();
          incoming = envelope.seq;
          const body = JSON.parse(envelope.body);
          if (body.kind === 'accepted') {
            if (accepted || body.protocolVersion !== 1 || typeof body.version !== 'string') throw new Error('Invalid connector acceptance');
            connectorName = body.computerName;
            accepted = true; connectorBuild = normalizeBuild(body.build, body.version); connectorVersion = versionText(body.version) || ''; this.update({ status: 'Establishing private peer connection…' }); await offer();
          } else if (body.kind === 'answer') {
            if (!accepted || !offered || answered || !pc || typeof body.sdp !== 'string') throw new Error('Unexpected answer');
            answered = true; await pc.setRemoteDescription({ type: 'answer', sdp: body.sdp }); guard();
          } else throw new Error('Unexpected signed message');
        } else if (msg.type === 'ended' || msg.type === 'error') throw new Error('Signaling session ended');
        else throw new Error('Unknown relay message');
      };
      let queue = Promise.resolve();
      let queued = 0;
      socket.onmessage = event => {
        if (++queued > 16) { fail(); return; }
        queue = queue.then(() => process(event)).catch(fail).finally(() => queued--);
      };
      socket.onerror = fail; socket.onclose = fail;
      deadline = setTimeout(fail, 45_000);
    } catch { fail(); }
  }
}
export function gatherIce(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve, reject) => {
    const done = () => { clearTimeout(timer); pc.removeEventListener('icegatheringstatechange', changed); pc.removeEventListener('connectionstatechange', closed); };
    const changed = () => { if (pc.iceGatheringState === 'complete') { done(); resolve(); } };
    const closed = () => { if (pc.connectionState === 'closed' || pc.connectionState === 'failed') { done(); reject(new Error('Peer closed')); } };
    const timer = setTimeout(() => { done(); reject(new Error('ICE gathering timed out')); }, 15_000);
    pc.addEventListener('icegatheringstatechange', changed); pc.addEventListener('connectionstatechange', closed); changed();
  });
}
