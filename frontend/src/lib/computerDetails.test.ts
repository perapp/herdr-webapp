import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import { computerIssue } from './computerDetails';
import ComputerDetailsDialog from '../ComputerDetailsDialog.svelte';

const dns = JSON.stringify({ code: -32000, data: { hint: 'Check hc machine ls.', kind: 'unavailable' }, message: 'aidev is unreachable: ssh: Could not resolve hostname example.test: nodename nor servname provided' });
describe('computer connection details', () => {
  it('turns native RPC JSON into a readable DNS explanation with separate technical details', () => {
    expect(computerIssue(dns)).toMatchObject({ title: 'Hostname not found', disconnected: true, hint: 'Check hc machine ls.', technical: dns });
    expect(computerIssue(dns)?.message).not.toContain('{');
    expect(computerIssue(dns)?.recovery).toContain('VPN');
    expect(computerIssue(undefined)).toBeUndefined();
  });
  it('supports wrapped RPC errors, legacy strings and malformed/hostile responses', () => {
    expect(computerIssue(JSON.stringify({ error: JSON.parse(dns) }))?.title).toBe('Hostname not found');
    expect(computerIssue('Connection refused')?.title).toBe('Connection refused');
    expect(computerIssue('Connection timed out')?.title).toBe('Computer did not respond');
    expect(computerIssue('Permission denied (publickey)')?.title).toBe('Connection not authorized');
    expect(computerIssue('{broken')?.message).toBe('{broken');
    expect(computerIssue('x'.repeat(20000))?.technical).toHaveLength(16384);
  });
  it('renders a disconnected computer with escaped expandable details, not raw JSON as its main message', () => {
    const html = render(ComputerDetailsDialog, { props: { name: '<script>host</script>', on: 'aidev', document: { on: 'aidev', version: 0, spaces: [], error: dns }, connected: true, refresh: async () => {}, close: () => {} } }).body;
    expect(html).toContain('Hostname not found');
    expect(html).toContain('Technical details');
    expect(html).toContain('Disconnected');
    expect(html).toContain('&lt;script>host&lt;/script>');
    expect(html).not.toContain('<script>host</script>');
    expect(html).not.toContain('Layout unavailable:');
  });
});
