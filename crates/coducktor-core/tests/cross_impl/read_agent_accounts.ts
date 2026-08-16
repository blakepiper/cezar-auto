// Cross-impl fixture: loads `~/.coducktor/agent-accounts.json` through the REAL Node
// implementation (a file the Rust side of tests/cross_impl.rs wrote) and prints the
// parsed result as JSON. Invoked as `tsx read_agent_accounts.ts <homeDir>`.
process.env.CEZ_HOME = process.argv[2];

import { loadAgentAccounts } from '../../../../packages/cezar/src/workspace/agent-accounts.ts';

async function main() {
  const store = await loadAgentAccounts();
  process.stdout.write(JSON.stringify(store));
}

main();
