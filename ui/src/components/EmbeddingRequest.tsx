import { Field, Panel } from './primitives'

/**
 * The input side of an embedding profile.
 *
 * There is no second turn of an embedding, so there is no conversation here and
 * no loop to run — one call, one answer, and the interesting part is the shape
 * of what comes back rather than what it says.
 */
export function EmbeddingRequest({
  input,
  repeat,
  includeVectors,
  busy,
  onInput,
  onRepeat,
  onIncludeVectors,
  onSend,
}: {
  input: string
  repeat: number
  includeVectors: boolean
  busy: boolean
  onInput: (value: string) => void
  onRepeat: (value: number) => void
  onIncludeVectors: (value: boolean) => void
  onSend: () => void
}) {
  return (
    <Panel title="Input">
      <div className="space-y-3">
        <Field label="One text per line">
          <textarea
            value={input}
            onChange={(event) => onInput(event.target.value)}
            rows={4}
            className="w-full rounded border border-stone-300 bg-white px-2 py-1 font-mono text-sm dark:border-stone-700 dark:bg-stone-950"
          />
        </Field>

        <div className="flex flex-wrap items-end gap-4">
          <Field label="Runs (2+ checks determinism)">
            <input
              type="number"
              min={1}
              max={5}
              value={repeat}
              onChange={(event) => onRepeat(Number(event.target.value))}
              className="w-20 rounded border border-stone-300 bg-white px-2 py-1 text-sm dark:border-stone-700 dark:bg-stone-950"
            />
          </Field>
          <label className="flex items-center gap-2 pb-1 text-xs">
            <input
              type="checkbox"
              checked={includeVectors}
              onChange={(event) => onIncludeVectors(event.target.checked)}
            />
            Attach full vectors
          </label>
        </div>

        <button
          type="button"
          disabled={busy}
          onClick={onSend}
          className="rounded-lg bg-stone-900 px-4 py-1.5 font-medium text-sm text-stone-50 disabled:opacity-50 dark:bg-stone-100 dark:text-stone-900"
        >
          Send
        </button>
      </div>
    </Panel>
  )
}
