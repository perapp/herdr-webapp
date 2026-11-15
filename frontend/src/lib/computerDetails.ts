import { record, readable } from './schema';

export interface ComputerIssue { title: string; message: string; recovery: string; hint?: string; technical: string; disconnected: boolean }
/** Decode native RPC envelopes without showing JSON or interpreting it as markup. */
export function computerIssue(error: string | undefined): ComputerIssue | undefined {
  if (!error) return;
  const technical = error.slice(0, 16384);
  let value: unknown = technical;
  try { value = JSON.parse(technical); } catch { /* Older cores report plain text. */ }
  if (record(value) && record(value.error)) value = value.error;
  const data = record(value) && record(value.data) ? value.data : undefined;
  const message = readable(record(value) ? value.message : value, 'The computer’s layout could not be loaded.');
  const hint = typeof data?.hint === 'string' ? readable(data.hint, '') : undefined;
  let title = 'Layout unavailable', summary = message;
  let recovery = 'Refresh the status. If the problem continues, check Herdr Core on this computer.';
  let disconnected = false;
  if (/could not resolve hostname|name or service not known|nodename nor servname|name resolution/i.test(message)) {
    title = 'Hostname not found'; summary = 'The computer’s hostname could not be resolved.';
    recovery = 'Check its address and your network or VPN connection. Herdr will reconnect when it becomes reachable.'; disconnected = true;
  } else if (/permission denied|authentication failed|publickey/i.test(message)) {
    title = 'Connection not authorized'; summary = 'Herdr Core could not authenticate with this computer.';
    recovery = 'Check the computer’s connection credentials in Herdr Core, then refresh the status.'; disconnected = true;
  } else if (/connection refused/i.test(message)) {
    title = 'Connection refused'; summary = 'The computer is not accepting connections.';
    recovery = 'Check that it is running and its connection service is available, then refresh the status.'; disconnected = true;
  } else if (/timed? out|timeout/i.test(message)) {
    title = 'Computer did not respond'; summary = 'The connection attempt timed out.';
    recovery = 'Check the computer and your network or VPN connection. Herdr will reconnect when it becomes reachable.'; disconnected = true;
  } else if (data?.kind === 'unavailable' || /unreachable|cannot reach|disconnected|offline/i.test(message)) {
    title = 'Computer disconnected';
    recovery = 'Check that the computer and Herdr Core are running and reachable. Herdr will reconnect automatically.'; disconnected = true;
  }
  return { title, message: summary, recovery, hint, technical, disconnected };
}
