import { type ReactNode, useMemo, useState } from 'react'
import type { DecodeTrace, StreamView, ToolInvocation } from '../api'
import {
  type Exchange,
  type ModelExchange,
  type ProtocolExchange,
  statusTone,
  type ToolExchange,
} from '../conversation'
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
export function TrafficPanel({
  exchanges,
  expectUnauthorized,
  onClear,
}: {
  exchanges: Exchange[]
  expectUnauthorized: boolean
  onClear: () => void
}) {
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set())
  const allClosed = exchanges.length > 0 && exchanges.every((exchange) => closed.has(exchange.id))

  const toggle = (id: string) =>
    setClosed((current) => {
      const next = new Set(current)
      if (!next.delete(id)) {
        next.add(id)
      }
      return next
    })

  return (
    <Panel
      title="Traffic"
      actions={
        <div className="flex items-center gap-2">
          <span className="text-faint text-xs">
            {exchanges.length} {exchanges.length === 1 ? 'exchange' : 'exchanges'}
          </span>
          {exchanges.length === 0 ? null : (
            <>
              <Button
                onClick={() =>
                  setClosed(allClosed ? new Set() : new Set(exchanges.map((one) => one.id)))
                }
              >
                {allClosed ? 'Expand all' : 'Collapse all'}
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
        <ol className="space-y-2">
          {exchanges.map((exchange) =>
            exchange.kind === 'model' ? (
              <ModelCard
                key={exchange.id}
                exchange={exchange}
                expectUnauthorized={expectUnauthorized}
                open={!closed.has(exchange.id)}
                onToggle={() => toggle(exchange.id)}
              />
            ) : exchange.kind === 'protocol' ? (
              <ProtocolCard
                key={exchange.id}
                exchange={exchange}
                open={!closed.has(exchange.id)}
                onToggle={() => toggle(exchange.id)}
              />
            ) : (
              <ToolCard
                key={exchange.id}
                exchange={exchange}
                open={!closed.has(exchange.id)}
                onToggle={() => toggle(exchange.id)}
              />
            ),
          )}
        </ol>
      )}
    </Panel>
  )
}

/** The frame every exchange shares: a summary line you can fold away. */
function Card({
  label,
  summary,
  badges,
  open,
  onToggle,
  children,
}: {
  label: string
  summary: string
  badges: ReactNode
  open: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <li className="rounded border border-line">
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
  onToggle,
}: {
  exchange: ModelExchange
  expectUnauthorized: boolean
  open: boolean
  onToggle: () => void
}) {
  const { outcome } = exchange
  const { http, decoded, decode } = outcome.response
  const tone = statusTone(http.status, expectUnauthorized)
  const protectedAsExpected = expectUnauthorized && (http.status === 401 || http.status === 403)

  return (
    <Card
      label={`${turnLabel(exchange.turn, 'Call')} · model`}
      summary={outcome.request.url}
      open={open}
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

        {decoded?.kind === 'completion' ? (
          <div className="space-y-2">
            {decoded.content === null ? (
              <p className="text-muted text-sm">
                No configured path resolved the content. The raw response is below, and the decode
                trace says what was tried.
              </p>
            ) : (
              <p className="whitespace-pre-wrap text-sm">{decoded.content}</p>
            )}

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
  onToggle,
}: {
  exchange: ProtocolExchange
  open: boolean
  onToggle: () => void
}) {
  const mcp = exchange.exchange
  // Not `statusTone`: nothing is ever expected to be refused here. A protocol
  // request that failed failed, whoever the run was calling as.
  const tone = mcp.error ? 'bad' : mcp.status >= 400 ? 'bad' : mcp.status === 0 ? 'bad' : 'good'

  return (
    <Card
      label={`${turnLabel(exchange.turn, 'Setup')} · ${mcp.method}`}
      summary={`${mcp.server} · ${mcp.url}`}
      open={open}
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
 * One tool invocation, in the same three parts.
 *
 * The arguments the model produced are the request, the schema check is the
 * decode — the only reading anyone does of them — and what the tool handed back
 * is the response. Whether it really ran is the first thing the card says,
 * because a simulated result that looks plausible is the easiest way to believe
 * an integration works when nothing has been wired up.
 */
function ToolCard({
  exchange,
  open,
  onToggle,
}: {
  exchange: ToolExchange
  open: boolean
  onToggle: () => void
}) {
  const tool: ToolInvocation = exchange.invocation

  return (
    <Card
      label={`${turnLabel(exchange.turn, 'Call')} · ${tool.call.name}`}
      summary={tool.source === 'mcp' ? (tool.server ?? 'mcp') : 'simulated'}
      open={open}
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
