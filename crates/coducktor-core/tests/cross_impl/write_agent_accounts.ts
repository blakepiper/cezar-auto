// Cross-impl fixture: writes `~/.coducktor/agent-accounts.json` through the REAL Node
// implementation. Invoked as `tsx write_agent_accounts.ts <homeDir>`.
process.env.CEZ_HOME = process.argv[2];

import { mergeWriteAgentAccounts } from '../../../../packages/cezar/src/workspace/agent-accounts.ts';

async function main() {
  await mergeWriteAgentAccounts((store) => {
    store.accounts.push({
      id: 'work',
      provider: 'claude',
      configDir: '~/.claude-work',
      label: 'Work account',
      addedAt: '2026-08-16T00:00:00.000Z',
    });
    store.defaults.claude = 'work';
    store.selections['/repo/cross-impl'] = { claude: 'work' };
    (store as Record<string, unknown>).fromTheFutureNode = 'kept-by-rust';
  });
}

main();
