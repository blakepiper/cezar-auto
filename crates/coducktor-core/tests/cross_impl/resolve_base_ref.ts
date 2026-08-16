// Cross-impl fixture (B3): calls the REAL `resolveBaseRef` from `git-worktree.ts` against a
// repo whose state the Rust side of tests/cross_impl.rs already built (with `git` directly —
// repos are language-neutral, no need to build them from TS), and prints the result as JSON
// (`"main"`, `"origin/main"`, or `null`). Invoked as
// `tsx resolve_base_ref.ts <repoRoot> <base>`.
import { resolveBaseRef } from '../../../../packages/cezar/src/git-worktree.ts';

async function main() {
  const [repoRoot, base] = process.argv.slice(2);
  const resolved = await resolveBaseRef(repoRoot as string, base as string);
  process.stdout.write(JSON.stringify(resolved));
}

main();
