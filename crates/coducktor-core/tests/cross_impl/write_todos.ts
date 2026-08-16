// Cross-impl fixture (B4): writes `<repoRoot>/.ai/coducktor/todos.json` through the REAL
// Node implementation so the Rust side of tests/cross_impl.rs can read it back and assert
// field-for-field agreement. Invoked as `tsx write_todos.ts <repoRoot>`.
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { todosPath } from '../../../../packages/cezar/src/todos.ts';

async function main() {
  const repoRoot = process.argv[2] as string;
  const dataDir = join(repoRoot, '.ai/coducktor');
  await mkdir(dataDir, { recursive: true });
  // One entry with an id (as the server would have assigned already), one written the way
  // an agent process actually writes it — no `id` at all — and one malformed entry (empty
  // summary) that must be skipped without evicting its siblings.
  const raw = [
    { id: 'node-1', summary: 'a real follow-up', runnable: true, suggestedSkill: 'om-a' },
    { summary: 'from an agent, no id yet', prUrl: 'https://github.com/o/r/pull/1' },
    { id: 'bad', summary: '' },
  ];
  await writeFile(todosPath(dataDir), JSON.stringify(raw, null, 2), 'utf8');
}

main();
