// Cross-impl fixture (B4): loads `<repoRoot>/.ai/coducktor/workflows/*.{yaml,yml}` through
// the REAL Node implementation (files the Rust side of tests/cross_impl.rs wrote directly —
// workflow YAML is plain repo content, not something either implementation writes at
// runtime) and prints the result as JSON, so Rust can assert its own `serde_yaml_ng`-backed
// loader agrees with Node's `yaml`-backed one. Invoked as `tsx load_workflows.ts <repoRoot>`.
import { loadWorkflows } from '../../../../packages/cezar/src/workflows/load.ts';

async function main() {
  const repoRoot = process.argv[2] as string;
  const result = await loadWorkflows(repoRoot);
  process.stdout.write(JSON.stringify(result));
}

main();
