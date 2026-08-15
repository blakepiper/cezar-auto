import {
  ArrowUpIcon,
  CheckIcon,
  ChevronRightIcon,
  Code2Icon,
  FileIcon,
  FolderIcon,
  LoaderCircleIcon,
  SaveIcon,
  TriangleAlertIcon,
} from 'lucide-react'
import * as React from 'react'
import { useSearchParams } from 'react-router'

import { useIdeDirectory, useIdeFile, useSaveIdeFile } from '@/api/queries'
import { CenteredState } from '@/components/centered-state'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import type { IdeEntry } from '@open-mercato/cezar-api-client'

function parentPath(path: string): string {
  const slash = path.lastIndexOf('/')
  return slash < 0 ? '' : path.slice(0, slash)
}

function formatBytes(size: number): string {
  if (size < 1_024) return `${size} B`
  if (size < 1_024 * 1_024) return `${Math.round(size / 1_024)} KB`
  return `${(size / (1_024 * 1_024)).toFixed(1)} MB`
}

function entryIcon(entry: IdeEntry) {
  return entry.type === 'dir'
    ? <FolderIcon className="size-4 text-violet" aria-hidden="true" />
    : <FileIcon className="size-4 text-muted-foreground" aria-hidden="true" />
}

export function IdeRoute() {
  const [searchParams, setSearchParams] = useSearchParams()
  const selectedPath = searchParams.get('file') || null
  const [directoryPath, setDirectoryPath] = React.useState(() => (selectedPath ? parentPath(selectedPath) : ''))
  const [draft, setDraft] = React.useState('')
  const [draftPath, setDraftPath] = React.useState<string | null>(null)
  const [dirty, setDirty] = React.useState(false)
  const directory = useIdeDirectory(directoryPath || null)
  const file = useIdeFile(selectedPath)
  const save = useSaveIdeFile()

  React.useEffect(() => {
    setDirectoryPath(selectedPath ? parentPath(selectedPath) : '')
  }, [selectedPath])

  React.useEffect(() => {
    if (!file.data || selectedPath === null || file.data.path !== selectedPath) return
    if (dirty && draftPath === selectedPath) return
    setDraft(file.data.content)
    setDraftPath(file.data.path)
    setDirty(false)
  }, [dirty, draftPath, file.data, selectedPath])

  const navigateToDirectory = React.useCallback((path: string) => {
    setDirectoryPath(path)
    setSearchParams((current) => {
      const next = new URLSearchParams(current)
      next.delete('file')
      return next
    }, { replace: true })
  }, [setSearchParams])

  const openFile = React.useCallback((path: string) => {
    setDraft('')
    setDraftPath(null)
    setDirty(false)
    setDirectoryPath(parentPath(path))
    setSearchParams((current) => {
      const next = new URLSearchParams(current)
      next.set('file', path)
      return next
    }, { replace: true })
  }, [setSearchParams])

  const saveFile = React.useCallback(() => {
    if (!dirty || draftPath === null || save.isPending) return
    save.mutate(
      { path: draftPath, content: draft },
      { onSuccess: () => setDirty(false) },
    )
  }, [dirty, draft, draftPath, save])

  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
        event.preventDefault()
        saveFile()
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [saveFile])

  return (
    <div data-route="ide" className="flex min-h-full flex-col">
      <header className="sticky top-0 z-20 border-b border-border bg-background/95 px-4 py-3 backdrop-blur md:px-6">
        <div className="flex min-w-0 items-center gap-2.5">
          <Code2Icon className="size-4 text-violet" aria-hidden="true" />
          <h1 className="text-lg font-semibold">IDE</h1>
          <span className="truncate text-[12px] text-muted-foreground">
            {selectedPath ?? 'Browse and edit project files'}
          </span>
          {dirty ? <span className="shrink-0 text-[11px] text-pending">Unsaved changes</span> : null}
          <Button
            type="button"
            size="sm"
            className="ml-auto"
            disabled={!dirty || save.isPending}
            onClick={saveFile}
          >
            {save.isPending ? <LoaderCircleIcon className="motion-safe:animate-spin" /> : <SaveIcon />}
            Save
          </Button>
        </div>
      </header>

      <div className="flex min-h-[calc(100dvh-4.25rem)] flex-1 flex-col md:flex-row">
        <aside className="w-full shrink-0 border-b border-border bg-muted/20 md:w-72 md:border-b-0 md:border-r">
          <div className="flex items-center gap-2 border-b border-border px-3 py-2.5">
            <FolderIcon className="size-4 text-violet" aria-hidden="true" />
            <span className="truncate text-xs font-semibold">{directoryPath || 'Project root'}</span>
            {directoryPath ? (
              <button
                type="button"
                className="ml-auto rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                title="Go up"
                aria-label="Go up one folder"
                onClick={() => navigateToDirectory(parentPath(directoryPath))}
              >
                <ArrowUpIcon className="size-3.5" aria-hidden="true" />
              </button>
            ) : null}
          </div>
          <ExplorerList
            data={directory.data}
            isPending={directory.isPending}
            isError={directory.isError}
            error={directory.error}
            onDirectory={navigateToDirectory}
            onFile={openFile}
          />
        </aside>

        <main className="flex min-h-0 min-w-0 flex-1 flex-col bg-card">
          {selectedPath === null ? (
            <CenteredState
              icon={<Code2Icon />}
              tone="neutral"
              heading="h2"
              title="Choose a file"
              subtitle="Select a file from the project explorer to start editing."
            />
          ) : file.isPending ? (
            <div className="flex flex-1 items-center justify-center gap-2 text-xs text-soft-foreground">
              <LoaderCircleIcon className="size-4 motion-safe:animate-spin" aria-hidden="true" />
              Loading {selectedPath}…
            </div>
          ) : file.isError ? (
            <CenteredState
              icon={<TriangleAlertIcon />}
              tone="danger"
              heading="h2"
              title="This file cannot be edited"
              subtitle={file.error.message}
            />
          ) : (
            <Editor
              path={selectedPath}
              content={draft}
              size={file.data?.size ?? 0}
              dirty={dirty}
              saveError={save.isError ? save.error.message : null}
              onChange={(value) => {
                setDraft(value)
                setDirty(true)
              }}
            />
          )}
        </main>
      </div>
    </div>
  )
}

