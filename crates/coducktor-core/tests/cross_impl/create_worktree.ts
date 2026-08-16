// Cross-impl fixture (B3): calls the REAL `createWorktree` from `git-worktree.ts` against a
// repo the Rust side of tests/cross_impl.rs built directly with `git` (repos are
// language-neutral, no build-through-TS needed), and prints the resulting `WorktreeInfo` as
// JSON. Invoked as `tsx create_worktree.ts <repoRoot> <runId> <baseBranch>`.
import { createWorktree } from '../../../../packages/cezar/src/git-worktree.ts';

async function main() {
  const [repoRoot, runId, baseBranch] = process.argv.slice(2);
  const info = await createWorktree(repoRoot as string, runId as string, baseBranch as string);
  process.stdout.write(JSON.stringify(info));
}

main();
