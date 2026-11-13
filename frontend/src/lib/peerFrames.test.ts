import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  FramedChannel, encodeFrame, decodeFrame, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES,
  MAX_BUFFERED_BYTES, PARTIAL_TIMEOUT_MS, SEND_TIMEOUT_MS,
} from './peerFrames';

class Channel extends EventTarget {
  ordered = true;
  maxRetransmits: number | null = null;
  maxPacketLifeTime: number | null = null;
  readyState = 'open';
  binaryType = 'blob';
  bufferedAmountLowThreshold = 0;
  bufferedAmount = 0;
  frames: ArrayBuffer[] = [];
  send(frame: ArrayBuffer): void {
    this.frames.push(frame);
    this.bufferedAmount += frame.byteLength;
  }
  close(): void { this.readyState = 'closed'; this.dispatchEvent(new Event('close')); }
  flush(): void { this.bufferedAmount = 0; this.dispatchEvent(new Event('bufferedamountlow')); }
  receive(data: unknown): void { this.dispatchEvent(new MessageEvent('message', { data })); }
}
function setup() {
  const channel = new Channel();
  const framed = new FramedChannel(channel as unknown as RTCDataChannel);
  const onmessage = vi.fn();
  const onerror = vi.fn();
  framed.onmessage = onmessage;
  framed.onerror = onerror;
  return { channel, framed, onmessage, onerror };
}
function frame(id = 1, total = 2, offset = 0, payload = new Uint8Array([65]), kind: 0 | 1 = 1) {
  return encodeFrame({ id, total, offset, payload, kind });
}
afterEach(() => vi.useRealTimers());

describe('peer frame codec', () => {
  it('uses the exact 17-byte HDR1 little-endian header', () => {
    const bytes = new Uint8Array(frame(0x12345678, 3, 1, new Uint8Array([7, 8]), 0));
    expect([...bytes]).toEqual([72, 68, 82, 1, 0x78, 0x56, 0x34, 0x12, 3, 0, 0, 0, 1, 0, 0, 0, 0, 7, 8]);
    expect(decodeFrame(bytes.buffer)).toMatchObject({ id: 0x12345678, total: 3, offset: 1, kind: 0 });
  });
  it('rejects invalid sizes, magic, kind, total, IDs, offsets and empty nonterminal chunks', () => {
    for (const size of [0, 16, MAX_FRAME_BYTES + 1]) expect(() => decodeFrame(new ArrayBuffer(size))).toThrow();
    for (const [byte, value] of [[0, 0], [3, 2], [16, 2]]) {
      const bytes = new Uint8Array(frame()); bytes[byte] = value;
      expect(() => decodeFrame(bytes.buffer)).toThrow();
    }
    for (const values of [
      { id: 0 }, { total: MAX_MESSAGE_BYTES + 1 }, { offset: 3 },
      { payload: new Uint8Array(0) }, { payload: new Uint8Array(MAX_FRAME_BYTES) },
    ]) expect(() => encodeFrame({ id: 1, total: 2, offset: 0, kind: 1, payload: new Uint8Array([1]), ...values })).toThrow();
    expect(decodeFrame(frame(1, MAX_MESSAGE_BYTES)).total).toBe(MAX_MESSAGE_BYTES);
  });
});

