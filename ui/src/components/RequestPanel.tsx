import type { CallOutcome, ProfileSummary } from '../api'
import { Code, CopyButton, Field, Panel } from './primitives'

export function RequestPanel({
  profile,
  prompt,
  turns,
  input,
  repeat,
  includeVectors,
  maxIterations,
  busy,
  onPrompt,
  onInput,
  onRepeat,
  onIncludeVectors,
  onMaxIterations,
  onDryRun,
  onSend,
  onStream,
  onLoop,
}: {
  profile: ProfileSummary
  prompt: string
  /** Turns already in the conversation, so the box can say what it is joining. */
  turns: number
  input: string
  repeat: number
  includeVectors: boolean
  maxIterations: number
  busy: boolean
  onPrompt: (value: string) => void
  onInput: (value: string) => void
  onRepeat: (value: number) => void
  onIncludeVectors: (value: boolean) => void
  onMaxIterations: (value: number) => void
  onDryRun: () => void
  onSend: () => void
  onStream: () => void
  onLoop: () => void
}) {
  const embedding = profile.kind === 'embedding'

  return (
    <Panel title={embedding ? 'Input' : 'Prompt'}>
      <div className="space-y-3">
        {embedding ? (
          <>
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
          </>
        ) : (
          <Field label={turns === 0 ? 'Message' : `Next message (${turns} before it)`}>
            <textarea
              value={prompt}
              onChange={(event) => onPrompt(event.target.value)}
              onKeyDown={(event) => {
                // The box stays multi-line, so a bare Enter has to keep meaning
                // "newline". The modifier is what sends, as everywhere else.
                if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !busy) {
                  event.preventDefault()
                  onSend()
                }
              }}
              rows={4}
              placeholder={
                turns === 0 ? undefined : 'Leave empty to resend the conversation unchanged'
              }
              className="w-full rounded border border-stone-300 bg-white px-2 py-1 text-sm dark:border-stone-700 dark:bg-stone-950"
            />
          </Field>
        )}

        <div className="flex flex-wrap items-end gap-2">
          <button
            type="button"
            disabled={busy}
            onClick={onSend}
            className="rounded bg-stone-900 px-3 py-1.5 font-medium text-sm text-stone-50 disabled:opacity-50 dark:bg-stone-100 dark:text-stone-900"
          >
            Send
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={onDryRun}
            className="rounded border border-stone-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-stone-700"
          >
            Dry run
          </button>

          {embedding ? null : (
            <>
              <button
                type="button"
                disabled={busy}
                onClick={onStream}
                className="rounded border border-stone-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-stone-700"
                title="Ask the endpoint to stream, and read it chunk by chunk"
              >
                Stream
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={onLoop}
                className="rounded border border-stone-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-stone-700"
                title="Run the profile in a loop, answering its simulated tools"
              >
                Run agent
              </button>
              <label className="flex items-center gap-1.5 pb-1 text-xs">
                max turns
                <input
                  type="number"
                  min={1}
                  max={50}
                  value={maxIterations}
                  onChange={(event) => onMaxIterations(Number(event.target.value))}
                  className="w-16 rounded border border-stone-300 bg-white px-2 py-1 text-sm dark:border-stone-700 dark:bg-stone-950"
                />
              </label>
            </>
          )}
        </div>
      </div>
    </Panel>
  )
}

/** What would actually go on the wire, credentials already masked. */
export function RenderedRequest({ outcome }: { outcome: CallOutcome }) {
  return (
    <Panel
      title="Rendered request"
      actions={<CopyButton text={outcome.curl} label="Copy as curl" />}
    >
      <p className="mb-2 break-all font-mono text-xs">
        <span className="font-semibold">{outcome.request.method}</span> {outcome.request.url}
      </p>
      <ul className="mb-2 space-y-0.5 font-mono text-[11px] text-stone-600 dark:text-stone-400">
        {Object.entries(outcome.request.headers).map(([name, value]) => (
          <li key={name}>
            {name}: {value}
          </li>
        ))}
      </ul>
      <Code>{outcome.request.body}</Code>
    </Panel>
  )
}
