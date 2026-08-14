import { useEffect, useId, useRef, useState } from 'react'
import type { Content, ContentPart, Message } from '../api'
import {
  type Attachment,
  blocked,
  humanSize,
  MAX_FILE_BYTES,
  MAX_UPLOAD_BYTES,
  type Rejection,
  SHAPE_LABELS,
  type Shape,
  shapesFor,
} from '../attachments'
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
  attachments,
  rejections,
  uploadTo,
  maxIterations,
  streaming,
  error,
  revisions,
  mcpProtocol,
  showProtocol,
  onPrompt,
  onAttach,
  onShape,
  onDetach,
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
  /** Files the next turn will carry, already read. */
  attachments: Attachment[]
  /** Files that were offered and refused, with the reason. */
  rejections: Rejection[]
  /**
   * The server a file would be uploaded to, or `null` when this profile has
   * none — which is what decides whether *as an upload* is offered at all.
   */
  uploadTo: string | null
  maxIterations: number
  /** How **Send** sends: chunk by chunk on one turn, or the loop on all of them. */
  streaming: boolean
  error: { code: string; message: string; detail?: unknown } | null
  revisions: string[]
  mcpProtocol: string | null
  /** Only a profile that names a server has a revision to speak. */
  showProtocol: boolean
  onPrompt: (value: string) => void
  onAttach: (files: File[]) => void
  onShape: (id: string, shape: Shape) => void
  onDetach: (id: string) => void
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
          attachments={attachments}
          rejections={rejections}
          uploadTo={uploadTo}
          turns={turns}
          busy={busy}
          maxIterations={maxIterations}
          streaming={streaming}
          revisions={revisions}
          mcpProtocol={mcpProtocol}
          showProtocol={showProtocol}
          onPrompt={onPrompt}
          onAttach={onAttach}
          onShape={onShape}
          onDetach={onDetach}
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
      {/*
        `select-none`, because this row is chrome and the bubble under it is the
        thing that was said. A drag that starts a few pixels high — which is most
        of them, the bubble being sided rather than full width — used to put
        "you Retry" at the top of whatever you pasted. Copying a question to ask
        it somewhere else is the most ordinary thing to do with one, and it was
        handing back the label and a button.

        The whole run, attribution and all, has its own way out: the export
        button hands over the `messages` array as JSON, which is a better answer
        to "send me your conversation" than a screen-scrape ever was.
      */}
      <div className="flex select-none items-center gap-2 px-1">
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
        <Said content={message.content} role={message.role} />

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
 * What a turn actually says, whether or not it carries files.
 *
 * A turn with an image in it is drawn with the image in it. That is not a
 * flourish: the transcript is the `messages` array laid out, and the one
 * question you have after attaching a screenshot is whether *that* screenshot
 * is what went out. The base64 behind it is in **Traffic**, where every other
 * byte on the wire also is.
 */
function Said({ content, role }: { content: Content | undefined; role: Message['role'] }) {
  // Only the model's half is prose that was written in markdown. A question was
  // typed by hand, a tool result is JSON, and a system prompt is an instruction
  // — rendering any of those would be showing someone something other than what
  // is on the wire.
  const prose = role === 'assistant'

  if (content === undefined || content === '') {
    return <p className="text-faint italic">no text content</p>
  }
  if (typeof content === 'string') {
    return <Words prose={prose}>{content}</Words>
  }
  return (
    <div className="space-y-2">
      {content.map((part, index) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity of a part within a turn
        <Piece key={index} part={part} prose={prose} />
      ))}
    </div>
  )
}

/**
 * Words in a turn, read as markdown only where markdown is what they are.
 *
 * Carried down into the parts rather than decided once above them, because a
 * turn that came back from an endpoint may be several parts and still be the
 * model's own prose. The rule is about *who said it*, not about how many pieces
 * it arrived in.
 */
function Words({ prose, children }: { prose: boolean; children: string }) {
  return prose ? (
    <Markdown>{children}</Markdown>
  ) : (
    // `break-words`: a URL or a base64 blob with no space in it is not a
    // reason for the page to grow sideways.
    <p className="whitespace-pre-wrap break-words">{children}</p>
  )
}

