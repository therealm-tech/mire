import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import type { DecodeTrace, StreamView, ToolInvocation } from '../api'
import {
  type Exchange,
  failed,
  type HookExchange,
  type ModelExchange,
  type ProtocolExchange,
  statusTone,
  type ToolExchange,
} from '../conversation'
import { formatBytes } from './ChatPanel'
import { JsonTree } from './JsonTree'
import { Badge, Button, Code, CopyButton, Panel } from './primitives'

/**
 * Everything that left this process, in the order it left.
 *
 * The conversation above is the readable half; this is the half you came for.
 * Model calls and tool calls sit in the same list because they are the same
 * question asked twice — what went out, what was made of what came back, and
 * what actually came back — and a run is only explicable when you can read both
 * against each other in order.
 *
 * It accumulates across the whole conversation rather than resetting per send:
 * "it worked on turn one and not on turn four" is a comparison, and a panel that
 * only ever shows the latest turn cannot make one.
 */
/** Which half of the traffic you are reading. */
type Lens = 'all' | 'model' | 'tool' | 'protocol' | 'hook'

const LENSES: { key: Lens; label: string }[] = [
  { key: 'all', label: 'All' },
  { key: 'model', label: 'Model' },
  { key: 'tool', label: 'Tools' },
  { key: 'protocol', label: 'Protocol' },
  { key: 'hook', label: 'Hooks' },
]

