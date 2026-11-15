import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import { readFileSync } from 'node:fs';
import AgentIndicator from '../AgentIndicator.svelte';

describe('native default agent indicators', () => {
  const html = (status: string, showIdle = true) => render(AgentIndicator, { props: { status, showIdle } }).body;
  it('uses the native spinner, Needs you badge, done dot and idle ring independently of program logos', () => {
    expect(html('working')).toContain('aria-label="Working"');
    expect(html('working')).toContain('M12 2.6a9.4 9.4 0 0 1 9.4 9.4');
    expect(html('blocked')).toContain('Needs you</span>');
    expect(html('done')).toContain('aria-label="Finished"');
    expect(html('idle')).toContain('data-status="idle"');
    expect(html('idle', false)).not.toContain('role="status"');
    expect(html('unreported')).not.toContain('role="status"');
  });
  it('keeps native paper/graphite semantic colors independent from chrome accents', () => {
    const css = readFileSync(new URL('../style.css', import.meta.url), 'utf8');
    for (const token of ['--agent-working:#9a6700', '--agent-blocked:#c93c2c', '--agent-done:#2f6fe4', '--agent-idle:#acadaf', '--agent-working:#e5c07b', '--agent-blocked:#f0705f', '--agent-done:#76a8ff', '--agent-idle:#5e6064']) expect(css).toContain(token);
    const component = readFileSync(new URL('../AgentIndicator.svelte', import.meta.url), 'utf8');
    expect(component).toContain('900ms');
    expect(component).toContain('prefers-reduced-motion:reduce');
  });
});
