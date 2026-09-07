import { useState } from 'react'
import type { ConfigDirectory } from '../api'
import { Badge, Button } from './primitives'

/**
 * The files of the configuration directories that did not load, all of them, in
 * one place under the header.
 *
 * One block rather than a complaint per panel. The issues used to be shown where
 * they were collected — the broken models at the foot of the model list, the
 * broken prompts beside the saved-prompt dropdown — which put each of them
 * behind whatever had to be unfolded to reach that panel, said nothing at all
 * about `auth/` and `mcp/`, and could not say anything about `decodes/`, which no
 * listing carries. A save that breaks a file is one event, and it is worth one
 * place to read it.
 *
 * Gone entirely when everything loaded: this is the failure case, and a bar that
 * is always there saying "0" is a bar nobody reads when it says "1".
 */
export function ConfigBanner({ directories }: { directories: ConfigDirectory[] }) {
  const broken = directories.filter((entry) => entry.issues.length > 0)
  const total = broken.reduce((count, entry) => count + entry.issues.length, 0)

  // A signature rather than the count: two saves that each break one file are
  // two different things to read, and a count would call them the same.
  const signature = broken
    .flatMap((entry) => entry.issues.map((issue) => `${issue.file}:${issue.message}`))
    .join('|')

  const [open, setOpen] = useState(true)
  // Open again whenever what it has to say changes. Folding it away is about
  // the issues that were on it, not about the ones a later save introduces —
  // and a save landing on a folded bar is exactly the moment to unfold.
  //
  // Adjusted during the render rather than in an effect, which is what React
  // asks for when state has to follow a prop: an effect would paint the folded
  // bar once and unfold it on the pass after.
  const [shown, setShown] = useState(signature)
  if (shown !== signature) {
    setShown(signature)
    setOpen(true)
  }

  if (total === 0) {
    return null
  }

  return (
    <section
      aria-label="Configuration load errors"
      role="status"
      className="flex gap-2.5 rounded border border-line border-l-[3px] border-l-bad bg-bad-soft p-2.5"
    >
      <Badge tone="bad">{total}</Badge>

      <div className="min-w-0 flex-1">
        <p className="flex flex-wrap items-baseline gap-2">
          <span className="font-medium text-sm">
            {total === 1 ? '1 file' : `${total} files`} of the configuration did not load
          </span>
          <span className="text-faint text-xs">
            {broken.map((entry) => `${entry.name}/`).join(' · ')}
          </span>
        </p>

        {open ? (
          <div className="mt-2 space-y-2 border-bad/25 border-t pt-2">
            {broken.map((entry) => (
              <div key={entry.name}>
                <h3 className="font-semibold text-faint text-xs uppercase tracking-wide">
                  {entry.name}/
                </h3>
                {entry.issues.map((issue) => (
                  <p key={`${issue.file}:${issue.line}:${issue.message}`} className="mt-1">
                    <span className="break-all font-mono text-bad text-xs">
                      {issue.file}
                      {issue.line === null ? '' : `:${issue.line}:${issue.column ?? 0}`}
                    </span>
                    <span className="block text-muted text-xs">{issue.message}</span>
                  </p>
                ))}
              </div>
            ))}
            <p className="text-faint text-xs">
              Everything else loaded. Fix the file and save it — the directories are read again
              without a restart.
            </p>
          </div>
        ) : null}
      </div>

      <Button aria-expanded={open} onClick={() => setOpen((shown) => !shown)}>
        {open ? 'Hide' : 'Show'}
      </Button>
    </section>
  )
}
