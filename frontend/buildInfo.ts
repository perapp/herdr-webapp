import { execFileSync } from 'node:child_process';
import { commitDate, timestampVersion } from './src/lib/versions.ts';
import { validVersion } from './src/lib/updates.ts';
export function sourceBuild(env: NodeJS.ProcessEnv, git: (args: string[]) => string = args => execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()) {
  let head = '';
  try { head = git(['rev-parse', 'HEAD']); } catch { /* Archive. */ }
  const revision = /^[a-fA-F0-9]{40}$/.test(env.HERDR_BUILD_REVISION || '') ? env.HERDR_BUILD_REVISION! : /^[a-fA-F0-9]{40}$/.test(head) ? head : '';
  let date: string | null = null;
  let dirty: boolean | null = null;
  let releaseVersion: string | null = null;
  if (revision && head.toLowerCase() === revision.toLowerCase()) {
    try { date = commitDate(git(['show', '-s', '--format=%cI', revision])); } catch { /* Unknown. */ }
    try { dirty = git(['status', '--porcelain', '--untracked-files=all']).length > 0; } catch { /* Unknown. */ }
    try { releaseVersion = dirty === false ? git(['tag', '--points-at', 'HEAD', '--sort=version:refname']).split('\n').map(tag => tag.replace(/^v/, '')).filter(validVersion).at(-1) || null : null; } catch { /* Unknown. */ }
  } else if (revision && !head) {
    date = commitDate(env.HERDR_BUILD_COMMIT_DATE);
    const tag = env.HERDR_BUILD_TAG?.replace(/^v/, '');
    releaseVersion = validVersion(tag) ? tag : null;
  }
  return { revision, commitDate: date, dirty, releaseVersion, version: releaseVersion || timestampVersion(revision, date) || 'Unavailable' };
}
