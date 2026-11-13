import { record } from './schema';

export class ApiError extends Error {
  constructor(message: string, public status: number) { super(message); }
}
import { transportRequest } from './transport';
export async function request(path: string, body?: unknown, signal?: AbortSignal): Promise<unknown> {
  const response = await transportRequest(path, {
    method: body === undefined ? 'GET' : 'POST', credentials: 'same-origin',
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body), signal,
    cache: 'no-store', redirect: 'error',
  });
  const text = await response.text();
  let data: unknown;
  try { data = text ? JSON.parse(text) : {}; }
  catch {
    // Older servers and reverse proxies may send plain-text errors. Preserve
    // the useful reason, not a misleading JSON parsing error. Render as text.
    const message = !response.ok && text.trim() ? text.trim().slice(0, 2048) : 'The server returned an unreadable response.';
    throw new ApiError(message, response.status);
  }
  const error = record(data) && record(data.error) ? data.error.message : record(data) ? data.error : undefined;
  if (!response.ok || error) throw new ApiError(typeof error === 'string' ? error : `Request failed (${response.status}).`, response.status);
  return data;
}
export function errorMessage(error: unknown): string { return error instanceof Error ? error.message : 'The request could not be completed.'; }