function ExplorerList({
  data,
  isPending,
  isError,
  error,
  onDirectory,
  onFile,
}: {
  data: ReturnType<typeof useIdeDirectory>['data']
  isPending: boolean
  isError: boolean
  error: Error | null
  onDirectory: (path: string) => void
  onFile: (path: string) => void
}) {
  if (isPending) {
    return <p className="flex items-center gap-2 px-3 py-5 text-xs text-soft-foreground"><LoaderCircleIcon className="size-3.5 motion-safe:animate-spin" />Loading folder…</p>
  }
  if (isError) {
    return <p className="px-3 py-5 text-xs text-danger">{error?.message ?? 'Could not load this folder.'}</p>
  }
  if (!data || data.entries.length === 0) {
    return <p className="px-3 py-5 text-xs text-soft-foreground">This folder is empty.</p>
  }
  return (
    <div className="p-1.5">
      {data.entries.map((entry) => (
        <button
          key={entry.path}
          type="button"
          className="flex min-h-8 w-full items-center gap-2 rounded px-2 text-left text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
          onClick={() => entry.type === 'dir' ? onDirectory(entry.path) : onFile(entry.path)}
        >
          {entryIcon(entry)}
          <span className="min-w-0 flex-1 truncate">{entry.name}</span>
          {entry.type === 'dir' ? <ChevronRightIcon className="size-3 text-soft-foreground" aria-hidden="true" /> : null}
          {entry.type === 'file' && entry.size !== undefined ? <span className="text-[10px] text-soft-foreground">{formatBytes(entry.size)}</span> : null}
        </button>
      ))}
      {data.truncated ? <p className="px-2 py-2 text-[10px] text-pending">Folder is too large to show every entry.</p> : null}
    </div>
  )
}

function Editor({
  path,
  content,
  size,
  dirty,
  saveError,
  onChange,
}: {
  path: string
  content: string
  size: number
  dirty: boolean
  saveError: string | null
  onChange: (value: string) => void
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-border px-4 py-2 text-xs md:px-5">
        <FileIcon className="size-3.5 text-muted-foreground" aria-hidden="true" />
        <span className="truncate font-mono">{path}</span>
        {dirty ? <span className="text-pending">•</span> : <CheckIcon className="size-3 text-primary" aria-label="Saved" />}
        <span className="ml-auto shrink-0 text-[10px] text-soft-foreground">{formatBytes(size)}</span>
      </div>
      {saveError ? <p className="border-b border-danger/20 bg-danger/10 px-4 py-2 text-xs text-danger">{saveError}</p> : null}
      <Textarea
        value={content}
        onChange={(event) => onChange(event.target.value)}
        spellCheck={false}
        wrap="off"
        aria-label={`Editing ${path}`}
        className="min-h-[60vh] flex-1 resize-none rounded-none border-0 bg-card px-4 py-4 font-mono text-[13px] leading-6 shadow-none focus-visible:ring-0 md:px-5"
      />
      <div className="flex items-center gap-3 border-t border-border px-4 py-2 text-[10px] text-soft-foreground md:px-5">
        <span>UTF-8</span>
        <span>Ctrl/Cmd + S to save</span>
      </div>
    </div>
  )
}
