// Cross-impl fixture (B4): reads `<repoRoot>/.ai/coducktor/todos.json` through the REAL
// Node implementation (a file the Rust side of tests/cross_impl.rs wrote) and prints the
// parsed result as JSON, so Rust can assert Node agrees with its own parse of the same
// file. Invoked as `tsx read_todos.ts <repoRoot>`.
import { join } from 'node:path';
import { readTodos } from '../../../../packages/cezar/src/todos.ts';

async function main() {
  const repoRoot = process.argv[2] as string;
  const dataDir = join(repoRoot, '.ai/coducktor');
  const items = await readTodos(dataDir);
  process.stdout.write(JSON.stringify(items));
}

main();
