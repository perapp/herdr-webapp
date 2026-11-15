import { execFileSync } from 'node:child_process';
import { commitDate } from './src/lib/versions.ts';
export function sourceBuild(env: NodeJS.ProcessEnv, git: (args: string[]) => string = args => execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()) {
  const revision = /^[a-f0-9]{40}$/.test(env.HERDR_BUILD_REVISION || '') ? env.HERDR_BUILD_REVISION! : '';
  let date: string | null = null;
  let dirty: boolean | null = null;
  if (!revision) return { revision, commitDate: date, dirty };
  let head: string;
  try { head = git(['rev-parse', 'HEAD']); }
  catch {
    // Explicit archive attestation only when no checkout can establish HEAD.
    return { revision, commitDate: commitDate(env.HERDR_BUILD_COMMIT_DATE), dirty };
  }
  if (head === revision) {
    try { date = commitDate(git(['show', '-s', '--format=%cI', revision])); } catch { /* Unknown, never a build timestamp. */ }
    try { dirty = git(['status', '--porcelain', '--untracked-files=all']).length > 0; } catch { /* Unknown working tree. */ }
  }
  return { revision, commitDate: date, dirty };
}
