// Wire format shared with the connector: HDR1 + little-endian id/total/offset + kind.
export const FRAME_HEADER_BYTES = 17;
export const MAX_FRAME_BYTES = 16_384;
export const MAX_MESSAGE_BYTES = 16 * 1024 * 1024;
export const MAX_PARTIAL_BYTES = 32 * 1024 * 1024;
export const MAX_PARTIAL_MESSAGES = 4;
export const MAX_BUFFERED_BYTES = 512 * 1024;
export const PARTIAL_TIMEOUT_MS = 10_000;
export const SEND_TIMEOUT_MS = 2_000;
const PAYLOAD_BYTES = MAX_FRAME_BYTES - FRAME_HEADER_BYTES;
const magic = [72, 68, 82, 1];

export interface PeerFrame {
  id: number;
  total: number;
  offset: number;
  kind: 0 | 1;
  payload: Uint8Array;
}

function validate(frame: PeerFrame): void {
  const { id, total, offset, kind, payload } = frame;
  if (!Number.isInteger(id) || id < 1 || id > 0xffffffff ||
      !Number.isInteger(total) || total < 0 || total > MAX_MESSAGE_BYTES ||
      !Number.isInteger(offset) || offset < 0 || offset > total ||
      (kind !== 0 && kind !== 1) || payload.byteLength > PAYLOAD_BYTES ||
      offset + payload.byteLength > total ||
      (total > 0 && payload.byteLength === 0)) throw new Error('Invalid peer frame');
}

export function encodeFrame(frame: PeerFrame): ArrayBuffer {
  validate(frame);
  const bytes = new Uint8Array(FRAME_HEADER_BYTES + frame.payload.byteLength);
  bytes.set(magic);
  const view = new DataView(bytes.buffer);
  view.setUint32(4, frame.id, true);
  view.setUint32(8, frame.total, true);
  view.setUint32(12, frame.offset, true);
  bytes[16] = frame.kind;
  bytes.set(frame.payload, FRAME_HEADER_BYTES);
  return bytes.buffer;
}

export function decodeFrame(buffer: ArrayBuffer): PeerFrame {
  if (buffer.byteLength < FRAME_HEADER_BYTES || buffer.byteLength > MAX_FRAME_BYTES) {
    throw new Error('Invalid peer frame size');
  }
  const bytes = new Uint8Array(buffer);
  if (!magic.every((byte, i) => bytes[i] === byte)) throw new Error('Invalid peer frame magic');
  const view = new DataView(buffer);
  const frame: PeerFrame = {
    id: view.getUint32(4, true), total: view.getUint32(8, true),
    offset: view.getUint32(12, true), kind: bytes[16] as 0 | 1,
    payload: bytes.subarray(FRAME_HEADER_BYTES),
  };
  validate(frame);
  return frame;
}

interface PartialMessage {
  bytes: Uint8Array;
  offset: number;
  kind: 0 | 1;
  timer: ReturnType<typeof setTimeout>;
}
interface Outgoing {
  id: number;
  bytes: Uint8Array;
  offset: number;
  kind: 0 | 1;
  resolve: () => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** One reliable ordered channel, one connection epoch. Never queues offline data. */
export class FramedChannel {
  onmessage: ((data: string | ArrayBuffer) => void) | null = null;
  onerror: ((error: Error) => void) | null = null;
  private readonly partials = new Map<number, PartialMessage>();
  private partialBytes = 0;
  private lastIncomingId = 0;
  private nextOutgoingId = 1;
  private readonly outgoing: Outgoing[] = [];
  private outgoingBytes = 0;
  private stopped = false;
  private draining = false;

  constructor(private readonly channel: RTCDataChannel) {
    if (!channel.ordered || channel.maxRetransmits != null || channel.maxPacketLifeTime != null) {
      channel.close();
      throw new Error('Peer framing requires a reliable ordered channel');
    }
    channel.binaryType = 'arraybuffer';
    channel.bufferedAmountLowThreshold = MAX_BUFFERED_BYTES / 2;
    channel.addEventListener('message', this.receive);
    channel.addEventListener('bufferedamountlow', this.drain);
    channel.addEventListener('close', this.channelClosed);
    channel.addEventListener('error', this.channelError);
  }

