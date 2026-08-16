import { useState } from 'react'
import { useNavigate } from 'react-router'

import { useRegisterProject } from '@/api/queries'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

/**
 * "Add project → Open local folder" (multi-project spec, "Add project" / step 4.2).
 *
 * Registers the typed folder with `POST /api/v1/projects`, then navigates to the new project's
 * scope. A15 (decision 8) retired the folder-BROWSE assist along with `fs-browse.ts` / `GET
 * /api/v1/fs/browse` and its realpath-containment model — typing (or pasting) the absolute path
 * is now the only way in, same as "Add agent account" already worked.
 *
 * A non-git folder is accepted: cezar degrades in a non-git folder exactly as `cezar serve` does
 * today, so refusing one here would invent a restriction the server does not have.
 *
 * The register error is shown VERBATIM: the server writes it for the person reading it, and this
 * dialog cannot know which situation (a home directory, a duplicate, an unreadable path) applies.
 */
export function AddProjectDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const [path, setPath] = useState('')
  const register = useRegisterProject()
  const navigate = useNavigate()

  const trimmed = path.trim()

  const add = () => {
    if (trimmed === '' || register.isPending) return
    register.mutate(trimmed, {
      onSuccess: ({ project }) => {
        onOpenChange(false)
        setPath('')
        // Raw react-router `useNavigate`, not the scope-aware wrapper: this is a deliberate
        // cross-project jump, and `/p/…` targets pass through the wrapper untouched anyway.
        navigate(`/p/${encodeURIComponent(project.id)}/`)
      },
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent data-slot="add-project-dialog" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Open local folder</DialogTitle>
          <DialogDescription>
            The absolute path to the folder cezar should run in. Any folder works; git repos get
            the full worktree/diff experience.
          </DialogDescription>
        </DialogHeader>

        <input
          type="text"
          spellCheck={false}
          autoComplete="off"
          aria-label="Project folder"
          data-slot="add-project-path"
          value={path}
          placeholder="~/code/my-repo"
          onChange={(event) => {
            setPath(event.target.value)
            register.reset()
          }}
          className="min-w-0 flex-1 rounded-md border border-input bg-card px-2 py-1 font-mono text-[12.5px] outline-none focus-visible:border-ring"
        />

        {register.isError ? (
          <p data-slot="add-project-error" className="min-w-0 break-words text-[13px] text-danger">
            {register.error instanceof Error ? register.error.message : 'could not add that folder'}
          </p>
        ) : null}

        <DialogFooter className="min-w-0 sm:items-center sm:justify-end">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            data-slot="add-project-confirm"
            disabled={trimmed === '' || register.isPending}
            onClick={add}
          >
            {register.isPending ? 'Adding…' : 'Add project'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
