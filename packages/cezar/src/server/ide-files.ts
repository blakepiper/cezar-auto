import { lstat, readdir, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';

export const IDE_FILE_MAX_BYTES = 1_000_000;
const IDE_DIRECTORY_MAX_ENTRIES = 2_000;

type IdeErrorStatus = 400 | 404 | 409;

export type IdeResult<T> =
  | { ok: true; body: T }
  | { ok: false; status: IdeErrorStatus; error: string };

export interface IdeDirectoryBody {
  path: string;
  entries: Array<{ name: string; path: string; type: 'dir' | 'file'; size?: number }>;
  truncated: boolean;
}

export interface IdeFileBody {
  path: string;
  content: string;
  size: number;
}

function contains(root: string, candidate: string): boolean {
  return candidate === root || candidate.startsWith(root.endsWith(sep) ? root : `${root}${sep}`);
}

function displayPath(root: string, target: string): string {
  return relative(root, target).split(sep).join('/');
}

async function realpathOrNull(path: string): Promise<string | null> {
  try {
    return await realpath(path);
  } catch {
    return null;
  }
}

/** Resolve a browser-supplied relative path without ever exposing a path outside the project. */
async function resolveProjectPath(
  root: string,
  path: string,
  kind: 'directory' | 'file',
): Promise<IdeResult<{ root: string; target: string }>> {
  if (path.includes('\0') || isAbsolute(path) || path.includes('\\')) {
    return { ok: false, status: 400, error: 'invalid project path' };
  }

  const projectRoot = await realpathOrNull(root);
  if (projectRoot === null) return { ok: false, status: 404, error: 'project folder not found' };

  const lexical = resolve(projectRoot, path);
  if (!contains(projectRoot, lexical)) {
    return { ok: false, status: 400, error: 'path is outside the project' };
  }

  const target = await realpathOrNull(lexical);
  if (target === null) return { ok: false, status: 404, error: kind === 'directory' ? 'directory not found' : 'file not found' };
  if (!contains(projectRoot, target)) return { ok: false, status: 400, error: 'path is outside the project' };

  // Do not follow links in the IDE. A path that resolves somewhere else is not a stable project
  // file and rejecting it also keeps a link replacement from turning an edit into an escape.
  if (target !== lexical) return { ok: false, status: 400, error: 'symbolic links are not editable' };

  const info = await lstat(target).catch(() => null);
  const valid = kind === 'directory' ? info?.isDirectory() : info?.isFile();
  if (!valid) return { ok: false, status: 404, error: kind === 'directory' ? 'directory not found' : 'file not found' };

  return { ok: true, body: { root: projectRoot, target } };
}

export async function listIdeDirectory(root: string, path = ''): Promise<IdeResult<IdeDirectoryBody>> {
  const resolved = await resolveProjectPath(root, path, 'directory');
  if (!resolved.ok) return resolved;

  const entries = await readdir(resolved.body.target, { withFileTypes: true }).catch(() => null);
  if (entries === null) return { ok: false, status: 404, error: 'directory is not readable' };

  const candidates = entries
    // `.git` is repository metadata rather than an editable source file, and its object store can
    // contain hundreds of thousands of entries. The working tree remains fully visible.
    .filter((entry) => entry.name !== '.git')
    .filter((entry) => entry.isDirectory() || entry.isFile())
    .sort((a, b) => {
      const typeOrder = Number(b.isDirectory()) - Number(a.isDirectory());
      return typeOrder || a.name.localeCompare(b.name);
    });
  const truncated = candidates.length > IDE_DIRECTORY_MAX_ENTRIES;
  const output: IdeDirectoryBody['entries'] = [];

  for (const entry of candidates.slice(0, IDE_DIRECTORY_MAX_ENTRIES)) {
    const target = join(resolved.body.target, entry.name);
    const entryPath = displayPath(resolved.body.root, target);
    if (entry.isDirectory()) {
      output.push({ name: entry.name, path: entryPath, type: 'dir' });
      continue;
    }
    const info = await stat(target).catch(() => null);
    if (info?.isFile()) output.push({ name: entry.name, path: entryPath, type: 'file', size: info.size });
  }

  return {
    ok: true,
    body: {
      path: path === '' ? '' : displayPath(resolved.body.root, resolved.body.target),
      entries: output,
      truncated,
    },
  };
}

export async function readIdeFile(root: string, path: string): Promise<IdeResult<IdeFileBody>> {
  const resolved = await resolveProjectPath(root, path, 'file');
  if (!resolved.ok) return resolved;

  const info = await stat(resolved.body.target).catch(() => null);
  if (info === null) return { ok: false, status: 404, error: 'file not found' };
  if (info.size > IDE_FILE_MAX_BYTES) return { ok: false, status: 409, error: 'file is too large to edit' };

  const bytes = await readFile(resolved.body.target).catch(() => null);
  if (bytes === null) return { ok: false, status: 404, error: 'file is not readable' };
  try {
    const content = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    if (content.includes('\0')) return { ok: false, status: 409, error: 'binary files cannot be edited' };
    return { ok: true, body: { path: displayPath(resolved.body.root, resolved.body.target), content, size: bytes.byteLength } };
  } catch {
    return { ok: false, status: 409, error: 'binary files cannot be edited' };
  }
}

export async function writeIdeFile(root: string, path: string, content: string): Promise<IdeResult<IdeFileBody>> {
  const bytes = new TextEncoder().encode(content);
  if (bytes.byteLength > IDE_FILE_MAX_BYTES) {
    return { ok: false, status: 409, error: 'file is too large to edit' };
  }
  const resolved = await resolveProjectPath(root, path, 'file');
  if (!resolved.ok) return resolved;
  await writeFile(resolved.body.target, content, 'utf8');
  return readIdeFile(root, path);
}
