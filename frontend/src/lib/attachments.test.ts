import { afterEach, describe, expect, it, vi } from 'vitest';
import { imageReference, maxImageBytes, uploadImage, validateImage } from './attachments';

const png = () => new File([new Uint8Array([137, 80, 78, 71])], 'screen.png', { type: 'image/png' });
afterEach(() => vi.unstubAllGlobals());
describe('image attachments', () => {
  it('bounds images and explicitly rejects unsupported phone formats', () => {
    expect(() => validateImage(png())).not.toThrow();
    expect(() => validateImage({ size: 1, type: '' })).not.toThrow();
    expect(() => validateImage({ size: maxImageBytes, type: 'image/webp' })).not.toThrow();
    expect(() => validateImage({ size: maxImageBytes + 1, type: 'image/png' })).toThrow('8 MiB');
    expect(() => validateImage({ size: 0, type: 'image/png' })).toThrow('empty');
    expect(() => validateImage({ size: 1, type: 'image/heic' })).toThrow('PNG, JPEG, GIF or WebP');
    expect(() => validateImage({ size: 1, type: 'image/svg+xml' })).toThrow();
  });
  it('formats a path reference without submitting or injecting control characters', () => {
    expect(imageReference('/private/screen.png')).toBe(' "/private/screen.png" ');
    expect(imageReference('/private/a b.png')).toBe(' "/private/a b.png" ');
    for (const path of [null, '', 'x\r', 'x\n', 'x\x1b[31m', 'x\x7f', 'x'.repeat(4097)]) {
      expect(() => imageReference(path)).toThrow('invalid image path');
    }
  });
  it('uploads raw bytes to the exact selected remote terminal, never an arbitrary path', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ path: '/remote/image.png' })));
    vi.stubGlobal('fetch', fetch);
    const signal = new AbortController().signal, file = png();
    expect(await uploadImage('beta/t7', file, signal)).toBe('/remote/image.png');
    const [url, options] = fetch.mock.calls[0];
    expect(new URL(url, 'http://app/').searchParams.get('terminal')).toBe('beta/t7');
    expect(options).toMatchObject({ method: 'POST', credentials: 'same-origin', body: file, signal, redirect: 'error', cache: 'no-store' });
    expect(options.headers).toEqual({ 'Content-Type': 'application/octet-stream' });
  });
  it('never starts a request for invalid images', async () => {
    const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
    await expect(uploadImage('t7', new File([], 'empty.png'), new AbortController().signal)).rejects.toThrow('empty');
    expect(fetch).not.toHaveBeenCalled();
  });
  it('presents backend failures without retrying or claiming an upload succeeded', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { message: 'Remote machine unavailable; upload may exist.' } }), { status: 502 }));
    vi.stubGlobal('fetch', fetch);
    await expect(uploadImage('beta/t7', png(), new AbortController().signal)).rejects.toThrow('upload may exist');
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it('rejects bad server paths and unreadable upload responses', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ path: 'bad\rpath' }))).mockResolvedValueOnce(new Response('<html>proxy error</html>', { status: 502 }));
    vi.stubGlobal('fetch', fetch);
    await expect(uploadImage('t7', png(), new AbortController().signal)).rejects.toThrow('invalid image path');
    await expect(uploadImage('t7', png(), new AbortController().signal)).rejects.toThrow('unreadable upload response');
  });
});
