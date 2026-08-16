// Cross-impl fixture (B2 accept criterion): writes a project's `.ai/coducktor/runs.json`
// and per-run NDJSON event log through the REAL Node `RunStore` so the Rust side of
// tests/cross_impl.rs can read them back and assert field-for-field agreement — including
// the legacy `claude-cli` runner id (#547), which `RunStore.createRun`'s typed input can't
// express, so it's patched onto the index by hand after a real save. Invoked as
// `tsx write_runs_and_events.ts <dataDir>`.
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { RunStore } from '../../../../packages/cezar/src/runs/store.ts';

async function main() {
  const dataDir = process.argv[2];
  const store = RunStore.open(dataDir);

  const run = store.createRun({
    title: 'Cross-impl task',
    workflow: 'quick-task',
    task: 'do the cross-impl thing',
    runner: 'claude',
    requestedRunner: 'auto',
    steps: [{ id: 'task', name: 'Do the task', kind: 'agent' }],
  });
  store.updateStep(run.id, 'task', { status: 'running', iterations: 1, backend: 'claude' });
  store.appendEvent(run.id, { type: 'message', stepId: 'task', text: 'hello from node' });
  store.appendEvent(run.id, { type: 'message', stepId: 'task', text: 'a second line' });
  // Left `running` on purpose: flushing without closing the store leaves a live-looking
  // status on disk, so the Rust reader's `reconcile_loaded_run` has something real to do.
  store.updateRun(run.id, { status: 'running' });
  store.flush();

  // Patch in the legacy runner id RunStore's own typed API can no longer write (#547) —
  // simulating a `runs.json` a pre-#547 cezar left behind.
  const indexPath = join(dataDir, 'runs.json');
  const raw = JSON.parse(readFileSync(indexPath, 'utf8'));
  raw[0].runner = 'claude-cli';
  raw[0].steps[0].backend = 'claude-cli';
  writeFileSync(indexPath, JSON.stringify(raw, null, 2), 'utf8');

  console.log(run.id);
}

main();
