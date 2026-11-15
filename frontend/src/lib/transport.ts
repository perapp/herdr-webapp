import { channelInput, channelPath } from './channel';
// Structural facades retain native local behavior; relay mode never falls back to HTTP.
export interface SocketLike {
  readonly readyState: number;
  binaryType: BinaryType;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
}
export interface EventSourceLike {
  onopen: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
  addEventListener(type: string, listener: (event: MessageEvent) => void): void;
  close(): void;
}
export interface ApplicationTransport {
  request(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  events(path: string): EventSourceLike;
  socket(url: string | URL): SocketLike;
}
export const disconnectedTransport: ApplicationTransport = {
  request: () => Promise.reject(new Error('Computer disconnected')),
  events: () => { throw new Error('Computer disconnected'); },
  socket: () => { throw new Error('Computer disconnected'); },
};
let active: ApplicationTransport | undefined;
export function setApplicationTransport(transport: ApplicationTransport | undefined): void { active = transport; }
export function transportRequest(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  return active ? active.request(input, init) : fetch(channelInput(input), init);
}
/** Bind asynchronous settings work to its authenticated computer, never a later selection. */
export function captureTransportRequest(): typeof transportRequest {
  const transport = active;
  return transport ? (input, init) => transport.request(input, init) : (input, init) => fetch(channelInput(input), init);
}
export function transportEvents(path: string): EventSourceLike {
  return active ? active.events(path) : new EventSource(channelPath(path));
}
export function transportSocket(url: string | URL): SocketLike {
  return active ? active.socket(url) : new WebSocket(channelInput(url) as string | URL);
}
