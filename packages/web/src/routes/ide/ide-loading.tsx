import { Code2Icon, LoaderCircleIcon } from 'lucide-react'

/** Lightweight Suspense fallback for the lazy IDE surface. */
export function IdeLoading() {
  return (
    <div data-route="ide" className="flex min-h-full flex-col">
      <div className="flex items-center gap-3 border-b border-border px-4 py-3 md:px-6">
        <Code2Icon className="size-4 text-muted-foreground" aria-hidden="true" />
        <h1 className="text-base font-semibold">IDE</h1>
      </div>
      <p className="flex items-center justify-center gap-2 px-4 py-10 text-xs text-soft-foreground">
        <LoaderCircleIcon className="size-3.5 motion-safe:animate-spin" aria-hidden="true" />
        Loading editor…
      </p>
    </div>
  )
}