  send(data: string | ArrayBuffer | Uint8Array): Promise<void> {
    if (this.stopped) return Promise.reject(new Error('Peer channel is closed'));
    try {
      if (this.channel.readyState !== 'open') throw new Error('Peer channel is not open');
      if (this.outgoing.length >= MAX_PARTIAL_MESSAGES) throw new Error('Peer send queue limit exceeded');
      const size = typeof data === 'string' ? data.length : data.byteLength;
      if (size > MAX_MESSAGE_BYTES) throw new Error('Peer message too large');
      const bytes = typeof data === 'string' ? new TextEncoder().encode(data) :
        data instanceof Uint8Array ? new Uint8Array(data) : new Uint8Array(data.slice(0));
      if (bytes.byteLength > MAX_MESSAGE_BYTES || this.outgoingBytes + bytes.byteLength > MAX_PARTIAL_BYTES) {
        throw new Error('Peer send byte limit exceeded');
      }
      if (this.nextOutgoingId > 0xffffffff) throw new Error('Peer message IDs exhausted');
      const id = this.nextOutgoingId++;
      return new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => this.fail(new Error('Peer send timed out')), SEND_TIMEOUT_MS);
        this.outgoing.push({ id, bytes, offset: 0, kind: typeof data === 'string' ? 0 : 1, resolve, reject, timer });
        this.outgoingBytes += bytes.byteLength;
        this.drain();
      });
    } catch (error) {
      const failure = error instanceof Error ? error : new Error('Peer send failed');
      this.fail(failure);
      return Promise.reject(failure);
    }
  }

  close(): void {
    this.stop(new Error('Peer channel is closed'));
  }

  private readonly channelClosed = (): void => { this.fail(new Error('Peer channel closed')); };
  private readonly channelError = (): void => { this.fail(new Error('Peer channel error')); };

  private readonly receive = (event: MessageEvent): void => {
    if (this.stopped) return;
    let delivered: string | ArrayBuffer | undefined;
    try {
      if (!(event.data instanceof ArrayBuffer)) throw new Error('Peer frames must be binary');
      const frame = decodeFrame(event.data);
      let partial = this.partials.get(frame.id);
      if (!partial) {
        if (frame.offset !== 0 || frame.id <= this.lastIncomingId) throw new Error('Peer frame replay or invalid ID');
        if (this.partials.size >= MAX_PARTIAL_MESSAGES || this.partialBytes + frame.total > MAX_PARTIAL_BYTES) {
          throw new Error('Peer partial message limit exceeded');
        }
        this.lastIncomingId = frame.id;
        partial = {
          bytes: new Uint8Array(frame.total), offset: 0, kind: frame.kind,
          timer: setTimeout(() => this.fail(new Error('Peer partial message timed out')), PARTIAL_TIMEOUT_MS),
        };
        this.partials.set(frame.id, partial);
        this.partialBytes += frame.total;
      }
      if (frame.total !== partial.bytes.byteLength || frame.kind !== partial.kind || frame.offset !== partial.offset) {
        throw new Error('Inconsistent or nonsequential peer frame');
      }
      partial.bytes.set(frame.payload, partial.offset);
      partial.offset += frame.payload.byteLength;
      if (partial.offset === partial.bytes.byteLength) {
        clearTimeout(partial.timer);
        this.partials.delete(frame.id);
        this.partialBytes -= partial.bytes.byteLength;
        delivered = partial.kind === 0 ? new TextDecoder('utf-8', { fatal: true }).decode(partial.bytes) :
          partial.bytes.buffer as ArrayBuffer;
      }
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error('Peer receive failed'));
      return;
    }
    if (delivered !== undefined) this.onmessage?.(delivered);
  };

  private readonly drain = (): void => {
    if (this.stopped || this.draining) return;
    this.draining = true;
    try {
      while (this.outgoing.length) {
        if (this.channel.readyState !== 'open') throw new Error('Peer channel is not open');
        if (this.channel.bufferedAmount > MAX_BUFFERED_BYTES) throw new Error('Peer buffer limit exceeded');
        const job = this.outgoing[0];
        const end = Math.min(job.offset + PAYLOAD_BYTES, job.bytes.byteLength);
        const frameSize = FRAME_HEADER_BYTES + end - job.offset;
        if (this.channel.bufferedAmount + frameSize > MAX_BUFFERED_BYTES) break;
        this.channel.send(encodeFrame({ id: job.id, total: job.bytes.byteLength, offset: job.offset,
          kind: job.kind, payload: job.bytes.subarray(job.offset, end) }));
        job.offset = end;
        if (end === job.bytes.byteLength) {
          this.outgoing.shift();
          this.outgoingBytes -= job.bytes.byteLength;
          clearTimeout(job.timer);
          job.resolve();
        }
      }
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error('Peer send failed'));
    } finally {
      this.draining = false;
    }
  };

  private fail(error: Error): void {
    if (this.stopped) return;
    this.stop(error);
    this.onerror?.(error);
  }

  private stop(error: Error): void {
    if (this.stopped) return;
    this.stopped = true;
    this.channel.removeEventListener('message', this.receive);
    this.channel.removeEventListener('bufferedamountlow', this.drain);
    this.channel.removeEventListener('close', this.channelClosed);
    this.channel.removeEventListener('error', this.channelError);
    for (const partial of this.partials.values()) clearTimeout(partial.timer);
    this.partials.clear();
    this.partialBytes = 0;
    for (const job of this.outgoing.splice(0)) {
      clearTimeout(job.timer);
      job.reject(error);
    }
    this.outgoingBytes = 0;
    this.channel.close();
  }
}
