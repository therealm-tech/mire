import { useEffect, useRef } from 'react'
import type { Message, ToolInvocation } from '../api'
import { type ChatItem, describeStop, messagePositions, type VerdictItem } from '../conversation'
import { Badge, Panel } from './primitives'

/**
 * The conversation, as a conversation.
 *
 * It is still the `messages` array the next request will carry — that has not
 * changed and cannot, because `mire` holds no session. What changed is that you
 * no longer have to read an array to follow it: a question sits on the right, an
 * answer on the left, and the tool calls a run made in between sit where they
 * happened rather than in a panel somewhere else.
 *
 * The last turn keeps a **Retry**, and only the last one: a call that failed
 * left the question in the transcript, and pressing **Send** on an empty box to
 * get it out again is a thing nobody guessed. On a question it asks it again; on
 * an answer it drops that answer first, which is how you find out whether the
 * model only said that because it had already said it.
 */
export function ChatPanel({
  items,
  live,
  busy,
  prompt,
  maxIterations,
  error,
  onPrompt,
  onMaxIterations,
  onSend,
  onStream,
  onRetry,
  onReset,
}: {
  items: ChatItem[]
  /** Text arriving chunk by chunk, shown as an answer still being written. */
  live: string | null
  busy: boolean
  prompt: string
  maxIterations: number
  error: { code: string; message: string; detail?: unknown } | null
  onPrompt: (value: string) => void
  onMaxIterations: (value: number) => void
  onSend: () => void
  onStream: () => void
  onRetry: (id: string) => void
  onReset: () => void
}) {
  const positions = messagePositions(items)
  const turns = positions.size

  // Only the last turn can be asked again: anything earlier has answers after
  // it, and re-running from the middle would silently drop them.
  const last = [...items].reverse().find((item) => item.kind === 'message')?.id ?? null

  // Following the answer as it is written is the whole reason this is a
  // transcript rather than a list, so the view keeps its end in sight.
  const foot = useRef<HTMLDivElement>(null)
  // biome-ignore lint/correctness/useExhaustiveDependencies: the effect reads nothing, it reacts — a new item or a new chunk is exactly when the view has to move.
  useEffect(() => {
    foot.current?.scrollIntoView({ block: 'end' })
  }, [items.length, live])

  return (
    <Panel
      title="Conversation"
      actions={
        <div className="flex items-center gap-2">
          <Badge tone="neutral">
            {turns} {turns === 1 ? 'turn' : 'turns'}
          </Badge>
          <button
            type="button"
            disabled={busy || (turns === 0 && items.length === 0)}
            onClick={onReset}
            className="rounded border border-stone-300 px-2 py-1 text-xs disabled:opacity-50 hover:bg-stone-100 dark:border-stone-700 dark:hover:bg-stone-800"
          >
            New conversation
          </button>
        </div>
      }
    >
      <div className="space-y-3">
        <div
          // Tall enough to hold a conversation, capped so the traffic below
          // stays one scroll away rather than one page.
          className="max-h-[28rem] space-y-3 overflow-y-auto pr-1 sm:max-h-[36rem]"
          role="log"
          aria-label="Conversation"
        >
          {items.length === 0 && live === null ? (
            <p className="py-8 text-center text-stone-500 text-sm dark:text-stone-400">
              Nothing said yet. Ask something below.
            </p>
          ) : null}

          {items.map((item) =>
            item.kind === 'message' ? (
              <Bubble
                key={item.id}
                message={item.message}
                position={positions.get(item.id) ?? 0}
                busy={busy}
                {...(item.id === last ? { onRetry: () => onRetry(item.id) } : {})}
              />
            ) : item.kind === 'activity' ? (
              <Activity key={item.id} turn={item.turn} tools={item.tools} />
            ) : (
              <Verdict key={item.id} item={item} />
            ),
          )}

          {live === null ? null : <Writing text={live} done={!busy} />}

          {busy && live === null ? (
            <p className="text-stone-500 text-sm dark:text-stone-400" role="status">
              Thinking…
            </p>
          ) : null}

          {error ? (
            <div className="rounded border border-rose-300 bg-rose-50 p-2 dark:border-rose-900 dark:bg-rose-950">
              <p className="flex flex-wrap items-baseline gap-2 text-sm">
                <Badge tone="bad">{error.code}</Badge>
                <span>{error.message}</span>
              </p>
              {error.detail === undefined ? null : (
                <pre className="mt-2 overflow-x-auto rounded bg-stone-100 p-2 font-mono text-xs dark:bg-stone-950">
                  {JSON.stringify(error.detail, null, 2)}
                </pre>
              )}
            </div>
          ) : null}

          <div ref={foot} />
        </div>

        <Composer
          prompt={prompt}
          turns={turns}
          busy={busy}
          maxIterations={maxIterations}
          onPrompt={onPrompt}
          onMaxIterations={onMaxIterations}
          onSend={onSend}
          onStream={onStream}
        />
      </div>
    </Panel>
  )
}

