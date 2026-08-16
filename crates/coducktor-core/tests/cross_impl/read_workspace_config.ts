// Cross-impl fixture (B1 accept criterion): loads `~/.coducktor/config.json` through the
// REAL Node implementation (a file the Rust side of tests/cross_impl.rs wrote) and prints
// the parsed result as JSON, so Rust can assert Node agrees with its own parse of the
// same file. Invoked as `tsx read_workspace_config.ts <homeDir>`.
process.env.CEZ_HOME = process.argv[2];

import { loadWorkspaceConfig } from '../../../../packages/cezar/src/workspace/config.ts';

async function main() {
  const config = await loadWorkspaceConfig();
  process.stdout.write(JSON.stringify(config));
}

main();
