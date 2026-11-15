import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import ProgramIcon from '../ProgramIcon.svelte';
import catalog from './programs.catalog.json';
import { programIconUrl, resolveProgram, terminalProgram } from './programs';

describe('native shipped program resolution', () => {
  it.each([
    ['claude-code', ['/bin/zsh'], 'claude'], ['GROK-BUILD', [], 'grok'],
    ['dsh-tui', ['/bin/zsh'], 'dsh-tui'], ['newagent', ['codex'], 'agent'],
    [null, ['/bin/zsh'], 'shell'], [null, ['-zsh'], 'shell'],
    [null, ['nvim', 'x.rs'], 'nvim'], [null, ['vi'], 'vim'],
    [null, ['node', '/x/node_modules/.bin/vite', 'dev'], 'vite'],
    [null, ['npx', 'vitest'], 'vitest'], [null, ['node', 'server.js'], 'node'],
    [null, ['python3', '-m', 'http.server', '8799'], 'python'],
    [null, ['uv', 'run', 'pytest'], 'pytest'], [null, ['mytool'], 'program'],
    [null, ['/bin/zsh', '-ic', '"$@"; exec "$0" -i', '/bin/zsh', 'vi', 'x'], 'vim'],
    [null, ['/usr/bin/fish', '-ic', 'command $argv[2..]; exec $argv[1] -i', '/usr/bin/fish', 'vi', 'x'], 'vim'],
    [null, ['pwsh.exe', '-NoExit', '-Command', "& 'codex' 'fix it'"], 'codex'],
    [null, ['C:\\Windows\\powershell.exe'], 'shell'],
    [null, ['C:\\tools\\CODEX.EXE'], 'codex'],
    [null, ['dsh-tui'], 'dsh-tui'],
  ] as const)('resolves %s / %j to %s', (app, argv, key) => {
    expect(resolveProgram(app, argv).key).toBe(key);
  });

  it('preserves native flags, names, tint and fallback semantics', () => {
    expect(resolveProgram('claude-code')).toMatchObject({ name: 'Claude Code', agent: true, shell: false, tint: false });
    expect(resolveProgram(null, ['claude'])).toMatchObject({ agent: false, shell: false });
    expect(resolveProgram(null, [])).toMatchObject({ name: 'Shell', shell: true, agent: false });
    expect(resolveProgram(null, ['-bash'])).toMatchObject({ name: 'bash', shell: true });
    expect(resolveProgram('  ')).toMatchObject({ key: 'agent', name: '  ', agent: true });
    expect(resolveProgram('PI')).toMatchObject({ key: 'pi', name: 'Pi', tint: true });
    // Explicit catalog keys win over aliases; wrappers are case-sensitive in native code.
    expect(resolveProgram(null, ['pnpm'])).toMatchObject({ key: 'pnpm' });
    expect(resolveProgram(null, ['NODE', 'vite'])).toMatchObject({ key: 'node' });
  });

  it('uses foreground argv before launch argv; status.app before either, never names/models', () => {
    const terminal = { argv: ['bash'], foreground: { argv: ['node', 'vite.js'] }, status: { app: 'kiro-cli' } };
    expect(terminalProgram(terminal).key).toBe('kiro');
    expect(terminalProgram({ ...terminal, status: {} }).key).toBe('vite');
    expect(terminalProgram({ argv: ['nvim'], foreground: { argv: [] } }).key).toBe('nvim');
    expect(terminalProgram({ argv: ['vi'], foreground: { argv: [42] } }).key).toBe('shell');
    expect(terminalProgram({ name: 'claude', title: 'codex', status: { model: 'gemini', provider: 'anthropic' } }).key).toBe('shell');
    expect(terminalProgram().key).toBe('shell');
  });

  it('does not treat arbitrary shell command strings as native launchers', () => {
    expect(resolveProgram(null, ['bash', '-c', 'codex']).key).toBe('shell');
    expect(resolveProgram(null, ['pwsh', '-NoExit', '-Command', 'codex']).key).toBe('shell');
    expect(resolveProgram(null, ['pwsh', '-NoExit', '-Command', "& 'pi' 'it''s ’’fine’’' '' 'a  b'"]).key).toBe('pi');
  });

  it('matches native UTF-8 FNV-1a colors for unlisted programs', () => {
    expect(resolveProgram(null, ['mytool'])).toMatchObject({ name: 'mytool', tile: '#3a7d44', color: '#ffffff' });
    expect(resolveProgram(null, ['工具']).tile).toBe('#d9822b');
    expect(resolveProgram(null, ['/x/mytool'])).toEqual(resolveProgram(null, ['mytool']));
  });

  it('ships a local original icon for every entry and ignores externally supplied paths', () => {
    for (const key of Object.keys(catalog.programs)) {
      expect(programIconUrl(resolveProgram(key)), key).toBeTruthy();
    }
    const program = resolveProgram('claude');
    for (const icon of ['https://example.org/icon.svg', 'icons/../../../secret.svg', '/etc/passwd', 'icons/not-shipped.svg']) {
      expect(programIconUrl({ ...program, icon })).toBeUndefined();
    }
  });
});

describe('ProgramIcon', () => {
  it('uses a tinted mask for monochrome glyphs and preserves full-color artwork', () => {
    const pi = render(ProgramIcon, { props: { program: resolveProgram('pi'), size: 14, color: '#abcdef' } }).body;
    expect(pi).toContain('mask-image:');
    expect(pi).toContain('background-color: #abcdef');
    expect(pi).toContain('width: 14px');
    expect(pi).toContain('aria-hidden="true"');
    expect(pi).not.toContain('<img');
    const claude = render(ProgramIcon, { props: { program: resolveProgram('claude'), decorative: false } }).body;
    expect(claude).toContain('<img');
    expect(claude).not.toContain('mask-image:');
    expect(claude).toContain('aria-label="Claude Code"');
    expect(claude).toContain('role="img"');
  });
});