const ROLE_LABELS: Record<Message['role'], string> = {
  system: 'system',
  user: 'you',
  assistant: 'model',
  tool: 'tool result',
}

/** One turn of the wire history, sided by who said it. */
function Bubble({
  message,
  position,
  busy,
  onRetry,
}: {
  message: Message
  position: number
  busy: boolean
  /** Absent on every turn but the last, which is the only one that can be run again. */
  onRetry?: () => void
}) {
  const mine = message.role === 'user'
  const aside = message.role === 'system' || message.role === 'tool'

  return (
    <div className={`flex flex-col gap-1 ${mine ? 'items-end' : 'items-start'}`}>
      <div className="flex items-center gap-2 px-1">
        <span className="text-stone-500 text-xs dark:text-stone-400">
          {ROLE_LABELS[message.role]}
        </span>
        {onRetry === undefined ? null : (
          <button
            type="button"
            disabled={busy}
            onClick={onRetry}
            aria-label={`Retry turn ${position}`}
            title={
              mine
                ? 'Send the conversation again, ending on this message.'
                : 'Drop this answer and ask the same question again.'
            }
            className="text-stone-400 text-xs disabled:opacity-50 hover:text-stone-700 hover:underline dark:hover:text-stone-200"
          >
            Retry
          </button>
        )}
      </div>

      <div
        className={`max-w-[85%] rounded-2xl px-3 py-2 text-sm ${
          mine
            ? 'bg-stone-900 text-stone-50 dark:bg-stone-100 dark:text-stone-900'
            : aside
              ? 'border border-amber-300 bg-amber-50 dark:border-amber-900 dark:bg-amber-950'
              : 'border border-stone-200 bg-stone-50 dark:border-stone-800 dark:bg-stone-950'
        }`}
      >
        {message.content ? (
          // `break-words`: a URL or a base64 blob with no space in it is not a
          // reason for the page to grow sideways.
          <p className="whitespace-pre-wrap break-words">{message.content}</p>
        ) : (
          <p className="text-stone-500 italic dark:text-stone-400">no text content</p>
        )}

        {message.toolCalls && message.toolCalls.length > 0 ? (
          <div className="mt-2 space-y-1">
            {message.toolCalls.map((toolCall, index) => (
              <p
                // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity of a call within a turn
                key={index}
                className="break-all font-mono text-[11px] opacity-80"
              >
                {toolCall.name}({JSON.stringify(toolCall.arguments)})
              </p>
            ))}
            <p className="text-amber-700 text-xs dark:text-amber-500">
              Nothing answered this call — the run stopped on it, or it came back from a stream,
              which does not loop. Most endpoints refuse the next turn until it has a result:
              <strong> Retry</strong> drops this turn and asks again.
            </p>
          </div>
        ) : null}
      </div>
    </div>
  )
}

/**
 * What the run did on its own, between the question and the answer.
 *
 * A summary on purpose: the arguments, the schema check and the result in full
 * are one panel down, where every other thing that went on a wire also is.
 */
function Activity({ turn, tools }: { turn: number; tools: ToolInvocation[] }) {
  return (
    <ul className="space-y-1 border-stone-200 border-l-2 pl-3 dark:border-stone-800">
      {tools.map((tool) => (
        <li
          key={`${tool.call.id ?? ''}${tool.call.name}`}
          className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-xs"
        >
          <span className="text-stone-400 dark:text-stone-500">turn {turn}</span>
          <span className="font-medium font-mono">{tool.call.name}</span>
          {tool.source === 'mcp' ? (
            <span className="text-stone-500 dark:text-stone-400">
              called for real via <span className="font-mono">{tool.server}</span>
              {tool.latencyMs === undefined ? null : ` · ${tool.latencyMs} ms`}
            </span>
          ) : (
            <span className="text-stone-500 dark:text-stone-400">simulated, nothing executed</span>
          )}
          {tool.error ? <Badge tone="bad">tool failed</Badge> : null}
          {tool.reportedError ? <Badge tone="warn">the tool reported a problem</Badge> : null}
          {tool.schemaErrors.length > 0 ? <Badge tone="warn">schema</Badge> : null}
        </li>
      ))}
    </ul>
  )
}