export function TrafficPanel({
  exchanges,
  expectUnauthorized,
  reveal,
  onRevealed,
  onExport,
  onClear,
}: {
  exchanges: Exchange[]
  expectUnauthorized: boolean
  /** An exchange the conversation above is pointing at, or `null`. */
  reveal: string | null
  onRevealed: () => void
  onExport: () => void
  onClear: () => void
}) {
  // Which cards the reader has opened. Folded is the default, and the set
  // tracks the exceptions rather than the rule: a run puts a wall of headers,
  // bodies and decode traces on the page, and the list is a table of contents
  // before it is a transcript. You open what you came for.
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set())
  const [lens, setLens] = useState<Lens>('all')
  const [onlyFailures, setOnlyFailures] = useState(false)
  // The card just jumped to, marked for a moment so the eye can find where it
  // landed. A scroll on its own moves the page and says nothing about why.
  const [flash, setFlash] = useState<string | null>(null)

  const failures = useMemo(
    () => new Set(exchanges.filter((one) => failed(one, expectUnauthorized)).map((one) => one.id)),
    [exchanges, expectUnauthorized],
  )

  const shown = exchanges.filter(
    (exchange) =>
      (lens === 'all' || exchange.kind === lens) && (!onlyFailures || failures.has(exchange.id)),
  )

  const allOpen = shown.length > 0 && shown.every((exchange) => open.has(exchange.id))

  const toggle = (id: string) =>
    setOpen((current) => {
      const next = new Set(current)
      if (!next.delete(id)) {
        next.add(id)
      }
      return next
    })

  const fading = useRef<ReturnType<typeof setTimeout> | null>(null)

  /**
   * Bring the card the conversation is pointing at into view.
   *
   * The filters are dropped first, and deliberately: the alternative is a click
   * that appears to do nothing because the card it meant is behind a filter the
   * reader set four minutes ago and has stopped thinking about.
   *
   * Nothing is cancelled on the way out, which is not an oversight: the pointer
   * is cleared as soon as it has been read, so a cleanup here would fire on the
   * very next render — cancelling the scroll it had just scheduled and leaving
   * the card marked for good. The one timer that outlives a reveal is replaced
   * by the next one.
   */
  useEffect(() => {
    if (reveal === null) {
      return
    }
    setLens('all')
    setOnlyFailures(false)
    setOpen((current) => new Set(current).add(reveal))
    setFlash(reveal)
    onRevealed()

    // Deferred: the card may have been behind a filter a moment ago, and an
    // element React has not committed yet cannot be scrolled to.
    setTimeout(() => {
      document.getElementById(`exchange-${reveal}`)?.scrollIntoView({ block: 'center' })
    }, 0)

    if (fading.current) {
      clearTimeout(fading.current)
    }
    fading.current = setTimeout(() => setFlash(null), 1600)
  }, [reveal, onRevealed])

  return (
    <Panel
      title="Traffic"
      actions={
        <div className="flex items-center gap-2">
          <span className="text-faint text-xs">
            {shown.length === exchanges.length
              ? `${exchanges.length} ${exchanges.length === 1 ? 'exchange' : 'exchanges'}`
              : `${shown.length} of ${exchanges.length}`}
          </span>
          {exchanges.length === 0 ? null : (
            <>
              <Button
                onClick={() => setOpen(allOpen ? new Set() : new Set(shown.map((one) => one.id)))}
              >
                {allOpen ? 'Collapse all' : 'Expand all'}
              </Button>
              {/*
                Next to the list it exports, and only once there is something to
                export. What *Copy as curl* does for one request, this does for
                the run: the order, the turns, and what the decoder made of each
                answer — the half a single reproduced call loses.
              */}
              <Button onClick={onExport} title="Every exchange above, as a JSON file">
                Export
              </Button>
              <Button onClick={onClear}>Clear</Button>
            </>
          )}
        </div>
      }
    >
      {exchanges.length === 0 ? (
        <p className="text-muted text-sm">
          Nothing on the wire yet. Every model call and every tool invocation lands here, with the
          request that went out, what the decoder made of the answer, and the answer itself.
        </p>
      ) : (
        <>
          {/*
            A run puts five cards on the page and a session puts fifty, so the
            list needs a way to be asked a narrower question than "what
            happened". Failures first among them: it is the question this tool
            exists to answer, and scrolling for a red badge is not an answer.
          */}
          <div className="mb-2 flex flex-wrap items-center gap-1.5 border-line border-b pb-2">
            {LENSES.map((entry) => (
              <Button
                key={entry.key}
                aria-pressed={lens === entry.key}
                onClick={() => setLens(entry.key)}
                className={lens === entry.key ? 'bg-well font-medium' : 'text-muted'}
              >
                {entry.label}
              </Button>
            ))}
            <Button
              aria-pressed={onlyFailures}
              disabled={failures.size === 0}
              onClick={() => setOnlyFailures((current) => !current)}
              className={`ml-auto ${onlyFailures ? 'bg-bad-soft font-medium text-bad' : 'text-muted'}`}
              title="A bad status, a stream that stopped without ending, a handshake that never landed, or a tool that failed its schema"
            >
              {failures.size === 0
                ? 'Nothing failed'
                : `${failures.size} failed${onlyFailures ? '' : ' — show'}`}
            </Button>
          </div>

          {shown.length === 0 ? (
            <p className="text-muted text-sm">
              Nothing under this filter. {exchanges.length} exchanges are hidden by it.
            </p>
          ) : null}

          <ol className="space-y-2">
            {shown.map((exchange) =>
              exchange.kind === 'model' ? (
                <ModelCard
                  key={exchange.id}
                  exchange={exchange}
                  expectUnauthorized={expectUnauthorized}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : exchange.kind === 'protocol' ? (
                <ProtocolCard
                  key={exchange.id}
                  exchange={exchange}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : exchange.kind === 'hook' ? (
                <HookCard
                  key={exchange.id}
                  exchange={exchange}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : (
                <ToolCard
                  key={exchange.id}
                  exchange={exchange}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ),
            )}
          </ol>
        </>
      )}
    </Panel>
  )
}

/** The frame every exchange shares: a summary line you can fold away. */
function Card({
  id,
  label,
  summary,
  badges,
  open,
  flash,
  onToggle,
  children,
}: {
  /** The exchange's own id, so the conversation above can address this card. */
  id: string
  label: string
  summary: string
  badges: ReactNode
  open: boolean
  /** Just jumped to. */
  flash: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <li
      id={`exchange-${id}`}
      className={`rounded border transition-colors duration-500 ${
        flash ? 'border-brand bg-well' : 'border-line'
      }`}
    >
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex w-full flex-wrap items-baseline gap-2 rounded px-2 py-1.5 text-left transition-colors hover:bg-well"
      >
        <span className="font-medium text-sm">
          {open ? '▾' : '▸'} {label}
        </span>
        {badges}
        <span className="min-w-0 flex-1 truncate text-right font-mono text-[11px] text-faint">
          {summary}
        </span>
      </button>

      {open ? (
        // `min-w-0`, here and on every ancestor down from the page grid: a flex or
        // grid child is `min-width: auto` by default, so a wide body pushes the
        // column out and the *page* grows a horizontal scrollbar instead of the
        // block that is actually too wide. `overflow-x-auto` alone cannot fix
        // that — it only works once the box is allowed to be narrower than what
        // it contains.
        <div className="min-w-0 space-y-3 border-line border-t p-2">{children}</div>
      ) : null}
    </li>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="min-w-0 space-y-1">
      <h3 className="font-semibold text-faint text-xs uppercase tracking-wide">{title}</h3>
      {children}
    </section>
  )
}

/** Which turn of which run this was, for a list that spans several. */
function turnLabel(turn: number | null, fallback: string): string {
  return turn === null ? fallback : `Turn ${turn}`
}

/**
 * A body, as something you can actually work through.
 *
 * Every body gets the same treatment the raw response always had: a tree you can
 * fold, because finding where an endpoint hid a field is the job, and a wall of
 * text is the one shape that does not help with it. What went on the wire is one
 * line and stays one line in *Copy as curl* — that button reproduces the call,
 * this panel explains it.
 *
 * Anything that is not JSON is shown as itself rather than guessed at: an HTML
 * error page from a gateway is a finding, and mangling it would hide the finding.
 */
function Body({ text }: { text: string }) {
  const parsed = useMemo(() => {
    try {
      // `undefined` is unambiguous as "not JSON": `JSON.parse` never returns it,
      // and `null` is a perfectly good body.
      return JSON.parse(text) as unknown
    } catch {
      return undefined
    }
  }, [text])

  if (parsed === undefined) {
    return <Code>{text}</Code>
  }
  return <Tree value={parsed} />
}

/** A parsed value, already JSON. */
function Tree({ value }: { value: unknown }) {
  return (
    <div className="overflow-x-auto font-mono text-xs">
      <JsonTree value={value} />
    </div>
  )
}

/**
 * One call to a model endpoint.
 *
 * Request, decode, response — in that order, because that is the order in which
 * a wrong answer is diagnosed: was the right thing asked, was the answer read
 * correctly, and only then, is the endpoint wrong.
 */
function ModelCard({
  exchange,
  expectUnauthorized,
  open,
  flash,
  onToggle,
}: {
  exchange: ModelExchange
  expectUnauthorized: boolean
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const { outcome } = exchange
  const { http, decoded, decode, error } = outcome.response
  const tone = statusTone(http.status, expectUnauthorized)
  const protectedAsExpected = expectUnauthorized && (http.status === 401 || http.status === 403)

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Call')} · model`}
      summary={outcome.request.url}
      open={open}
      flash={flash}
      onToggle={onToggle}
      badges={
        <>
          <Badge tone={tone}>{http.status}</Badge>
          <span className="text-faint text-xs">{http.latencyMs} ms</span>
          {http.ttftMs === undefined ? null : (
            <Badge tone="neutral">first token {http.ttftMs} ms</Badge>
          )}
          {outcome.retriedAfterUnauthorized ? (
            <Badge tone="warn">credential refreshed, replayed once</Badge>
          ) : null}
          {/*
            Only worth a badge when the status did not already say so: a `400`
            with an error in it is not news, a `200` with one very much is.
          */}
          {error && http.status < 400 ? <Badge tone="bad">error in the body</Badge> : null}
        </>
      }
    >
      <Section title="Request">
        <div className="flex items-start justify-between gap-2">
          <p className="break-all font-mono text-xs">
            <span className="font-semibold">{outcome.request.method}</span> {outcome.request.url}
          </p>
          <CopyButton text={outcome.curl} label="Copy as curl" />
        </div>
        <ul className="space-y-0.5 break-all font-mono text-[11px] text-muted">
          {Object.entries(outcome.request.headers).map(([name, value]) => (
            <li key={name}>
              {name}: {value}
            </li>
          ))}
        </ul>
        <Body text={outcome.request.body} />
      </Section>

      <Section title="Decode">
        <DecodeTraceView trace={decode} />
      </Section>

      <Section title="Response">
        {protectedAsExpected ? (
          <p className="text-good text-sm">
            The route is protected — that is a pass, not a failure.
          </p>
        ) : null}

        {/*
          First, above the decoded answer: when the endpoint refused, its own
          sentence is the answer, and reading it should not mean unfolding the
          raw body underneath.
        */}
        {error ? (
          <div className="space-y-1 rounded bg-bad-soft p-2">
            <p className="text-bad text-sm">
              {error.message ?? 'The endpoint reported an error without saying what.'}
            </p>
            <div className="flex flex-wrap gap-2">
              {error.type ? <Badge tone="bad">{error.type}</Badge> : null}
              {error.code ? <Badge tone="bad">code {error.code}</Badge> : null}
            </div>
          </div>
        ) : null}

        {decoded?.kind === 'completion' ? (
          <div className="space-y-2">
            {decoded.content === null ? null : (
              <p className="whitespace-pre-wrap text-sm">{decoded.content}</p>
            )}

            {/*
              Not said when the endpoint reported an error: there is nothing
              wrong with the profile, and sending the reader off to fix its
              paths would be sending them the wrong way.
            */}
            {decoded.content === null && !error ? (
              <p className="text-muted text-sm">
                No configured path resolved the content. The raw response is below, and the decode
                trace says what was tried.
              </p>
            ) : null}

            <div className="flex flex-wrap gap-2 text-xs">
              {decoded.finishReason ? <Badge>finish: {decoded.finishReason}</Badge> : null}
              {decoded.usage?.totalTokens ? (
                <Badge>{decoded.usage.totalTokens} tokens</Badge>
              ) : null}
            </div>

            {decoded.toolCalls.map((toolCall) => (
              <Code key={`${toolCall.id ?? ''}${toolCall.name}`}>
                {`${toolCall.name}(${JSON.stringify(toolCall.arguments)})`}
              </Code>
            ))}
          </div>
        ) : null}

        {decoded?.kind === 'embedding' ? (
          // The vectors themselves are analysed above; here the point is only
          // that the decoder found some, and how many.
          <p className="text-sm">
            Decoded {decoded.count} {decoded.count === 1 ? 'vector' : 'vectors'}, encoded as{' '}
            {decoded.encoding}.
          </p>
        ) : null}

        {outcome.response.jsonError ? (
          <div className="space-y-1">
            <Badge tone="warn">not JSON</Badge>
            <p className="text-muted text-xs">{outcome.response.jsonError}</p>
          </div>
        ) : null}

        {outcome.response.stream ? <StreamStats stream={outcome.response.stream} /> : null}

        {/*
          One section, not two. The server sends `bodyText` and `raw` for the
          same bytes, and only ever withholds the first — for an embedding whose
          vectors were elided, where `raw` is all there is. Showing both meant
          reading the same JSON twice under two headings.
        */}
        <details open={http.status >= 400}>
          <summary className="cursor-pointer text-faint text-xs hover:text-ink">
            Body received
            {outcome.response.elided ? ' (vectors elided)' : ''}
          </summary>
          {outcome.response.bodyText === undefined ? (
            <Tree value={outcome.response.raw} />
          ) : (
            <div className="space-y-1">
              <Body text={outcome.response.bodyText} />
              <CopyButton text={outcome.response.bodyText} label="Copy body" />
            </div>
          )}
        </details>
      </Section>
    </Card>
  )
}

/**
 * One JSON-RPC round trip: the protocol underneath the tools.
 *
 * `initialize` and `tools/list` never appear in a tool listing because no tool
 * was called — and when a server refuses the run, they are the only thing that
 * happened. A card for them is the difference between "the model called nothing"
 * and "the handshake came back `401`".
 */
function ProtocolCard({
  exchange,
  open,
  flash,
  onToggle,
}: {
  exchange: ProtocolExchange
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const mcp = exchange.exchange
  // Not `statusTone`: nothing is ever expected to be refused here. A protocol
  // request that failed failed, whoever the run was calling as.
  const tone = mcp.error ? 'bad' : mcp.status >= 400 ? 'bad' : mcp.status === 0 ? 'bad' : 'good'

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Setup')} · ${mcp.method}`}
      summary={`${mcp.server} · ${mcp.url}`}
      open={open}
      flash={flash}
      onToggle={onToggle}
      badges={
        <>
          <Badge tone="neutral">mcp</Badge>
          {mcp.error ? (
            <Badge tone="bad">never answered</Badge>
          ) : (
            <Badge tone={tone}>{mcp.status}</Badge>
          )}
          <span className="text-faint text-xs">{mcp.latencyMs} ms</span>
          <Badge tone="neutral">{mcp.revision}</Badge>
          {mcp.notification ? <Badge tone="neutral">notification</Badge> : null}
        </>
      }
    >
      <Section title="Request">
        <p className="break-all font-mono text-xs">
          <span className="font-semibold">POST</span> {mcp.url}
        </p>
        <ul className="space-y-0.5 break-all font-mono text-[11px] text-muted">
          {Object.entries(mcp.headers).map(([name, value]) => (
            <li key={name}>
              {name}: {value}
            </li>
          ))}
        </ul>
        <Body text={mcp.request} />
      </Section>

      <Section title="Response">
        {mcp.error ? (
          <p className="flex flex-wrap items-baseline gap-2 text-xs">
            <Badge tone="bad">no answer</Badge>
            <span className="text-muted">{mcp.error}</span>
          </p>
        ) : null}

        {mcp.streaming ? (
          <p className="text-muted text-xs">
            Answered as an event stream; the last event carries the response.
          </p>
        ) : null}

        {mcp.notification && mcp.response.length === 0 ? (
          <p className="text-muted text-sm">
            Nothing, which is what a notification is entitled to answer.
          </p>
        ) : null}

        {mcp.response.length > 0 ? <Body text={mcp.response} /> : null}
      </Section>
    </Card>
  )
}

/**
 * One hook firing around a tool call.
 *
 * A card of its own rather than a line on the tool's, because it is traffic to a
 * third address. When a gate refuses a call, the tool card says the call did not
 * happen; only this says who decided that, what they were told, and what they
 * answered — which is the difference between a broken server and a policy doing
 * exactly its job.
 */
function HookCard({
  exchange,
  open,
  flash,
  onToggle,
}: {
  exchange: HookExchange
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const hook = exchange.record
  // Not `statusTone`: a hook is something you asked for, so nothing it answers
  // is ever the expected refusal an anonymous call is looking for.
  const tone = hook.error || hook.status === 0 || hook.status >= 400 ? 'bad' : 'good'

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Hook')} · ${hook.hook} (${hook.phase})`}
      summary={`${hook.server} · ${hook.tool} · ${hook.url}`}
      open={open}
      flash={flash}
      onToggle={onToggle}
      badges={
        <>
          <Badge tone="neutral">hook</Badge>
          <Badge tone="neutral">{hook.phase}</Badge>
          {hook.status === 0 ? (
            <Badge tone="bad">never answered</Badge>
          ) : (
            <Badge tone={tone}>{hook.status}</Badge>
          )}
          <span className="text-faint text-xs">{hook.latencyMs} ms</span>
          {hook.stoppedTheCall ? <Badge tone="bad">stopped the call</Badge> : null}
        </>
      }
    >
      <Section title="Request">
        <p className="break-all font-mono text-xs">
          <span className="font-semibold">{hook.method}</span> {hook.url}
        </p>
        <ul className="space-y-0.5 break-all font-mono text-[11px] text-muted">
          {Object.entries(hook.headers).map(([name, value]) => (
            <li key={name}>
              {name}: {value}
            </li>
          ))}
        </ul>
        {hook.request.length > 0 ? <Body text={hook.request} /> : null}

        {hook.files.length > 0 ? (
          <ul className="space-y-0.5 text-xs">
            {hook.files.map((file) => (
              <li key={file.id} className="flex flex-wrap items-baseline gap-2">
                <Badge tone="neutral">file</Badge>
                <span className="font-mono">{file.name}</span>
                <span className="text-faint">
                  {file.contentType} · {formatBytes(file.size)}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </Section>

      <Section title="Response">
        {hook.error ? (
          <p className="flex flex-wrap items-baseline gap-2 text-xs">
            <Badge tone="bad">{hook.stoppedTheCall ? 'refused' : 'failed, stepped over'}</Badge>
            <span className="text-muted">{hook.error}</span>
          </p>
        ) : null}

        {hook.response.length > 0 ? (
          <Body text={hook.response} />
        ) : (
          <p className="text-muted text-sm">Nothing, which a hook is entitled to answer.</p>
        )}
      </Section>
    </Card>
  )
}

/**
 * One tool invocation, in the same three parts — four, when it captured.
 *
 * The arguments the model produced are the request, the schema check is the
 * decode — the only reading anyone does of them — and what the tool handed back
 * is the response. Whether it really ran is the first thing the card says,
 * because a simulated result that looks plausible is the easiest way to believe
 * an integration works when nothing has been wired up.
 *
 * **Captured** is the fourth part, and only shows up when `agent.capture` pulled
 * something out of that answer. It is the one part that is not a wire: it is
 * what a later hook's URL, header or body will render, and reading it here is
 * the difference between a rendered address you can explain and one you cannot.
 * A rule that matched nothing captures nothing, so the section is simply absent
 * — which is the same answer, and the log says which path was tried.
 */
function ToolCard({
  exchange,
  open,
  flash,
  onToggle,
}: {
  exchange: ToolExchange
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const tool: ToolInvocation = exchange.invocation
  const captured = Object.entries(tool.captured)

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Call')} · ${tool.call.name}`}
      summary={tool.source === 'mcp' ? (tool.server ?? 'mcp') : 'simulated'}
      open={open}
      flash={flash}
      onToggle={onToggle}
      badges={
        <>
          {tool.source === 'mcp' ? (
            <Badge tone="warn">called for real</Badge>
          ) : (
            <Badge tone="neutral">simulated, nothing executed</Badge>
          )}
          {tool.latencyMs === undefined ? null : (
            <span className="text-faint text-xs">{tool.latencyMs} ms</span>
          )}
          {tool.error ? <Badge tone="bad">tool failed</Badge> : null}
        </>
      }
    >
      <Section title="Request">
        <p className="font-mono text-xs">
          {tool.call.name}
          {tool.call.id ? <span className="text-faint"> · id {tool.call.id}</span> : null}
        </p>
        <Tree value={tool.call.arguments} />
      </Section>

      <Section title="Decode">
        {tool.schemaErrors.length > 0 ? (
          <ul className="space-y-0.5">
            {tool.schemaErrors.map((error) => (
              <li key={error} className="flex flex-wrap items-baseline gap-2 text-xs">
                <Badge tone="warn">schema</Badge>
                <span className="text-muted">{error}</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-xs">
            <Badge tone="good">arguments match the schema</Badge>
          </p>
        )}
      </Section>

      <Section title="Response">
        {tool.error ? (
          <p className="flex flex-wrap items-baseline gap-2 text-xs">
            <Badge tone="bad">tool failed</Badge>
            <span className="text-muted">{tool.error}</span>
          </p>
        ) : null}

        {tool.reportedError ? (
          <p className="flex flex-wrap items-baseline gap-2 text-xs">
            <Badge tone="warn">the tool reported a problem</Badge>
            <span className="text-muted">which the model is meant to react to</span>
          </p>
        ) : null}

        {/* Often not JSON at all — a tool is entitled to answer prose. */}
        <Body text={tool.result} />
      </Section>

      {captured.length > 0 ? (
        <Section title="Captured">
          {/* Name, then value, laid out the way the tree lays out a field —
              because that is what it is: a field of the answer above, under the
              name a template will call it by. */}
          <ul className="overflow-x-auto font-mono text-xs">
            {captured.map(([name, value]) => (
              <li key={name} className="py-px">
                <span className="text-muted">{name}</span>
                <span className="text-faint">: </span>
                <JsonTree value={value} />
              </li>
            ))}
          </ul>
        </Section>
      ) : null}
    </Card>
  )
}

/**
 * What the stream did, rather than what it said.
 *
 * The counters are the half a non-streaming call cannot answer: whether chunks
 * really arrived separately, and whether the endpoint ended the stream or it
 * merely stopped — which is what a proxy cutting a long generation looks like.
 */
function StreamStats({ stream }: { stream: StreamView }) {
  return (
    <div className="space-y-1">
      <div className="flex flex-wrap gap-2 text-xs">
        {stream.terminated ? (
          <Badge tone="good">ended cleanly</Badge>
        ) : (
          <Badge tone="warn">stopped without ending</Badge>
        )}
        {stream.framing ? <Badge tone="neutral">{stream.framing}</Badge> : null}
        <Badge>{stream.chunks} chunks</Badge>
        <Badge>{stream.deltas} with text</Badge>
        <Badge>{stream.bytes} bytes</Badge>
        {stream.firstChunkMs === undefined ? null : (
          <Badge>first chunk {stream.firstChunkMs} ms</Badge>
        )}
        {stream.unparsable > 0 ? <Badge tone="bad">{stream.unparsable} unreadable</Badge> : null}
      </div>
      {stream.terminated ? null : (
        <p className="text-muted text-xs">
          No end sentinel and no stop reason: the connection went quiet rather than finishing.
          Whatever arrived is above.
        </p>
      )}
    </div>
  )
}

/** Which configured path matched, which missed, and what was tried. */
function DecodeTraceView({ trace }: { trace: DecodeTrace }) {
  const matched = Object.entries(trace.matched)
  const missed = Object.entries(trace.missed)

  if (matched.length === 0 && missed.length === 0 && trace.issues.length === 0) {
    return (
      <p className="text-muted text-xs">
        Nothing was decoded — the profile declares no <span className="font-mono">decode:</span>{' '}
        block, or the call never got far enough to try.
      </p>
    )
  }

  return (
    <div className="space-y-1">
      <ul className="space-y-1 text-xs">
        {matched.map(([field, path]) => (
          <li key={field} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="good">matched</Badge>
            <span className="font-medium">{field}</span>
            <span className="font-mono text-muted">{path}</span>
          </li>
        ))}
        {missed.map(([field, paths]) => (
          <li key={field} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="warn">missed</Badge>
            <span className="font-medium">{field}</span>
            <span className="font-mono text-muted">{paths.join('  ·  ')}</span>
          </li>
        ))}
        {trace.issues.map((issue) => (
          <li key={`${issue.field}${issue.path}`} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="bad">wrong shape</Badge>
            <span className="font-medium">{issue.field}</span>
            <span className="font-mono text-muted">{issue.path}</span>
            <span className="text-muted">{issue.message}</span>
          </li>
        ))}
      </ul>
      {missed.length > 0 ? (
        <p className="text-muted text-xs">
          Pick the right path in the raw response, then add it to the profile's{' '}
          <span className="font-mono">decode:</span> block — it reloads without a restart.
        </p>
      ) : null}
    </div>
  )
}
