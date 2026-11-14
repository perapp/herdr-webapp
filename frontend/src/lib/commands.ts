import { record } from './schema';
import { storageKey } from './channel';
import { request } from './api';
import type { TerminalInfo } from './schema';

export type AgentKind = 'pi' | 'claude' | 'codex' | 'opencode';
export const AGENT_KINDS: readonly AgentKind[] = ['pi', 'claude', 'codex', 'opencode'];
export function detectCommandAgent(terminal: TerminalInfo | undefined): AgentKind | undefined {
  if (terminal?.state !== 'live' || terminal.reported === false || !record(terminal.status)) return;
  const app = terminal.status.app;
  // Exact OSC status app names only. claude-code is used by herdr-core's status fixtures.
  if (app === 'claude-code') return 'claude';
  return AGENT_KINDS.find(kind => kind === app);
}

export type CommandSource = 'extension' | 'template' | 'skill';
export interface Command { readonly id: string; readonly text: string; readonly description: string; readonly source?: CommandSource }
export const COMMAND_SOURCE_LABELS: Record<CommandSource, string> = { extension: 'Extension', template: 'Prompt template', skill: 'Skill' };
export const LIVE_COMMAND_LIMIT = 500;
export interface LiveCommands { available: boolean; message: string; commands: readonly Command[] }
const unsafeText = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u;
export function isCommandId(value: unknown): value is string {
  return typeof value === 'string' && !unsafeText.test(value) && /^\/[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/.test(value);
}
/** Fail closed on malformed publisher responses, rather than presenting a partial catalog. */
export function validateLiveCommands(value: unknown): LiveCommands {
  if (!record(value) || typeof value.available !== 'boolean' || typeof value.message !== 'string' ||
      value.message.length > 2048 || unsafeText.test(value.message) || !Array.isArray(value.commands) || value.commands.length > LIVE_COMMAND_LIMIT) {
    throw new Error('The live command response is invalid. Reload the Pi command publisher and retry.');
  }
  const commands: Command[] = [];
  for (const row of value.commands) {
    if (!record(row) || typeof row.name !== 'string' || !isCommandId(`/${row.name}`) ||
        typeof row.description !== 'string' || row.description.length > 200 || unsafeText.test(row.description) ||
        (row.source !== 'extension' && row.source !== 'template' && row.source !== 'skill')) {
      throw new Error('The live command response is invalid. Reload the Pi command publisher and retry.');
    }
    commands.push({ id: `/${row.name}`, text: `/${row.name}`, description: row.description, source: row.source });
  }
  return { available: value.available, message: value.message, commands: value.available ? commands : [] };
}
export async function fetchLiveCommands(address: string, signal: AbortSignal): Promise<LiveCommands> {
  const value = await request(`/api/commands?terminal=${encodeURIComponent(address)}`, undefined, signal);
  signal.throwIfAborted();
  return validateLiveCommands(value);
}
/** Built-ins keep their identity on collisions; duplicate runtime names never add extra rows. */
export function mergeCommands(agent: AgentKind, live: readonly Command[] = []): readonly Command[] {
  const commands = new Map(COMMANDS[agent].map(command => [command.id, command]));
  if (agent === 'pi') for (const command of live) {
    if (isCommandId(command.id) && command.text === command.id && !commands.has(command.id)) commands.set(command.id, command);
  }
  return [...commands.values()];
}
function catalog(rows: readonly (readonly [string, string])[]): readonly Command[] {
  return Object.freeze(rows.map(([text, description]) => Object.freeze({ id: text, text, description })));
}
/** Static fallback from herdr-remote/web/js/palette.js. Pi names also verified against
 * installed pi-coding-agent/dist/core/slash-commands.js (1.5.2-e2db10cafeeb-osc52-v1).
 * This is not a promise that any particular installed agent version supports a command.
 */
export const COMMANDS: Readonly<Record<AgentKind, readonly Command[]>> = Object.freeze({
  pi: catalog([
    ['/compact', 'Compact context'], ['/new', 'Start a new session'], ['/model', 'Switch model'],
    ['/session', 'Show session info'], ['/tree', 'Navigate the session tree'],
    ['/share', 'Share session via artifact or gist'], ['/copy', 'Copy last response'],
    ['/reload', 'Reload extensions, skills and configuration'],
  ]),
  claude: catalog([
    ['/compact', 'Summarize context'], ['/clear', 'Start a fresh conversation'], ['/model', 'Switch model'],
    ['/status', 'Show version and connectivity'], ['/context', 'Show context-window usage'],
    ['/review', 'Review a pull request'], ['/diff', 'Show uncommitted changes'],
    ['/resume', 'Resume a previous conversation'], ['/help', 'Show available commands'],
  ]),
  codex: catalog([
    ['/compact', 'Summarize history'], ['/clear', 'Start a new chat'], ['/diff', 'Show working-tree diff'],
    ['/model', 'Switch model'], ['/status', 'Show model and token usage'], ['/review', 'Review working tree'],
    ['/mention', 'Attach files to context'], ['/plan', 'Enter plan mode'],
  ]),
  opencode: catalog([
    ['/compact', 'Compact session'], ['/new', 'Start a new session'], ['/models', 'List and switch models'],
    ['/undo', 'Undo last turn and revert'], ['/share', 'Share session'],
    ['/diff', 'Show working changes'], ['/export', 'Export to Markdown'],
  ]),
});

/** Slash name (up to 128 ASCII characters), optional space + at most 512 argument characters.
 * No C0/C1 controls, line separators, or invisible bidi instructions may reach a terminal.
 */
export function isCommandText(value: unknown): value is string {
  if (typeof value !== 'string' || /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u.test(value)) return false;
  return /^\/[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}(?: [^\r\n]{0,512})?$/u.test(value);
}

export type Favorites = Record<AgentKind, string[]>;
export const FAVORITES_KEY = 'herdr.command-favorites.v1';
export const FAVORITES_LIMIT = 100;
const MAX_STORAGE_LENGTH = 32768;
function emptyFavorites(): Favorites { return { pi: [], claude: [], codex: [], opencode: [] }; }
export function validateFavorites(value: unknown): Favorites {
  const result = emptyFavorites();
  if (!record(value)) return result;
  for (const agent of AGENT_KINDS) {
    const ids = value[agent];
    if (!Array.isArray(ids)) continue;
    // Retain safe custom IDs even when their publisher/agent is temporarily unavailable.
    // Favorites are preferences only: callers must filter the current catalog, never these IDs.
    result[agent] = [...new Set(ids.slice(0, FAVORITES_LIMIT).filter(isCommandId))];
  }
  return result;
}
export interface FavoritesResult { favorites: Favorites; saved: boolean }
export function loadFavorites(): FavoritesResult {
  try {
    const raw = localStorage.getItem(storageKey(FAVORITES_KEY));
    if (raw !== null && raw.length > MAX_STORAGE_LENGTH) return { favorites: emptyFavorites(), saved: false };
    return { favorites: validateFavorites(raw === null ? {} : JSON.parse(raw)), saved: true };
  } catch { return { favorites: emptyFavorites(), saved: false }; }
}
/** Read-modify-write preserves other agents' choices; failed writes leave preferences unchanged. */
export function saveFavorites(agent: AgentKind, ids: readonly string[]): FavoritesResult {
  const current = loadFavorites();
  if (!current.saved) return current;
  const favorites = validateFavorites({ ...current.favorites, [agent]: ids });
  try {
    localStorage.setItem(storageKey(FAVORITES_KEY), JSON.stringify(favorites));
    return { favorites, saved: true };
  } catch { return { favorites: current.favorites, saved: false }; }
}
