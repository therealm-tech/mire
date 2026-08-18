import { useId } from 'react'
import type { LoadIssue, Prompt } from '../api'

/**
 * The questions worth keeping, one dropdown away from the box.
 *
 * They come from `prompts.yaml`, which is a file somebody edits — so this is a
 * picker and never an editor, exactly like the profile list next to it. Picking
 * one drops its text in the box and stops there: nothing is sent, and what the
 * text becomes on the wire is still the profile's template's decision.
 *
 * It **replaces** what is in the box rather than appending to it, which is the
 * honest reading of "load the saved one" — and why the label says so.
 *
 * Gone entirely when the file declares nothing and complains about nothing: a
 * permanently empty dropdown is a control that only ever wastes a click.
 */
export function SavedPrompts({
  prompts,
  issues,
  disabled,
  onPick,
}: {
  /** Every prompt that loaded, in the order the file wrote them. */
  prompts: Prompt[]
  /** Entries of `prompts.yaml` that did not, so a typo is visible where it bites. */
  issues: LoadIssue[]
  disabled: boolean
  onPick: (text: string) => void
}) {
  // Two of these can be on the page over a session — the composer's and the
  // embedding box's — and a hard-coded id would tie both labels to the first.
  const id = useId()

  if (prompts.length === 0 && issues.length === 0) {
    return null
  }

  return (
    <div className="flex flex-wrap items-center gap-2">
      <label className="font-medium text-muted text-xs" htmlFor={id}>
        Saved
      </label>
      <select
        id={id}
        // Always back to the placeholder, so picking the same prompt twice fires
        // twice. A select that remembers its choice would look like a button
        // that stopped working the second time you reached for it.
        value=""
        disabled={disabled || prompts.length === 0}
        onChange={(event) => {
          const picked = prompts.find((prompt) => prompt.name === event.target.value)
          if (picked) {
            onPick(picked.text)
          }
        }}
        title="Replaces what is in the box. Nothing is sent."
        className="max-w-48 rounded border border-line-strong bg-panel px-2 py-1 text-ink text-xs disabled:opacity-50"
      >
        <option value="">pick one…</option>
        {prompts.map((prompt) => (
          <option key={prompt.name} value={prompt.name}>
            {prompt.name}
          </option>
        ))}
      </select>

      {issues.length > 0 ? (
        <span className="text-[11px] text-bad">
          {issues.length === 1 ? '1 entry' : `${issues.length} entries`} of prompts.yaml did not
          load: {issues.map((issue) => issue.message).join('; ')}
        </span>
      ) : null}
    </div>
  )
}
