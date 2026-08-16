import { mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = resolve(packageDir, '../..');

function freePort() {
  return new Promise((resolvePort, reject) => {
    const probe = createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const address = probe.address();
      if (!address || typeof address === 'string') {
        probe.close();
        reject(new Error('could not select a test port'));
        return;
      }
      probe.close((error) => (error ? reject(error) : resolvePort(address.port)));
    });
  });
}

async function waitForHealth(baseUrl, child) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`Rust server exited with ${child.exitCode}`);
    try {
      const response = await fetch(`${baseUrl}/api/v1/health`, {
        headers: { host: new URL(baseUrl).host },
      });
      if (response.ok) return;
    } catch {
      // The listener may still be starting.
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  throw new Error('Rust server did not become healthy within 15 seconds');
}

const port = await freePort();
const baseUrl = `http://127.0.0.1:${port}`;
const tempRepo = mkdtempSync(join(tmpdir(), 'coducktor-rust-repo-'));
const tempHome = mkdtempSync(join(tmpdir(), 'coducktor-rust-home-'));
const child = spawn(
  'cargo',
  [
    'run',
    '--quiet',
    '--package',
    'coducktor-server',
    '--',
    '--bind',
    `127.0.0.1:${port}`,
    '--repo-root',
    tempRepo,
  ],
  {
    cwd: repoRoot,
    env: { ...process.env, DUCK_HOME: tempHome, CEZ_HOME: tempHome },
    stdio: ['ignore', 'pipe', 'pipe'],
  },
);
let stderr = '';
child.stderr.setEncoding('utf8');
child.stderr.on('data', (chunk) => {
  stderr += chunk;
});

try {
  await waitForHealth(baseUrl, child);
  const result = await new Promise((resolveResult, reject) => {
    const test = spawn(
      'npm',
      ['test', '--', 'packages/cezar/src/server/rust-server.smoke.test.ts'],
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          DUCK_HTTP_BASE_URL: baseUrl,
          DUCK_HOME: tempHome,
          CEZ_HOME: tempHome,
        },
        stdio: 'inherit',
      },
    );
    test.once('error', reject);
    test.once('exit', (code, signal) => resolveResult({ code, signal }));
  });
  if (result.code !== 0) {
    process.exitCode = result.code ?? 1;
  }
} finally {
  child.kill('SIGTERM');
  await Promise.race([once(child, 'exit'), new Promise((resolveExit) => setTimeout(resolveExit, 2_000))]);
  if (process.exitCode && stderr.trim()) process.stderr.write(stderr);
  rmSync(tempHome, { force: true, recursive: true });
  rmSync(tempRepo, { force: true, recursive: true });
}
