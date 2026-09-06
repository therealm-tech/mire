import type { PromptsResponse } from '../api'
import { Button, Field, INPUT_CLASSES, Panel } from './primitives'
import { SavedPrompts } from './SavedPrompts'

/**
 * The input side of an embedding model.
 *
 * There is no second turn of an embedding, so there is no conversation here and
 * no loop to run — one call, one answer, and the interesting part is the shape
 * of what comes back rather than what it says.
 */
export function EmbeddingRequest({
  input,
  prompts,
  repeat,
  includeVectors,
  busy,
  missingSignIn,
  onInput,
  onRepeat,
  onIncludeVectors,
  onSend,
  onStop,
}: {
  input: string
  /** The same library the composer offers: a saved text is a text either box takes. */
  prompts: PromptsResponse
  repeat: number
  includeVectors: boolean
  busy: boolean
  /**
   * An identity this call needs has no browser session behind it, so it would be
   * refused here rather than by the endpoint. There is no loop and no MCP server
   * in an embedding run, so this is only ever the model's own `auth:`.
   */
  missingSignIn: boolean
  onInput: (value: string) => void
  onRepeat: (value: number) => void
  onIncludeVectors: (value: boolean) => void
  onSend: () => void
  onStop: () => void
}) {
  return (
    <Panel title="Input">
      <div className="space-y-3">
        <SavedPrompts
          prompts={prompts.prompts}
          issues={prompts.issues}
          disabled={busy}
          onPick={onInput}
        />

        <Field label="One text per line">
          <textarea
            value={input}
            onChange={(event) => onInput(event.target.value)}
            rows={4}
            className={`${INPUT_CLASSES} w-full font-mono`}
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
              className={`${INPUT_CLASSES} w-20`}
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

        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            size="md"
            disabled={busy || missingSignIn}
            onClick={onSend}
            title={
              missingSignIn
                ? 'An identity this run needs has nobody signed in to it. Sign in above, and Send comes back.'
                : undefined
            }
          >
            Send
          </Button>
          {busy ? (
            <Button size="md" onClick={onStop} title="Drop this request. What has arrived stays.">
              Stop
            </Button>
          ) : null}
        </div>
      </div>
    </Panel>
  )
}
