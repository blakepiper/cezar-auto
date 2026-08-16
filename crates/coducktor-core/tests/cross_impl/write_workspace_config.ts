// Cross-impl fixture (B1 accept criterion): writes `~/.coducktor/config.json` through the
// REAL Node implementation so the Rust side of tests/cross_impl.rs can read it back and
// assert field-for-field agreement. Invoked as `tsx write_workspace_config.ts <homeDir>`.
process.env.CEZ_HOME = process.argv[2];

import { mergeWriteWorkspaceConfig } from '../../../../packages/cezar/src/workspace/config.ts';

async function main() {
  await mergeWriteWorkspaceConfig((config) => {
    config.resources.maxParallel = 9;
    config.resources.monitoringWakeIntervalMinutes = null; // explicit "park until resumed"
    config.resources.memoryLimitMb = 4096;
    config.composerDefaults.autonomous = true;
    config.disabledProviders = ['pi', 'claude']; // must land deduped + canonical order
    config.agentDefaults.runner = 'codex';
    config.agentDefaults.models = { codex: 'gpt-cross-impl' };
    config.quotaRouting.enabled = true;
    config.quotaRouting.providers.codex.longWindowStopAtPercent = 77;
    config.projects.push({
      id: 'cross-impl',
      root: '/repo/cross-impl',
      name: 'Cross Impl',
      addedAt: '2026-08-16T00:00:00.000Z',
      lastOpenedAt: '2026-08-16T00:00:00.000Z',
      source: 'local',
      tags: ['storefront'],
    });
    // An unknown key a "newer" writer might add — must round-trip through Rust untouched.
    (config as Record<string, unknown>).fromTheFutureNode = 'kept-by-rust';
  });
}

main();
