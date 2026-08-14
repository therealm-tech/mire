import { useEffect, useRef } from 'react'
import type { Message } from '../api'
import {
  type ActivityItem,
  type ChatItem,
  describeStop,
  messagePositions,
  type VerdictItem,
} from '../conversation'
import { Failure } from './Failure'
import { Markdown } from './Markdown'
import { McpProtocol } from './McpProtocol'
import { Badge, Button, INPUT_CLASSES, Panel } from './primitives'

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
  stopped,
  prompt,
  maxIterations,
  streaming,
  error,
  revisions,
  mcpProtocol,
  showProtocol,
  onPrompt,
  onMaxIterations,
  onStreaming,
  onMcpProtocol,
  onSend,
  onStop,
  onRetry,
  onReveal,
  onReset,
}: {
  items: ChatItem[]
  /** Text arriving chunk by chunk, shown as an answer still being written. */
  live: string | null
  busy: boolean
  /** The last run was called off rather than finished. */
  stopped: boolean
  prompt: string
  maxIterations: number
  /** How **Send** sends: chunk by chunk on one turn, or the loop on all of them. */
  streaming: boolean
  error: { code: string; message: string; detail?: unknown } | null
  revisions: string[]
  mcpProtocol: string | null
  /** Only a profile that names a server has a revision to speak. */
  showProtocol: boolean
  onPrompt: (value: string) => void
  onMaxIterations: (value: number) => void
  onStreaming: (value: boolean) => void
  onMcpProtocol: (revision: string | null) => void
  onSend: () => void
  onStop: () => void
  onRetry: (id: string) => void
  onReveal: (exchange: string) => void
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
          <Button disabled={busy || (turns === 0 && items.length === 0)} onClick={onReset}>
            New conversation
          </Button>
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
            <p className="py-8 text-center text-muted text-sm">
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
              <Activity key={item.id} item={item} onReveal={onReveal} />
            ) : (
              <Verdict key={item.id} item={item} />
            ),
          )}

          {live === null ? null : <Writing text={live} done={!busy} />}

          {busy && live === null ? (
            <p className="text-muted text-sm" role="status">
              Thinking…
            </p>
          ) : null}

          {stopped && !busy ? (
            <p className="text-muted text-sm" role="status">
              Stopped. Whatever had arrived is above, and on the wire below.
            </p>
          ) : null}

          {error ? <Failure error={error} /> : null}

          <div ref={foot} />
        </div>

        <Composer
          prompt={prompt}
          turns={turns}
          busy={busy}
          maxIterations={maxIterations}
          streaming={streaming}
          revisions={revisions}
          mcpProtocol={mcpProtocol}
          showProtocol={showProtocol}
          onPrompt={onPrompt}
          onMaxIterations={onMaxIterations}
          onStreaming={onStreaming}
          onMcpProtocol={onMcpProtocol}
          onSend={onSend}
          onStop={onStop}
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
        <span className="text-faint text-xs">{ROLE_LABELS[message.role]}</span>
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
            className="text-faint text-xs disabled:opacity-50 hover:text-ink hover:underline"
          >
            Retry
          </button>
        )}
      </div>

      <div
        className={`max-w-[85%] rounded-2xl px-3 py-2 text-sm ${
          mine
            ? 'bg-brand text-on-brand'
            : aside
              ? 'border border-warn/30 bg-warn-soft'
              : 'border border-line bg-paper'
        }`}
      >
        {message.content ? (
          // Only the model's half is prose that was written in markdown. A
          // question was typed by hand, a tool result is JSON, and a system
          // prompt is an instruction — rendering any of those would be showing
          // someone something other than what is on the wire.
          message.role === 'assistant' ? (
            <Markdown>{message.content}</Markdown>
          ) : (
            // `break-words`: a URL or a base64 blob with no space in it is not a
            // reason for the page to grow sideways.
            <p className="whitespace-pre-wrap break-words">{message.content}</p>
          )
        ) : (
          <p className="text-faint italic">no text content</p>
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
            <p className="text-warn text-xs">
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
 * are one panel down, where every other thing that went on a wire also is — and
 * every row here is a way of getting to its own card down there, because "which
 * of these forty is the one I just read about" was a question the reader was
 * being left to answer by eye.
 */
function Activity({ item, onReveal }: { item: ActivityItem; onReveal: (id: string) => void }) {
  return (
    <ul className="space-y-1 border-line border-l-2 pl-3">
      {item.tools.map((exchange) => {
        const tool = exchange.invocation
        return (
          <li key={exchange.id} className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-xs">
            {item.call === null ? (
              <span className="text-faint">turn {item.turn}</span>
            ) : (
              <button
                type="button"
                onClick={() => {
                  if (item.call) {
                    onReveal(item.call)
                  }
                }}
                className="text-faint hover:text-ink hover:underline"
                title="Show the model call that asked for this"
              >
                turn {item.turn}
              </button>
            )}
            <button
              type="button"
              onClick={() => onReveal(exchange.id)}
              className="font-medium font-mono hover:underline"
              title="Show this call in the traffic below"
            >
              {tool.call.name}
            </button>
            {tool.source === 'mcp' ? (
              <span className="text-muted">
                called for real via <span className="font-mono">{tool.server}</span>
                {tool.latencyMs === undefined ? null : ` · ${tool.latencyMs} ms`}
              </span>
            ) : (
              <span className="text-muted">simulated, nothing executed</span>
            )}
            {tool.error ? <Badge tone="bad">tool failed</Badge> : null}
            {tool.reportedError ? <Badge tone="warn">the tool reported a problem</Badge> : null}
            {tool.schemaErrors.length > 0 ? <Badge tone="warn">schema</Badge> : null}
          </li>
        )
      })}
    </ul>
  )
}

/** How the run ended, in the one place where its answer sits above it. */
function Verdict({ item }: { item: VerdictItem }) {
  const { tone, text } = describeStop(item.stop)
  return (
    <p className="flex flex-wrap items-baseline gap-2 text-xs">
      <Badge tone={tone}>{item.stop.outcome}</Badge>
      <span className="text-muted">{text}</span>
      <span className="text-faint">
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
        <span className="text-faint text-xs">model</span>
        <Badge tone={done ? 'good' : 'neutral'}>{done ? 'complete' : 'receiving…'}</Badge>
      </div>
      <div className="max-w-[85%] rounded-2xl border border-line bg-paper px-3 py-2 text-sm">
        {text.length === 0 ? (
          <p className="text-faint italic">Connected. Nothing has arrived yet.</p>
        ) : (
          // The caret goes into the source rather than after the render, which
          // is the only way it lands at the end of the last line instead of on a
          // line of its own — and inside an unclosed fence, which is where a
          // half-arrived code block genuinely is.
          <Markdown>{done ? text : `${text}▍`}</Markdown>
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
  streaming,
  revisions,
  mcpProtocol,
  showProtocol,
  onPrompt,
  onMaxIterations,
  onStreaming,
  onMcpProtocol,
  onSend,
  onStop,
}: {
  prompt: string
  turns: number
  busy: boolean
  maxIterations: number
  streaming: boolean
  revisions: string[]
  mcpProtocol: string | null
  showProtocol: boolean
  onPrompt: (value: string) => void
  onMaxIterations: (value: number) => void
  onStreaming: (value: boolean) => void
  onMcpProtocol: (revision: string | null) => void
  onSend: () => void
  onStop: () => void
}) {
  // An empty box is nothing to say, not an instruction to send the history
  // again — that is what **Retry** is for, and it says which turn it repeats.
  const empty = prompt.trim().length === 0

  return (
    <div className="space-y-2 border-line border-t pt-3">
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
        className={`${INPUT_CLASSES} w-full resize-y`}
      />

      <div className="flex flex-wrap items-center gap-2">
        {/*
          One button, because there was only ever one thing to do with what is in
          the box. How it goes out is a setting, and a setting is a checkbox.
        */}
        <Button
          variant="primary"
          size="md"
          disabled={busy || empty}
          onClick={onSend}
          title={
            streaming
              ? 'One turn, read chunk by chunk. Tool calls do not reassemble in a stream, so this does not loop.'
              : 'Run the profile in a loop, answering its tools. A profile with none stops on turn one.'
          }
        >
          Send
        </Button>
        <label className="flex items-center gap-1.5 text-muted text-xs">
          <input
            type="checkbox"
            checked={streaming}
            disabled={busy}
            onChange={(event) => onStreaming(event.target.checked)}
          />
          stream
        </label>
        {/*
          Only while there is something to stop. A permanently disabled Stop
          would be a second button competing with the one that does something.
        */}
        {busy ? (
          <Button size="md" onClick={onStop} title="Drop this request. What has arrived stays.">
            Stop
          </Button>
        ) : null}
        {/*
          Inert while streaming, and shown as inert rather than quietly ignored:
          a stream is one turn, so there is no second one to cap.
        */}
        <label
          className={`ml-auto flex items-center gap-1.5 text-muted text-xs ${
            streaming ? 'opacity-50' : ''
          }`}
          title={streaming ? 'A stream is one turn. Uncheck stream to run the loop.' : undefined}
        >
          max turns
          <input
            type="number"
            min={1}
            max={50}
            value={maxIterations}
            disabled={streaming}
            onChange={(event) => onMaxIterations(Number(event.target.value))}
            className={`${INPUT_CLASSES} w-16`}
          />
        </label>
      </div>

      <p className="text-faint text-xs">
        With <strong className="font-medium">stream</strong> on, <strong>Send</strong> is one turn,
        read as it arrives — which is the only way to see time to first token, and the only way to
        watch the answer being written. Tool calls do not reassemble in a stream, so it does not
        loop: turn it off and <strong>Send</strong> runs the loop instead, answering the tools the
        model asks for until it stops asking.
      </p>

      {/*
        A run parameter, so it sits with the other one. It used to live in the
        auth panel, which is the one thing on the page it is not about. Inert
        while streaming for the same reason **max turns** is: a stream calls no
        tools, so it never speaks to a server at all.
      */}
      {showProtocol ? (
        <McpProtocol
          revisions={revisions}
          selected={mcpProtocol}
          disabled={busy || streaming}
          onSelect={onMcpProtocol}
        />
      ) : null}
    </div>
  )
}