/** One part of a multipart turn. */
function Piece({ part, prose }: { part: ContentPart; prose: boolean }) {
  if (part.type === 'text') {
    return <Words prose={prose}>{part.text}</Words>
  }
  if (part.type === 'image_url') {
    return (
      <img
        src={part.image_url.url}
        alt="attached to this turn"
        className="max-h-48 rounded border border-line-strong"
      />
    )
  }
  return (
    <p className="flex items-baseline gap-1.5 text-xs opacity-80">
      <span aria-hidden="true">📎</span>
      <span className="font-mono">{part.file.filename ?? part.file.file_id ?? 'a file'}</span>
    </p>
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
      {/* Chrome, and unselectable for the same reason a bubble's label is. */}
      <div className="flex select-none items-center gap-2 px-1">
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
  attachments,
  rejections,
  uploadTo,
  turns,
  busy,
  maxIterations,
  streaming,
  revisions,
  mcpProtocol,
  showProtocol,
  onPrompt,
  onAttach,
  onShape,
  onDetach,
  onMaxIterations,
  onStreaming,
  onMcpProtocol,
  onSend,
  onStop,
}: {
  prompt: string
  attachments: Attachment[]
  rejections: Rejection[]
  uploadTo: string | null
  turns: number
  busy: boolean
  maxIterations: number
  streaming: boolean
  revisions: string[]
  mcpProtocol: string | null
  showProtocol: boolean
  onPrompt: (value: string) => void
  onAttach: (files: File[]) => void
  onShape: (id: string, shape: Shape) => void
  onDetach: (id: string) => void
  onMaxIterations: (value: number) => void
  onStreaming: (value: boolean) => void
  onMcpProtocol: (revision: string | null) => void
  onSend: () => void
  onStop: () => void
}) {
  // An empty box is nothing to say, not an instruction to send the history
  // again — that is what **Retry** is for, and it says which turn it repeats.
  // A file on its own *is* something to say: "look at this" is a question.
  const empty = prompt.trim().length === 0 && attachments.length === 0
  // A turn holding a file that never reached its store would go out quoting an
  // id no tool can resolve, and the run would fail three requests later with
  // nothing on screen to connect the two. It waits here instead.
  const waiting = blocked(attachments)
  const cannotSend = busy || empty || waiting !== null

  const picker = useId()
  const [over, setOver] = useState(false)

  return (
    <div className="space-y-2 border-line border-t pt-3">
      {attachments.length === 0 ? null : (
        <ul className="flex flex-wrap gap-2" aria-label="Attached files">
          {attachments.map((attachment) => (
            <Chip
              key={attachment.id}
              attachment={attachment}
              busy={busy}
              canUpload={uploadTo !== null}
              onShape={onShape}
              onDetach={onDetach}
            />
          ))}
        </ul>
      )}

      {rejections.map((rejection) => (
        <p key={rejection.name} className="text-warn text-xs">
          <span className="font-mono">{rejection.name}</span> was not attached: {rejection.reason}.
        </p>
      ))}

      {waiting === null ? null : (
        <p className="text-warn text-xs" role="status">
          {waiting} Sending now would quote an identifier no tool can resolve.
        </p>
      )}

      <textarea
        value={prompt}
        aria-label="Message"
        onChange={(event) => onPrompt(event.target.value)}
        onPaste={(event) => {
          // A screenshot in the clipboard is the fastest way anybody attaches
          // an image, and it never touches the disk to get here.
          const files = [...event.clipboardData.files]
          if (files.length > 0) {
            event.preventDefault()
            onAttach(files)
          }
        }}
        onDragOver={(event) => {
          // Without this the browser navigates to the file, which loses the
          // conversation and the tab along with it.
          event.preventDefault()
          setOver(true)
        }}
        onDragLeave={() => setOver(false)}
        onDrop={(event) => {
          event.preventDefault()
          setOver(false)
          onAttach([...event.dataTransfer.files])
        }}
        onKeyDown={(event) => {
          // Enter sends, Shift+Enter starts a line — which is what everybody's
          // fingers already do. The modifier still sends, for the pasted system
          // prompt that arrives with its own newlines.
          if (event.key !== 'Enter' || cannotSend) {
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
            ? 'Ask something, or drop a file in. Enter sends, Shift+Enter starts a line.'
            : 'Ask something else, or retry the last turn above.'
        }
        className={`${INPUT_CLASSES} w-full resize-y ${over ? 'border-brand ring-2 ring-line-strong' : ''}`}
      />

      <div className="flex flex-wrap items-center gap-2">
        {/*
          One button, because there was only ever one thing to do with what is in
          the box. How it goes out is a setting, and a setting is a checkbox.
        */}
        <Button
          variant="primary"
          size="md"
          disabled={cannotSend}
          onClick={onSend}
          title={
            streaming
              ? 'One turn, read chunk by chunk. Tool calls do not reassemble in a stream, so this does not loop.'
              : 'Run the profile in a loop, answering its tools. A profile with none stops on turn one.'
          }
        >
          Send
        </Button>
        {/*
          A label rather than a button, because the control it drives is the
          file input next to it — which stays in the page, hidden, so a
          keyboard reaches it and the picker opens where the browser wants.
        */}
        <label
          htmlFor={picker}
          title={
            uploadTo === null
              ? `Drop, paste or pick files. Up to ${humanSize(MAX_FILE_BYTES)} each — they travel in the body of this request and of every one after it.`
              : `Drop, paste or pick files. Up to ${humanSize(MAX_FILE_BYTES)} inline, or ${humanSize(MAX_UPLOAD_BYTES)} uploaded to \`${uploadTo}\` — an upload sends the identifier, not the bytes.`
          }
          className="cursor-pointer rounded-lg border border-line-strong px-4 py-1.5 text-sm transition-colors hover:bg-well"
        >
          Attach
        </label>
        <input
          id={picker}
          type="file"
          multiple
          className="sr-only"
          disabled={busy}
          onChange={(event) => {
            onAttach([...(event.target.files ?? [])])
            // Cleared so picking the same file twice in a row still fires a
            // change event, which is otherwise a silently ignored click.
            event.target.value = ''
          }}
        />
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

/**
 * One attached file, and the shape it will go out as.
 *
 * The shape is a dropdown rather than a decision `mire` makes quietly, because
 * it is the decision: the same PDF is a `file` part to one endpoint, a wall of
 * text to another, and nothing at all to a third. A guess from the media type
 * is what it starts on — which endpoint accepts which is the sort of thing this
 * tool exists to find out, and it cannot be found out if the answer is fixed.
 *
 * *As an upload* is the one shape whose state is worth showing on the chip: it
 * is the only one where something happens between attaching a file and sending
 * the turn, and where the thing that happened can fail on its own.
 */
function Chip({
  attachment,
  busy,
  canUpload,
  onShape,
  onDetach,
}: {
  attachment: Attachment
  busy: boolean
  /** Whether this profile has a server with somewhere to put a file. */
  canUpload: boolean
  onShape: (id: string, shape: Shape) => void
  onDetach: (id: string) => void
}) {
  return (
    <li className="flex items-center gap-2 rounded-lg border border-line-strong bg-well py-1 pr-1 pl-2 text-xs">
      <span className="max-w-40 truncate font-mono" title={attachment.name}>
        {attachment.name}
      </span>
      <span className="text-muted">{humanSize(attachment.size)}</span>
      <select
        value={attachment.shape}
        disabled={busy}
        aria-label={`How ${attachment.name} is sent`}
        onChange={(event) => onShape(attachment.id, event.target.value as Shape)}
        className={`${INPUT_CLASSES} px-1 py-0.5 text-xs`}
      >
        {shapesFor(attachment, canUpload).map((shape) => (
          <option key={shape} value={shape}>
            {SHAPE_LABELS[shape]}
          </option>
        ))}
      </select>
      {attachment.shape === 'upload' ? <UploadState state={attachment.upload} /> : null}
      <button
        type="button"
        disabled={busy}
        onClick={() => onDetach(attachment.id)}
        aria-label={`Remove ${attachment.name}`}
        className="rounded px-1 text-faint disabled:opacity-50 hover:text-ink"
      >
        ×
      </button>
    </li>
  )
}

/**
 * Where an uploaded file got to.
 *
 * The identifier is shown rather than hidden behind a tick, because it is what
 * the turn is about to say and what a tool call will quote — and reading it here
 * is how you tell a run that used the file from one that invented an id.
 */
function UploadState({ state }: { state: Attachment['upload'] }) {
  if (state === undefined || state.status === 'uploading') {
    return (
      <span className="text-muted" role="status">
        uploading…
      </span>
    )
  }
  if (state.status === 'failed') {
    return (
      <span className="text-bad" title={state.message}>
        upload failed
      </span>
    )
  }
  return (
    <span className="font-mono text-muted" title={`Uploaded to ${state.server}`}>
      {state.fileId}
    </span>
  )
}