describe('FramedChannel', () => {
  it('frames all text/binary/empty messages and preserves Uint8Array slices', async () => {
    const { channel, framed, onmessage } = setup();
    await framed.send('hello 🌍');
    await framed.send(new Uint8Array([1, 2, 3]).subarray(1));
    await framed.send(new ArrayBuffer(0));
    await framed.send('');
    expect(channel.binaryType).toBe('arraybuffer');
    for (const bytes of channel.frames) channel.receive(bytes);
    expect(onmessage.mock.calls.map(([data]) => typeof data === 'string' ? data : [...new Uint8Array(data)])).
      toEqual(['hello 🌍', [2, 3], [], '']);
    expect(channel.frames.map(bytes => decodeFrame(bytes).id)).toEqual([1, 2, 3, 4]);
    framed.close();
  });
  it('splits large payloads at 16384 bytes including header and reassembles split UTF8', async () => {
    const { channel, framed, onmessage } = setup();
    const text = 'x'.repeat(16_366) + '🌍'.repeat(10_000);
    await framed.send(text);
    expect(channel.frames.length).toBeGreaterThan(1);
    expect(Math.max(...channel.frames.map(bytes => bytes.byteLength))).toBe(MAX_FRAME_BYTES);
    for (const bytes of channel.frames) channel.receive(bytes);
    expect(onmessage).toHaveBeenCalledExactlyOnceWith(text);
    framed.close();
  });
  it.each(['replay', 'gap', 'overlap', 'total', 'kind', 'id', 'text', 'utf8'])('closes on %s violations', mode => {
    const { channel, framed, onerror } = setup();
    if (mode === 'text') channel.receive('unframed');
    else if (mode === 'utf8') channel.receive(frame(1, 1, 0, new Uint8Array([255]), 0));
    else if (mode === 'replay') {
      channel.receive(frame(1, 1)); channel.receive(frame(1, 1));
    } else {
      channel.receive(frame());
      if (mode === 'gap') channel.receive(frame(1, 3, 2));
      if (mode === 'overlap') channel.receive(frame());
      if (mode === 'total') channel.receive(frame(1, 3, 1));
      if (mode === 'kind') channel.receive(frame(1, 2, 1, new Uint8Array([65]), 0));
      if (mode === 'id') channel.receive(frame(2, 2, 1));
    }
    expect(channel.readyState).toBe('closed');
    expect(onerror).toHaveBeenCalledTimes(1);
    framed.close();
  });
  it('permits bounded interleaved partials but rejects decreasing new IDs', () => {
    const { channel, framed, onmessage, onerror } = setup();
    channel.receive(frame(2)); channel.receive(frame(3));
    channel.receive(frame(2, 2, 1)); channel.receive(frame(3, 2, 1));
    expect(onmessage).toHaveBeenCalledTimes(2);
    channel.receive(frame(1));
    expect(onerror).toHaveBeenCalledTimes(1);
    framed.close();
  });
  it.each(['count', 'bytes'])('bounds partial %s', mode => {
    const { channel, onerror } = setup();
    const total = mode === 'bytes' ? MAX_MESSAGE_BYTES : 2;
    const count = mode === 'bytes' ? 3 : 5;
    for (let id = 1; id <= count; id++) channel.receive(frame(id, total));
    expect(onerror).toHaveBeenCalledTimes(1);
    expect(channel.readyState).toBe('closed');
  });
  it('expires partials after 10s without extending the deadline for progress', () => {
    vi.useFakeTimers();
    const { channel, onerror } = setup();
    channel.receive(frame(1, 3));
    vi.advanceTimersByTime(PARTIAL_TIMEOUT_MS - 1);
    channel.receive(frame(1, 3, 1));
    expect(onerror).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onerror).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ message: 'Peer partial message timed out' }));
  });
  it('bounds buffered output, resumes on low buffer, and snapshots caller data', async () => {
    const { channel, framed } = setup();
    const bytes = new Uint8Array(MAX_BUFFERED_BYTES * 2).fill(7);
    const sent = framed.send(bytes);
    bytes.fill(9);
    expect(channel.bufferedAmount).toBeLessThanOrEqual(MAX_BUFFERED_BYTES);
    channel.flush(); channel.flush();
    await sent;
    expect(channel.frames.every(bytes => decodeFrame(bytes).payload.every(byte => byte === 7))).toBe(true);
    expect(channel.bufferedAmount).toBeLessThanOrEqual(MAX_BUFFERED_BYTES);
    framed.close();
  });
  it('times out blocked sends and rejects all queued work without replay', async () => {
    vi.useFakeTimers();
    const { channel, framed, onerror } = setup();
    channel.bufferedAmount = MAX_BUFFERED_BYTES;
    const first = expect(framed.send('one')).rejects.toThrow('timed out');
    const second = expect(framed.send('two')).rejects.toThrow('timed out');
    vi.advanceTimersByTime(SEND_TIMEOUT_MS);
    await Promise.all([first, second]);
    channel.flush();
    expect(channel.frames).toHaveLength(0);
    expect(onerror).toHaveBeenCalledTimes(1);
    await expect(framed.send('offline')).rejects.toThrow('closed');
  });
  it.each(['bytes', 'count'])('bounds outgoing queued %s', async mode => {
    const { channel, framed } = setup();
    channel.bufferedAmount = MAX_BUFFERED_BYTES;
    const queued: Promise<void>[] = [];
    const count = mode === 'bytes' ? 3 : 5;
    for (let i = 0; i < count; i++) queued.push(framed.send(mode === 'bytes' ? new Uint8Array(MAX_MESSAGE_BYTES) : '').catch(() => {}));
    await Promise.all(queued);
    expect(channel.readyState).toBe('closed');
  });
  it('rejects offline sends, oversized messages and unreliable transports', async () => {
    const { channel, framed } = setup();
    channel.readyState = 'connecting';
    await expect(framed.send('offline')).rejects.toThrow('not open');
    expect(channel.frames).toHaveLength(0);
    const large = setup();
    await expect(large.framed.send(new Uint8Array(MAX_MESSAGE_BYTES + 1))).rejects.toThrow('too large');
    for (const values of [{ ordered: false }, { maxRetransmits: 1 }, { maxPacketLifeTime: 1 }]) {
      const unreliable = Object.assign(new Channel(), values);
      expect(() => new FramedChannel(unreliable as unknown as RTCDataChannel)).toThrow('reliable ordered');
    }
  });
  it('cleans timers and pending sends on explicit/remote close and errors', async () => {
    vi.useFakeTimers();
    for (const mode of ['explicit', 'remote', 'error']) {
      const { channel, framed, onerror } = setup();
      channel.receive(frame());
      channel.bufferedAmount = MAX_BUFFERED_BYTES;
      const pending = expect(framed.send('pending')).rejects.toThrow();
      if (mode === 'explicit') framed.close();
      else if (mode === 'remote') channel.close();
      else channel.dispatchEvent(new Event('error'));
      await pending;
      expect(vi.getTimerCount()).toBe(0);
      expect(onerror).toHaveBeenCalledTimes(mode === 'explicit' ? 0 : 1);
    }
  });
});
