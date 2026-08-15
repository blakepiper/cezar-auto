import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

import { listIdeDirectory, readIdeFile, writeIdeFile } from './ide-files.ts';

describe('project IDE filesystem', () => {
  const roots: string[] = [];
  afterEach(() => {
    for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
  });

  it('lists source files and directories without exposing .git metadata', async () => {
    const root = mkdtempSync(join(tmpdir(), 'cez-ide-'));
    roots.push(root);
    mkdirSync(join(root, 'src'), { recursive: true });
    mkdirSync(join(root, '.git'), { recursive: true });
    writeFileSync(join(root, 'README.md'), '# hello\n', 'utf8');

    const result = await listIdeDirectory(root);
    expect(result).toEqual({
      ok: true,
      body: {
        path: '',
        truncated: false,
        entries: [
          { name: 'src', path: 'src', type: 'dir' },
          { name: 'README.md', path: 'README.md', type: 'file', size: 8 },
        ],
      },
    });
  });

  it('reads and writes UTF-8 files within the project', async () => {
    const root = mkdtempSync(join(tmpdir(), 'cez-ide-'));
    roots.push(root);
    writeFileSync(join(root, 'notes.txt'), 'before', 'utf8');

    expect(await readIdeFile(root, 'notes.txt')).toEqual({
      ok: true,
      body: { path: 'notes.txt', content: 'before', size: 6 },
    });
    expect(await writeIdeFile(root, 'notes.txt', 'after ✓')).toEqual({
      ok: true,
      body: { path: 'notes.txt', content: 'after ✓', size: Buffer.byteLength('after ✓') },
    });
  });

  it('rejects traversal, symlinks, binary files, and oversized files', async () => {
    const root = mkdtempSync(join(tmpdir(), 'cez-ide-'));
    const outside = mkdtempSync(join(tmpdir(), 'cez-ide-outside-'));
    roots.push(root, outside);
    writeFileSync(join(outside, 'secret.txt'), 'secret', 'utf8');
    symlinkSync(join(outside, 'secret.txt'), join(root, 'link.txt'));
    writeFileSync(join(root, 'image.bin'), Buffer.from([0, 1, 2, 3]));
    writeFileSync(join(root, 'large.txt'), 'x'.repeat(1_000_001), 'utf8');

    expect(await readIdeFile(root, '../secret.txt')).toMatchObject({ ok: false, status: 400 });
    expect(await readIdeFile(root, 'link.txt')).toMatchObject({ ok: false, status: 400 });
    expect(await readIdeFile(root, 'image.bin')).toMatchObject({ ok: false, status: 409 });
    expect(await readIdeFile(root, 'large.txt')).toMatchObject({ ok: false, status: 409 });
  });
});
