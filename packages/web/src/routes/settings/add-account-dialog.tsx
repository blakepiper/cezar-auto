import { useState } from 'react'

import { useCreateAgentProfile } from '@/api/queries'
import type { ProviderId } from '@open-mercato/cezar-api-client'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { toast } from '@/components/ui/toaster'

/**
 * "Add agent account" (spec 2026-07-29-agent-profiles): point a provider at a second config
 * folder, so a work login can sit beside a personal one.
 *
 * **The typed path is the source of truth, not the browser selection.** Two reasons, and both are
 * disqualifying for a browse-only picker:
 *
 * 1. every candidate is a dotfolder (`~/.claude-second`), and a folder that does not exist yet
 *    cannot be browsed to at all — yet that is the normal case, because the documented flow is
 *    *add the account → Connect → the CLI creates the folder on login*;
 * 2. `GET /api/v1/fs/browse` is refused in single-project mode and is rooted at the configured
 *    browse root, so a browse-only field would make the whole feature unreachable for anyone
 *    whose accounts live outside it.
 *
 * A15 (decision 8) retired the folder-browse assist along with `fs-browse.ts` / `GET
 * /api/v1/fs/browse` — the typed field was already the source of truth for exactly the two
 * reasons above, so it is now the only way in. cezar still does not go LOOKING for accounts —
 * see the spec's "no globbing": a folder is an account because the user said so.
 *
 * Validation stays server-side (absolute after `~` expansion, not already another account's
 * folder, no control characters); this dialog only refuses an empty field, because that is not a
 * request worth sending.
 */
export function AddAccountDialog({
  open,
  onOpenChange,
  providers,
  initialProvider,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Providers that can carry more than one account — the server decides, not this dialog. */
  providers: ProviderId[]
  /** Which agent's "Add account" was clicked; still switchable in the dialog. */
  initialProvider?: ProviderId
}) {
  const [provider, setProvider] = useState<ProviderId>(initialProvider ?? providers[0] ?? 'claude')
  const [label, setLabel] = useState('')
  const [configDir, setConfigDir] = useState('')
  const create = useCreateAgentProfile()

  const trimmed = configDir.trim()

  const add = () => {
    if (trimmed === '' || create.isPending) return
    create.mutate(
      { provider, configDir: trimmed, ...(label.trim() ? { label: label.trim() } : {}) },
      {
        onSuccess: () => {
          onOpenChange(false)
          setLabel('')
          setConfigDir('')
          toast('Account added — use Connect to sign in')
        },
      },
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent data-slot="add-account-dialog" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Add agent account</DialogTitle>
          <DialogDescription>
            The config folder this account uses. It does not have to exist yet — Connect signs in
            and the CLI creates it.
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-2 text-[13px]">
            <span className="text-muted-foreground">Agent</span>
            <select
              aria-label="Agent"
              data-slot="add-account-provider"
              value={provider}
              onChange={(event) => setProvider(event.target.value as ProviderId)}
              className="rounded-md border border-input bg-card px-2 py-1 text-[13px] outline-none focus-visible:border-ring"
            >
              {providers.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          </label>
          <label className="flex min-w-0 flex-1 items-center gap-2 text-[13px]">
            <span className="shrink-0 text-muted-foreground">Name</span>
            <input
              type="text"
              aria-label="Account name"
              data-slot="add-account-label"
              value={label}
              placeholder="Work"
              onChange={(event) => setLabel(event.target.value)}
              className="min-w-0 flex-1 rounded-md border border-input bg-card px-2 py-1 text-[13px] outline-none focus-visible:border-ring"
            />
          </label>
        </div>

        <div className="flex flex-col gap-1.5">
          <label className="flex min-w-0 items-center gap-2 text-[13px]">
            <span className="shrink-0 text-muted-foreground">Folder</span>
            <input
              type="text"
              spellCheck={false}
              autoComplete="off"
              aria-label="Config folder"
              data-slot="add-account-dir"
              value={configDir}
              placeholder={provider === 'codex' ? '~/.codex-second' : '~/.claude-second'}
              onChange={(event) => {
                setConfigDir(event.target.value)
                create.reset()
              }}
              className="min-w-0 flex-1 rounded-md border border-input bg-card px-2 py-1 font-mono text-[12.5px] outline-none focus-visible:border-ring"
            />
          </label>
          <p className="text-[11.5px] text-soft-foreground">
            A <code>~</code> is kept as written and expanded when the agent runs.
          </p>
        </div>

        {/* The server's own words: "that is already this agent's default folder", "already used
            by …", "must be an absolute path". This dialog cannot know which applies. */}
        {create.isError ? (
          <p data-slot="add-account-error" className="min-w-0 break-words text-[13px] text-danger">
            {create.error instanceof Error ? create.error.message : 'could not add that folder'}
          </p>
        ) : null}

        <DialogFooter className="min-w-0 sm:items-center sm:justify-end">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            data-slot="add-account-confirm"
            disabled={trimmed === '' || create.isPending}
            onClick={add}
          >
            {create.isPending ? 'Adding…' : 'Add account'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
