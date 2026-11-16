import { describe, expect, it } from 'vitest';
import { qrImage } from './qr';

describe('access link QR', () => {
  it('draws a square code with a quiet zone and the three finder patterns', async () => {
    const image = await qrImage(`https://relay.example/main/#pair=${'A'.repeat(400)}`);
    expect(image.size).toBeGreaterThan(21 + 8);
    // Finder patterns start at the quiet-zone offset in three corners.
    expect(image.path).toContain('M4 4h1v1h-1z');
    expect(image.path).toContain(`M${image.size - 5} 4h1v1h-1z`);
    expect(image.path).toContain(`M4 ${image.size - 5}h1v1h-1z`);
    expect((await qrImage('x')).size).toBe(21 + 8);
  });
});
