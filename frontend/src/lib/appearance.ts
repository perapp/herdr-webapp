import { storageKey } from './channel';
export type AppearancePreference = 'system' | 'light' | 'dark';
export interface Appearance { preference: AppearancePreference; dark: boolean }
export const appearanceKey = 'herdr.appearance';
export function parseAppearance(value: unknown): AppearancePreference {
  return value === 'light' || value === 'dark' ? value : 'system';
}
export function resolveAppearance(preference: AppearancePreference, systemDark: boolean): boolean {
  return preference === 'system' ? systemDark : preference === 'dark';
}
let preference: AppearancePreference = 'system';
let media: MediaQueryList | undefined;
const listeners = new Set<(appearance: Appearance) => void>();
function readPreference(): AppearancePreference {
  try { return parseAppearance(localStorage.getItem(storageKey(appearanceKey))); }
  catch { return 'system'; }
}
/** Called before mounting to avoid a flash of the wrong saved colour scheme. */
export function initializeAppearance(): Appearance {
  if (!media) {
    media = matchMedia('(prefers-color-scheme: dark)');
    preference = readPreference();
  }
  const appearance = { preference, dark: resolveAppearance(preference, media.matches) };
  document.documentElement.dataset.theme = appearance.dark ? 'dark' : 'light';
  return appearance;
}
function publish(): void {
  const appearance = initializeAppearance();
  for (const listener of listeners) listener(appearance);
}
function storageChanged(event: StorageEvent): void {
  if (event.key !== storageKey(appearanceKey) && event.key !== null) return;
  preference = readPreference(); publish();
}
export function watchAppearance(listener: (appearance: Appearance) => void): () => void {
  const appearance = initializeAppearance();
  if (!listeners.size) {
    media!.addEventListener('change', publish);
    window.addEventListener('storage', storageChanged);
  }
  listeners.add(listener); listener(appearance);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) {
      media!.removeEventListener('change', publish);
      window.removeEventListener('storage', storageChanged);
    }
  };
}
/** Keep the current choice usable even when private-mode storage is unavailable. */
export function setAppearance(value: AppearancePreference): boolean {
  initializeAppearance(); preference = value;
  let saved = true;
  try { localStorage.setItem(storageKey(appearanceKey), value); } catch { saved = false; }
  publish();
  return saved;
}
