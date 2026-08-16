// Cross-impl fixture (B2 accept criterion): reads a `runs.json` + per-run NDJSON event log
// that the Rust side of tests/cross_impl.rs wrote, through the REAL Node readers
// (`readRunIndexFromDisk` and `RunStore.open(...).readEvents`), and prints the result as
// JSON so Rust can assert Node parses it identically — including applying
// `reconcileLoadedRun` to a run Rust left in a live-looking status. Invoked as
// `tsx read_runs_index.ts <dataDir>`. The run id is read back off the index itself (the
// fixture writes exactly one run) rather than passed on argv, so this stays a plain
// single-positional fixture like every other one in this directory.
import { readRunIndexFromDisk } from '../../../../packages/cezar/src/runs/run-index.ts';
import { RunStore } from '../../../../packages/cezar/src/runs/store.ts';

async function main() {
  const dataDir = process.argv[2];

  const runs = readRunIndexFromDisk(dataDir);
  const store = RunStore.open(dataDir);
  const events = runs[0] ? store.readEvents(runs[0].id) : [];

  console.log(JSON.stringify({ runs, events }));
}

main();