/** How the run ended, in the one place where its answer sits above it. */
function Verdict({ item }: { item: VerdictItem }) {
  const { tone, text } = describeStop(item.stop)
  return (
    <p className="flex flex-wrap items-baseline gap-2 text-xs">
      <Badge tone={tone}>{item.stop.outcome}</Badge>
      <span className="text-stone-600 dark:text-stone-400">{text}</span>
      <span className="text-stone-400 dark:text-stone-500">
        {item.turns} {item.turns === 1 ? 'turn' : 'turns'} · {item.durationMs} ms
      </span>
    </p>
  )
}

/** The answer as it arrives, before the `done` event says what it really was. */
function Writing({ text, done }: { text: string; done: boolean }) {
  return (
    <div className="flex flex-col items-start gap-1">
      <div className="flex items-center gap-2 px-1">
        <span className="text-stone-500 text-xs dark:text-stone-400">model</span>
        <Badge tone={done ? 'good' : 'neutral'}>{done ? 'complete' : 'receiving…'}</Badge>
      </div>
      <div className="max-w-[85%] rounded-2xl border border-stone-200 bg-stone-50 px-3 py-2 text-sm dark:border-stone-800 dark:bg-stone-950">
        {text.length === 0 ? (
          <p className="text-stone-500 italic dark:text-stone-400">
            Connected. Nothing has arrived yet.
          </p>
        ) : (
          <p className="whitespace-pre-wrap break-words">
            {text}
            {done ? null : <span className="animate-pulse">▍</span>}
          </p>
        )}
      </div>
    </div>
  )
}

function Composer({
  prompt,
  turns,
  busy,
  maxIterations,
  onPrompt,
  onMaxIterations,
  onSend,
  onStream,
}: {
  prompt: string
  turns: number
  busy: boolean
  maxIterations: number
  onPrompt: (value: string) => void
  onMaxIterations: (value: number) => void
  onSend: () => void
  onStream: () => void
}) {
  // An empty box is nothing to say, not an instruction to send the history
  // again — that is what **Retry** is for, and it says which turn it repeats.
  const empty = prompt.trim().length === 0

  return (
    <div className="space-y-2 border-stone-200 border-t pt-3 dark:border-stone-800">
      <textarea
        value={prompt}
        aria-label="Message"
        onChange={(event) => onPrompt(event.target.value)}
        onKeyDown={(event) => {
          // Enter sends, Shift+Enter starts a line — which is what everybody's
          // fingers already do. The modifier still sends, for the pasted system
          // prompt that arrives with its own newlines.
          if (event.key !== 'Enter' || busy || empty) {
            return
          }
          if (event.shiftKey) {
            return
          }
          event.preventDefault()
          onSend()
        }}
        rows={3}
        placeholder={
          turns === 0
            ? 'Ask something. Enter sends, Shift+Enter starts a line.'
            : 'Ask something else, or retry the last turn above.'
        }
        className="w-full resize-y rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-950"
      />

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          disabled={busy || empty}
          onClick={onSend}
          className="rounded-lg bg-stone-900 px-4 py-1.5 font-medium text-sm text-stone-50 disabled:opacity-50 dark:bg-stone-100 dark:text-stone-900"
          title="Run the profile in a loop, answering its tools. A profile with none stops on turn one."
        >
          Send
        </button>
        <button
          type="button"
          disabled={busy || empty}
          onClick={onStream}
          className="rounded-lg border border-stone-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-stone-700"
          title="One turn, read chunk by chunk. Tool calls do not reassemble in a stream, so this one does not loop."
        >
          Stream
        </button>
        <label className="ml-auto flex items-center gap-1.5 text-stone-500 text-xs dark:text-stone-400">
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
      </div>
    </div>
  )
}
