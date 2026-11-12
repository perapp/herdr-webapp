import { ApiError } from './api';
import { record } from './schema';

export const maxImageBytes = 8 * 1024 * 1024;
export const imageAccept = 'image/png,image/jpeg,image/gif,image/webp,.png,.jpg,.jpeg,.gif,.webp';
export function validateImage(file: Pick<File, 'size' | 'type'>): void {
  if (!file.size) throw new Error('This image is empty. Choose another image.');
  if (file.size > maxImageBytes) throw new Error('Choose an image no larger than 8 MiB.');
  // Some phone pickers omit MIME; the server checks the actual file format.
  if (file.type && !['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(file.type)) {
    throw new Error('Choose a PNG, JPEG, GIF or WebP image.');
  }
}
/** A literal path, never a newline or a command. Pi can read it with its file tool. */
export function imageReference(path: unknown): string {
  if (typeof path !== 'string' || !path || path.length > 4096 || [...path].some(c => c < ' ' || c === '\x7f')) {
    throw new Error('The server returned an invalid image path.');
  }
  return ` ${JSON.stringify(path)} `;
}
export async function uploadImage(address: string, file: File, signal: AbortSignal): Promise<string> {
  validateImage(file);
  const query = new URLSearchParams({ terminal: address });
  const response = await fetch(`/api/attachments?${query}`, {
    method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/octet-stream' },
    body: file, signal, cache: 'no-store', redirect: 'error',
  });
  let data: unknown;
  try { data = await response.json(); }
  catch { throw new ApiError('The server returned an unreadable upload response.', response.status); }
  if (!response.ok) {
    const message = record(data) && record(data.error) && typeof data.error.message === 'string' ? data.error.message : `Image upload failed (${response.status}).`;
    throw new ApiError(message, response.status);
  }
  const path = record(data) ? data.path : undefined;
  imageReference(path);
  return path as string;
}
