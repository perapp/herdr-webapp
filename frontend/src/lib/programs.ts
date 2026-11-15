import catalog from './programs.catalog.json';
import { record } from './schema';
import type { TerminalInfo } from './schema';

/** Shipped herdr-app catalog only. Custom programs.json and home-folder icons are not supported. */
interface Entry {
  name?: string; icon?: string; tint?: boolean; tile?: string; color?: string; match?: string[];
}
export interface ProgramAppearance {
  key: string; name: string; icon: string | null; tint: boolean;
  tile: string | null; color: string | null; shell: boolean; agent: boolean;
}
const entries = Object.entries(catalog.programs as Record<string, Entry>).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
const assets = import.meta.glob('../assets/programs/*.svg', { eager: true, query: '?url', import: 'default' }) as Record<string, string>;

/** Only bundled asset URLs, never paths/URLs supplied by a terminal or remote machine. */
export function programIconUrl(program: ProgramAppearance): string | undefined {
  if (!program.icon?.startsWith('icons/')) return;
  return assets[`../assets/programs/${program.icon.slice(6)}`];
}
function find(name: string): [string, Entry] | undefined {
  const lower = name.toLowerCase();
  return entries.find(([key]) => key.toLowerCase() === lower)
    ?? entries.find(([, entry]) => entry.match?.some(alias => alias.toLowerCase() === lower));
}
function shown(key: string, entry: Entry, name: string | undefined, shell: boolean, agent: boolean): ProgramAppearance {
  return { key, name: name ?? entry.name ?? key, icon: entry.icon ?? null, tint: entry.tint ?? true,
    tile: entry.tile ?? null, color: entry.color ?? null, shell, agent };
}
function fallback(key: 'shell' | 'program' | 'agent'): Entry { return catalog.programs[key]; }
function base(value: string): string {
  return (value.split(/[\\/]/).pop() ?? value).replace(/^-+/, '').replace(/\.exe$/i, '');
}
const isShell = (name: string) => catalog.shells.includes(name);
const isInterpreter = (name: string) => catalog.interpreters.includes(name);
const psQuotes = "'\u2018\u2019\u201a\u201b";
/** Native PowerShell launcher format, not a general shell command parser. */
function powershellWords(line: string): string[] | undefined {
  if (!line.startsWith('& ')) return;
  const words: string[] = [];
  let i = 2;
  while (i < line.length) {
    const c = line[i++];
    if (c === ' ') continue;
    if (!psQuotes.includes(c)) return;
    let word = '';
    while (i < line.length) {
      const ch = line[i++];
      if (psQuotes.includes(ch)) {
        if (i < line.length && psQuotes.includes(line[i])) { word += line[i++]; continue; }
        break;
      }
      word += ch;
    }
    words.push(word);
  }
  return words;
}
function hashColor(name: string): string {
  const palette = ['#6d5bd0', '#2f80ed', '#0f9d8a', '#d9822b', '#c2417d', '#3a7d44', '#8a5a44', '#4e5d94'];
  let hash = 2166136261;
  for (const byte of new TextEncoder().encode(name)) hash = Math.imul(hash ^ byte, 16777619) >>> 0;
  return palette[hash % palette.length];
}

/** Port of native programs::of: reported app wins; argv is never inferred from titles or model/provider names. */
export function resolveProgram(app?: string | null, argv: readonly string[] = []): ProgramAppearance {
  if (app) {
    const found = find(app);
    return found ? shown(...found, undefined, false, true) : shown('agent', fallback('agent'), app, false, true);
  }
  // Unwrap only the native Unix/fish and PowerShell launchers. Each pass consumes argv.
  while (argv.length) {
    if (argv.length > 4 && isShell(base(argv[0])) && argv[1].startsWith('-') && argv[1].endsWith('c')
      && (argv[2].includes('"$@"') || argv[2] === 'command $argv[2..]; exec $argv[1] -i')) {
      argv = argv.slice(4); continue;
    }
    if (argv.length === 4 && isShell(base(argv[0])) && argv[1] === '-NoExit' && argv[2] === '-Command') {
      const words = powershellWords(argv[3]);
      if (words) { argv = words; continue; }
    }
    break;
  }
  if (!argv.length) return shown('shell', fallback('shell'), undefined, true, false);
  const first = base(argv[0]);
  if (isInterpreter(first)) {
    for (const arg of argv.slice(1).filter(arg => !arg.startsWith('-'))) {
      const stem = base(arg).replace(/\.(js|mjs|cjs|ts|py|rb)$/, '');
      const found = find(stem);
      if (found && !isInterpreter(found[0])) return shown(...found, undefined, false, false);
    }
  }
  const found = find(first);
  if (found) return shown(...found, undefined, false, false);
  if (isShell(first)) return shown('shell', fallback('shell'), first, true, false);
  return shown('program', { ...fallback('program'), tile: hashColor(first), color: '#ffffff' }, first, false, false);
}

/** Core record adapter: status.app and foreground.argv, falling back to initial argv exactly as native. */
export function terminalProgram(terminal?: TerminalInfo): ProgramAppearance {
  const status = record(terminal?.status) ? terminal.status : undefined;
  const foreground = record(terminal?.foreground) ? terminal.foreground : undefined;
  const argv = Array.isArray(foreground?.argv) && foreground.argv.length ? foreground.argv : terminal?.argv;
  return resolveProgram(typeof status?.app === 'string' ? status.app : undefined,
    Array.isArray(argv) ? argv.filter((word): word is string => typeof word === 'string') : []);
}
