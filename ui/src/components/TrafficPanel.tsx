import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import type { HookRecord, PartView, StreamView, ToolInvocation } from '../api'
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
import { Badge, Button, Code, CopyButton, Panel, type Tone } from './primitives'

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
 *
 * The list is a **grid**, not a stack of paragraphs. Every row puts its status,
 * its weight and its duration in the same column as every other row, so a list
 * of fifty is read by running down one column rather than by reading fifty
 * headlines — and a card that is opened is **request on the left, response on
 * the right**, in the same three strata on both sides, so comparing the two
 * halves is a sideways glance instead of a scroll.
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
  // tracks the exceptions rather than the rule: a run puts a wall of headers and
  // bodies on the page, and the list is a table of contents before it is a
  // transcript. You open what you came for.
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

  // What the duration bars are drawn against. The slowest call in view rather
  // than a fixed scale: the question a bar answers is "which of these did the
  // waiting", and that is a comparison within the list you are looking at.
  const slowest = Math.max(1, ...shown.map((exchange) => elapsed(exchange) ?? 0))

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

          <ol className="space-y-1.5">
            {shown.map((exchange) =>
              exchange.kind === 'model' ? (
                <ModelCard
                  key={exchange.id}
                  exchange={exchange}
                  expectUnauthorized={expectUnauthorized}
                  slowest={slowest}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : exchange.kind === 'protocol' ? (
                <ProtocolCard
                  key={exchange.id}
                  exchange={exchange}
                  slowest={slowest}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : exchange.kind === 'hook' ? (
                <HookCard
                  key={exchange.id}
                  exchange={exchange}
                  slowest={slowest}
                  open={open.has(exchange.id)}
                  flash={flash === exchange.id}
                  onToggle={() => toggle(exchange.id)}
                />
              ) : (
                <ToolCard
                  key={exchange.id}
                  exchange={exchange}
                  slowest={slowest}
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

/** How long this exchange took, when it took a measurable amount of time. */
function elapsed(exchange: Exchange): number | null {
  switch (exchange.kind) {
    case 'model':
      return exchange.outcome.response.http.latencyMs
    case 'protocol':
      return exchange.exchange.latencyMs
    case 'hook':
      return exchange.record.skipped === undefined ? exchange.record.latencyMs : null
    case 'tool':
      return exchange.invocation.latencyMs ?? null
  }
}

/**
 * The columns every row is laid out on, and the reason the list is scannable.
 *
 * Fixed tracks rather than content-sized ones, all the way across: a track that
 * sizes itself to what is in it lands in a different place on every row, and a
 * column that moves is a column you have to read instead of scan. Below `sm`
 * the four cells that carry context — where it went, which turn, how big, and
 * the bar — drop out, and the five that identify the call take the width.
 */
const COLUMNS =
  // `grid` and not only the tracks: without the display, the cells stack and one
  // row becomes six lines tall.
  'grid grid-cols-[0.75rem_2.5rem_minmax(0,1fr)_2.5rem_3.5rem]' +
  ' sm:grid-cols-[0.75rem_2.75rem_minmax(0,11rem)_minmax(0,1fr)_2.25rem_2.5rem_3.25rem_5rem]'

/** A cell that only earns its width once there is room for it. */
const CONTEXT = 'hidden sm:block'

/**
 * What the row says about how it went, in the one column reserved for it.
 *
 * `0` is never shown: nobody answered with it, and a column of numbers with a
 * zero in it reads as a status the endpoint chose. The row says so in the flags
 * instead, in words, and this column says there was no answer to put in it.
 */
interface Status {
  text: string
  tone: Tone
}

/** The status column for a call that never got an answer. */
const UNANSWERED: Status = { text: '—', tone: 'bad' }

/**
 * The frame every exchange shares: one line in the grid, and what unfolds under
 * it.
 */
function Card({
  id,
  label,
  kind,
  op,
  where,
  flags,
  turn,
  status,
  size,
  ms,
  slowest,
  open,
  flash,
  onToggle,
  children,
}: {
  /** The exchange's own id, so the conversation above can address this card. */
  id: string
  /**
   * The row as a sentence, for whoever is not looking at the grid.
   *
   * The cells are terse because a column is scanned rather than read, and eight
   * of them read aloud in order are the one thing a grid is worse at than the
   * paragraph it replaced. So the row is given the sentence outright — which
   * turn, what was asked, and how it went — and the columns are left to the eye.
   */
  label: string
  /** Which wire this was: `model`, `mcp`, `tool`, `hook`. */
  kind: string
  /** What was asked for — the path, the method, the tool's name. */
  op: string
  /** Who was asked, and at what address. */
  where: ReactNode
  /** The handful of things that are only worth saying when they are true. */
  flags?: ReactNode
  turn: string
  status: Status
  /** What came back, in bytes, when that is known. */
  size: number | null
  ms: number | null
  /** The slowest call in view, which is what the bar is drawn against. */
  slowest: number
  open: boolean
  /** Just jumped to. */
  flash: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <li
      id={`exchange-${id}`}
      className={`overflow-hidden rounded border transition-colors duration-500 ${
        flash ? 'border-brand bg-well' : 'border-line'
      }`}
    >
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        aria-label={`${label} — ${status.text}${ms === null ? '' : `, ${ms} ms`}`}
        className={`${COLUMNS} w-full items-center gap-x-2 px-2 py-1.5 text-left transition-colors hover:bg-well ${
          open ? 'bg-well' : ''
        }`}
      >
        <span className="text-[10px] text-faint">{open ? '▾' : '▸'}</span>
        <span className="rounded bg-flag-soft px-1 py-0.5 text-center font-mono text-[10px] text-muted uppercase tracking-wide">
          {kind}
        </span>
        <span className="truncate font-medium font-mono text-[12.5px]">{op}</span>
        <span className={`${CONTEXT} flex min-w-0 items-baseline gap-2`}>
          <span className="truncate font-mono text-[11px] text-faint">{where}</span>
          {flags}
        </span>
        <span className={`${CONTEXT} text-right font-mono text-[10.5px] text-faint`}>{turn}</span>
        <span
          className={`text-right font-mono text-[12.5px] font-medium ${TONE_TEXT[status.tone]}`}
        >
          {status.text}
        </span>
        <span className={`${CONTEXT} text-right font-mono text-[11px] text-faint`}>
          {size === null ? '' : formatBytes(size)}
        </span>
        <span className="flex items-center gap-1.5">
          {/*
            Drawn only when there is something to draw: a tool nothing sent and a
            hook that never fired have no duration, and a bar of width zero reads
            as an instant call rather than as no call at all.
          */}
          <span className={`${CONTEXT} h-1 min-w-0 flex-1 rounded-sm bg-flag-soft`}>
            {ms === null ? null : (
              <span
                className={`block h-1 rounded-sm ${status.tone === 'bad' ? 'bg-bad' : 'bg-faint'}`}
                style={{ width: `${Math.max(2, Math.round((ms / slowest) * 100))}%` }}
              />
            )}
          </span>
          <span className="w-10 text-right font-mono text-[10.5px] text-faint">
            {ms === null ? '—' : `${ms} ms`}
          </span>
        </span>
      </button>

      {open ? (
        // `min-w-0`, here and on every ancestor down from the page grid: a flex or
        // grid child is `min-width: auto` by default, so a wide body pushes the
        // column out and the *page* grows a horizontal scrollbar instead of the
        // block that is actually too wide. `overflow-x-auto` alone cannot fix
        // that — it only works once the box is allowed to be narrower than what
        // it contains.
        <div className="min-w-0 border-line border-t">{children}</div>
      ) : null}
    </li>
  )
}

const TONE_TEXT: Record<Tone, string> = {
  good: 'text-good',
  bad: 'text-bad',
  warn: 'text-warn',
  neutral: 'text-faint',
}

/** One of the few things worth saying on a folded row. */
function Flag({ tone = 'neutral', children }: { tone?: Tone; children: ReactNode }) {
  return (
    <span
      className={`shrink-0 rounded px-1.5 py-px font-medium text-[10px] ${
        tone === 'bad'
          ? 'bg-bad-soft text-bad'
          : tone === 'warn'
            ? 'bg-warn-soft text-warn'
            : 'bg-flag-soft text-muted'
      }`}
    >
      {children}
    </span>
  )
}

/**
 * The two halves of an exchange, side by side.
 *
 * Same three strata on both sides — start line, headers, body — because that is
 * what makes the comparison a glance: the thing you are checking is in the same
 * place on the left as on the right. They stack on a narrow screen, where side
 * by side would mean two columns too thin to hold a header value.
 */
function Panes({ children }: { children: ReactNode }) {
  return (
    <div className="grid min-w-0 divide-y divide-line md:grid-cols-2 md:divide-x md:divide-y-0">
      {children}
    </div>
  )
}

function Pane({
  title,
  action,
  children,
}: {
  title: string
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="min-w-0 space-y-2 p-2">
      <header className="flex items-center gap-2">
        <h3 className="font-semibold text-faint text-xs uppercase tracking-wide">{title}</h3>
        {action ? <span className="ml-auto">{action}</span> : null}
      </header>
      {children}
    </section>
  )
}

/** What went out, as the first line of a request really is. */
function RequestLine({ method, url }: { method: string; url: string }) {
  const { path, origin } = split(url)
  return (
    <p className="break-all font-mono text-xs">
      <span className="font-semibold">{method}</span> {path}
      {origin === null ? null : <span className="text-faint"> · {origin}</span>}
    </p>
  )
}

/** What came back, in the same place on the other side. */
function ResponseLine({
  status,
  tone,
  ms,
  note,
}: {
  status: number | string
  tone: Tone
  ms: number | null
  note?: string | undefined
}) {
  return (
    <p className="break-all font-mono text-xs">
      <span className={`font-semibold ${TONE_TEXT[tone]}`}>{status}</span>
      {ms === null ? null : <span className="text-faint"> · {ms} ms</span>}
      {note === undefined ? null : <span className="text-faint"> · {note}</span>}
    </p>
  )
}

/** The handful of numbers that are read before anything is unfolded. */
function Facts({ children }: { children: ReactNode }) {
  return <div className="flex flex-wrap gap-x-4 gap-y-0.5">{children}</div>
}

function Fact({ label, value }: { label: string; value: string | number | undefined }) {
  if (value === undefined || value === '') {
    return null
  }
  return (
    <span className="font-mono text-[11px] text-muted">
      <span className="text-faint">{label} </span>
      {value}
    </span>
  )
}

/**
 * The headers, folded, with enough on the fold to know whether to open it.
 *
 * A list of five `name: value` lines in the flow of the card is the shape that
 * pushed everything else off the screen — and it was only ever the request's,
 * which is the half that holds no surprises. Folded, with the count and the
 * first few names on the summary, they cost one line until somebody wants them.
 */
function Headers({ headers, empty }: { headers: Record<string, string>; empty?: string }) {
  const entries = Object.entries(headers)

  if (entries.length === 0) {
    return empty === undefined ? null : <p className="text-faint text-xs">{empty}</p>
  }

  const names = entries.slice(0, 3).map(([name]) => name)
  return (
    <details className="min-w-0">
      <summary className="cursor-pointer text-faint text-xs hover:text-ink">
        {entries.length} {entries.length === 1 ? 'header' : 'headers'}
        <span className="opacity-70">
          {' '}
          · {names.join(', ')}
          {entries.length > names.length ? `, +${entries.length - names.length}` : ''}
        </span>
      </summary>
      <table className="mt-1 w-full table-fixed">
        <tbody>
          {entries.map(([name, value]) => (
            <tr key={name}>
              <td className="w-2/5 break-all pr-2 align-top font-mono text-[11px] text-faint">
                {name}
              </td>
              <td className="break-all align-top font-mono text-[11px] text-muted">
                {value === MASK ? <span className="text-faint">{MASK}</span> : value}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  )
}

/** What `mire` replaces a credential with on its way out of the process. */
const MASK = '***'

/**
 * A body, in the readings there are of it.
 *
 * The same bytes are a tree, a piece of text, and — for a request — a command
 * you can paste. They were three stacked sections and a `<details>`, which is
 * three of them too many on screen at once: only one is ever being read.
 */
interface View {
  key: string
  label: string
  render: () => ReactNode
}

function Views({ views, initial }: { views: View[]; initial?: string | undefined }) {
  const [chosen, setChosen] = useState(initial ?? views[0]?.key)
  const current = views.find((view) => view.key === chosen) ?? views[0]

  if (current === undefined) {
    return null
  }

  return (
    <div className="min-w-0 space-y-1.5">
      {views.length > 1 ? (
        <div role="tablist" className="flex flex-wrap gap-1">
          {views.map((view) => (
            <button
              key={view.key}
              type="button"
              role="tab"
              aria-selected={view.key === current.key}
              onClick={() => setChosen(view.key)}
              className={`rounded px-2 py-0.5 text-[11px] transition-colors ${
                view.key === current.key
                  ? 'border border-line bg-well text-ink'
                  : 'border border-transparent text-faint hover:text-ink'
              }`}
            >
              {view.label}
            </button>
          ))}
        </div>
      ) : null}
      {current.render()}
    </div>
  )
}

/**
 * A body, as something you can actually work through.
 *
 * Anything that is not JSON is shown as itself rather than guessed at: an HTML
 * error page from a gateway is a finding, and mangling it would hide the
 * finding.
 */
function Body({ text }: { text: string }) {
  const parsed = useMemo(() => parse(text), [text])
  return parsed === undefined ? <Code>{text}</Code> : <Tree value={parsed} />
}

/** A parsed value, already JSON. */
function Tree({ value }: { value: unknown }) {
  return (
    <div className="overflow-x-auto font-mono text-[11.5px]">
      <JsonTree value={value} />
    </div>
  )
}

/**
 * `undefined` is unambiguous as "not JSON": `JSON.parse` never returns it, and
 * `null` is a perfectly good body.
 */
function parse(text: string): unknown {
  try {
    return JSON.parse(text) as unknown
  } catch {
    return undefined
  }
}

/**
 * A JSON-RPC document, with the envelope lifted off the payload.
 *
 * `jsonrpc`, `id` and `method` are three of the four fields of every MCP request
 * ever sent, and the tree opened on them while folding away the one field that
 * differs. On a line of their own they cost nothing and the tree opens on
 * `params` — or on `result`, which is the answer somebody came to read.
 */
function Envelope({ text }: { text: string }) {
  const parsed = useMemo(() => parse(text), [text])

  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return <Body text={text} />
  }
  const { jsonrpc, id, method, ...payload } = parsed as Record<string, unknown>
  if (jsonrpc === undefined) {
    return <Body text={text} />
  }

  return (
    <div className="min-w-0 space-y-1.5">
      <p className="flex flex-wrap gap-x-3 rounded bg-well px-2 py-1 font-mono text-[11px]">
        <span>
          <span className="text-faint">jsonrpc </span>
          {String(jsonrpc)}
        </span>
        {id === undefined ? null : (
          <span>
            <span className="text-faint">id </span>
            {String(id)}
          </span>
        )}
        {method === undefined ? null : (
          <span>
            <span className="text-faint">method </span>
            {String(method)}
          </span>
        )}
      </p>
      <Tree value={payload} />
    </div>
  )
}

/**
 * The parts of a `multipart:` request, in the order they went out.
 *
 * Order is shown because it is the model's and the wire's, not the alphabet's
 * — and a reader comparing this against a `curl` that worked wants the two lists
 * to line up.
 */
function FormParts({ parts }: { parts: PartView[] }) {
  return (
    <ul className="space-y-0.5 text-xs">
      {parts.map((part) => (
        // The field alone will not do: several files under one name is what an
        // upload handler reads, so a form can legitimately repeat it.
        <li
          key={`${part.field}·${part.uploadId ?? part.value ?? ''}`}
          className="flex flex-wrap items-baseline gap-2"
        >
          <Badge tone="neutral">{part.field}</Badge>
          {part.filename === undefined ? (
            <span className="break-all font-mono">{part.value}</span>
          ) : (
            <span className="font-mono">{part.filename}</span>
          )}
          <span className="text-faint">
            {part.contentType ?? 'form field'}
            {part.size === undefined ? null : ` · ${formatBytes(part.size)}`}
          </span>
        </li>
      ))}
    </ul>
  )
}

/** A full-width band under the two halves, for what belongs to neither. */
function Strip({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 border-line border-t px-2 py-1.5">
      <h3 className="font-semibold text-faint text-xs uppercase tracking-wide">{title}</h3>
      {children}
    </section>
  )
}

/** Which turn of which run this was, for a list that spans several. */
function turnLabel(turn: number | null, fallback: string): string {
  return turn === null ? fallback : `Turn ${turn}`
}

/** The same thing in the width a column can spare for it. */
function turnCell(turn: number | null, fallback: string): string {
  return turn === null ? fallback : `T${turn}`
}

/** The bytes a body actually weighed, rather than the characters it printed. */
function weigh(text: string): number {
  return new TextEncoder().encode(text).length
}

/**
 * The path and the origin of an address, for a start line that leads with the
 * path.
 *
 * Cut by hand rather than by `new URL`, which normalises: a hook's URL is a
 * template until it is resolved, and `URL` percent-encodes the braces of an
 * unresolved one into `%7B%7B%20vars.session%20%7D%7D` — turning the single most
 * important thing on the card into line noise. Anything that does not look like
 * an absolute URL is left exactly as it was written.
 */
function split(url: string): { path: string; origin: string | null } {
  const match = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)(.*)$/i.exec(url)
  if (match === null) {
    return { path: url, origin: null }
  }
  return {
    path: match[2] === undefined || match[2] === '' ? '/' : match[2],
    origin: match[1] ?? null,
  }
}

/**
 * An address without the scheme, for the column that has to carry the whole of
 * it.
 *
 * A hook's URL is a template until it is resolved, and the unresolved half is in
 * the *path* — so a row that kept only the host would drop the one part somebody
 * opening a hook card came to read.
 */
function terse(url: string): string {
  const { path, origin } = split(url)
  return origin === null ? path : `${origin}${path}`
}

/** A header, whatever case the endpoint chose to write it in. */
function header(headers: Record<string, string>, name: string): string | undefined {
  const found = Object.entries(headers).find(([key]) => key.toLowerCase() === name)
  return found?.[1]
}

/**
 * One call to a model endpoint.
 *
 * Request on the left, response on the right — the order a wrong answer is
 * diagnosed in: was the right thing asked, and is the endpoint wrong.
 */
function ModelCard({
  exchange,
  expectUnauthorized,
  slowest,
  open,
  flash,
  onToggle,
}: {
  exchange: ModelExchange
  expectUnauthorized: boolean
  slowest: number
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const { outcome } = exchange
  const { http, error, stream } = outcome.response
  const tone = statusTone(http.status, expectUnauthorized)
  const protectedAsExpected = expectUnauthorized && (http.status === 401 || http.status === 403)
  const { path, origin } = split(outcome.request.url)
  const sent =
    outcome.request.body.length > 0
      ? weigh(outcome.request.body)
      : outcome.request.parts.reduce((total, part) => total + (part.size ?? 0), 0)
  const received =
    stream?.bytes ??
    (outcome.response.bodyText === undefined ? null : weigh(outcome.response.bodyText))

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Call')} · model · ${path}`}
      kind="model"
      op={path}
      where={
        <>
          {outcome.model} <span className="opacity-70">· {origin ?? outcome.request.url}</span>
        </>
      }
      flags={
        <>
          {outcome.retriedAfterUnauthorized ? <Flag tone="warn">replayed</Flag> : null}
          {/*
            Only worth saying when the status did not already say it: a `400`
            with an error in it is not news, a `200` with one very much is.
          */}
          {error && http.status < 400 ? <Flag tone="bad">error in the body</Flag> : null}
          {stream && !stream.terminated ? <Flag tone="bad">cut off</Flag> : null}
          {/*
            On the folded row and not only in the pane: time to first token is
            the number a streaming call is read for, and having to open a card to
            learn it is how a slow endpoint and a slow model stay indistinguishable.
          */}
          {http.ttftMs === undefined ? null : <Flag>first token {http.ttftMs} ms</Flag>}
        </>
      }
      turn={turnCell(exchange.turn, 'call')}
      status={{ text: String(http.status), tone }}
      size={received}
      ms={http.latencyMs}
      slowest={slowest}
      open={open}
      flash={flash}
      onToggle={onToggle}
    >
      <Panes>
        <Pane title="Request" action={<CopyButton text={outcome.curl} label="Copy as curl" />}>
          <RequestLine method={outcome.request.method} url={outcome.request.url} />
          <Facts>
            <Fact label="content-type" value={header(outcome.request.headers, 'content-type')} />
            <Fact label="sent" value={sent === 0 ? undefined : formatBytes(sent)} />
            <Fact label="auth" value={outcome.auth} />
          </Facts>
          <Headers headers={outcome.request.headers} />
          {outcome.request.body.length > 0 ? (
            <Views
              views={[
                {
                  key: 'payload',
                  label: 'Payload',
                  render: () => <Body text={outcome.request.body} />,
                },
                { key: 'raw', label: 'Raw', render: () => <Code>{outcome.request.body}</Code> },
                { key: 'curl', label: 'curl', render: () => <Code>{outcome.curl}</Code> },
              ]}
            />
          ) : null}

          {/*
            A form, by the field each part went out under. The field is the half
            the endpoint actually reads — a form carrying the right file under the
            wrong name is refused exactly like one carrying no file at all.
          */}
          {outcome.request.parts.length > 0 ? <FormParts parts={outcome.request.parts} /> : null}
        </Pane>

        <Pane
          title="Response"
          action={
            outcome.response.bodyText === undefined ? undefined : (
              <CopyButton text={outcome.response.bodyText} label="Copy body" />
            )
          }
        >
          <ResponseLine
            status={http.status}
            tone={tone}
            ms={http.latencyMs}
            note={http.ttftMs === undefined ? undefined : `first token ${http.ttftMs} ms`}
          />
          <Facts>
            <Fact label="content-type" value={header(http.headers, 'content-type')} />
            <Fact label="received" value={received === null ? undefined : formatBytes(received)} />
          </Facts>
          {/*
            Already carried by every trace and never once shown. This is where a
            rate limit, a gateway's own identity and the request id an operator
            will ask for have been all along.
          */}
          <Headers
            headers={http.headers}
            empty="The endpoint returned no headers worth recording."
          />

          {protectedAsExpected ? (
            <p className="text-good text-sm">
              The route is protected — that is a pass, not a failure.
            </p>
          ) : null}

          {/*
            First, above everything: when the endpoint refused, its own sentence
            is the answer, and reading it should not mean opening a tab.
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

          <Views
            // A red status with the decoder's empty hands under it is the whole
            // problem this panel answers: when the endpoint refused, what it
            // said is the answer, and it is in the body.
            initial={http.status >= 400 ? 'payload' : 'decoded'}
            views={[
              {
                key: 'decoded',
                label: 'Decoded',
                render: () => <Decoded exchange={exchange} error={error !== undefined} />,
              },
              {
                key: 'payload',
                label: 'Payload',
                render: () =>
                  outcome.response.bodyText === undefined ? (
                    <Tree value={outcome.response.raw} />
                  ) : (
                    <Body text={outcome.response.bodyText} />
                  ),
              },
              ...(outcome.response.bodyText === undefined
                ? []
                : [
                    {
                      key: 'raw',
                      label: 'Raw',
                      render: () => <Code>{outcome.response.bodyText ?? ''}</Code>,
                    },
                  ]),
            ]}
          />

          {outcome.response.jsonError ? (
            <div className="space-y-1">
              <Badge tone="warn">not JSON</Badge>
              <p className="text-muted text-xs">{outcome.response.jsonError}</p>
            </div>
          ) : null}

          {outcome.response.elided ? (
            <p className="text-faint text-xs">
              The vectors are elided from the body above; they are analysed in full in the panel
              beside this one.
            </p>
          ) : null}

          {stream ? <StreamStats stream={stream} /> : null}
        </Pane>
      </Panes>
    </Card>
  )
}

/** What the decoder made of the answer, which is not the answer itself. */
function Decoded({ exchange, error }: { exchange: ModelExchange; error: boolean }) {
  const { decoded } = exchange.outcome.response

  if (decoded?.kind === 'embedding') {
    // The vectors themselves are analysed above; here the point is only that the
    // decoder found some, and how many.
    return (
      <p className="text-sm">
        Decoded {decoded.count} {decoded.count === 1 ? 'vector' : 'vectors'}, encoded as{' '}
        {decoded.encoding}.
      </p>
    )
  }

  if (decoded?.kind !== 'completion') {
    return <p className="text-muted text-sm">Nothing was decoded.</p>
  }

  return (
    <div className="space-y-2">
      {decoded.content === null ? null : (
        <p className="whitespace-pre-wrap text-sm">{decoded.content}</p>
      )}

      {/*
        Not said when the endpoint reported an error, nor when the model answered
        with a tool call and no prose: in neither case is there anything wrong
        with the paths, and sending the reader off to fix them would be sending
        them the wrong way.
      */}
      {decoded.content === null && !error && decoded.toolCalls.length === 0 ? (
        <p className="text-muted text-sm">
          No configured path resolved the content. The raw response is one tab over, and the decode
          trace below says what was tried.
        </p>
      ) : null}

      <div className="flex flex-wrap gap-2 text-xs">
        {decoded.finishReason ? <Badge>finish: {decoded.finishReason}</Badge> : null}
        {decoded.usage?.totalTokens ? <Badge>{decoded.usage.totalTokens} tokens</Badge> : null}
      </div>

      {decoded.toolCalls.map((toolCall) => (
        <Code key={`${toolCall.id ?? ''}${toolCall.name}`}>
          {`${toolCall.name}(${JSON.stringify(toolCall.arguments)})`}
        </Code>
      ))}
    </div>
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
  slowest,
  open,
  flash,
  onToggle,
}: {
  exchange: ProtocolExchange
  slowest: number
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const mcp = exchange.exchange
  // Not `statusTone`: nothing is ever expected to be refused here. A protocol
  // request that failed failed, whoever the run was calling as.
  const tone: Tone = mcp.error || mcp.status === 0 || mcp.status >= 400 ? 'bad' : 'good'
  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Setup')} · ${mcp.method}`}
      kind="mcp"
      op={mcp.method}
      where={
        <>
          {mcp.server} <span className="opacity-70">· {terse(mcp.url)}</span>
        </>
      }
      flags={
        <>
          {mcp.error || mcp.status === 0 ? <Flag tone="bad">never answered</Flag> : null}
          {mcp.notification ? <Flag>notification</Flag> : null}
        </>
      }
      turn={turnCell(exchange.turn, 'setup')}
      status={mcp.error || mcp.status === 0 ? UNANSWERED : { text: String(mcp.status), tone }}
      size={mcp.response.length === 0 ? null : weigh(mcp.response)}
      ms={mcp.latencyMs}
      slowest={slowest}
      open={open}
      flash={flash}
      onToggle={onToggle}
    >
      <Panes>
        <Pane title="Request">
          <RequestLine method="POST" url={mcp.url} />
          <Facts>
            <Fact label="sent" value={formatBytes(weigh(mcp.request))} />
            <Fact label="revision" value={mcp.revision} />
          </Facts>
          <Headers headers={mcp.headers} />
          <Envelope text={mcp.request} />
        </Pane>

        <Pane title="Response">
          <ResponseLine
            status={mcp.error || mcp.status === 0 ? 'never answered' : mcp.status}
            tone={tone}
            ms={mcp.latencyMs}
            note={mcp.streaming ? 'event stream' : undefined}
          />
          <Facts>
            <Fact label="content-type" value={header(mcp.responseHeaders, 'content-type')} />
            <Fact
              label="received"
              value={mcp.response.length === 0 ? undefined : formatBytes(weigh(mcp.response))}
            />
            <Fact label="session" value={header(mcp.responseHeaders, 'mcp-session-id')} />
          </Facts>
          <Headers headers={mcp.responseHeaders} />

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

          {mcp.response.length > 0 ? <Envelope text={mcp.response} /> : null}
        </Pane>
      </Panes>
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
  slowest,
  open,
  flash,
  onToggle,
}: {
  exchange: HookExchange
  slowest: number
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const hook = exchange.record
  // Not `statusTone`: a hook is something you asked for, so nothing it answers
  // is ever the expected refusal an anonymous call is looking for.
  const tone: Tone = hook.error || hook.status === 0 || hook.status >= 400 ? 'bad' : 'good'
  // A hook that sat the call out never sent anything, so it has no status to
  // report and nothing went wrong. Read as one, `status: 0` says "never
  // answered" in red about a hook nobody was ever asked anything by.
  const skipped = hook.skipped
  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Hook')} · ${hook.hook}${
        // Only when there is more than one to tell apart: `action 1` on a hook
        // that makes a single call is a number nobody needed.
        hook.step > 1 ? ` · action ${hook.step}` : ''
      } (${hook.phase})`}
      kind="hook"
      op={hook.step > 1 ? `${hook.hook} · ${hook.step}` : hook.hook}
      where={
        <>
          {hook.phase} {hook.tool} <span className="opacity-70">· {terse(hook.url)}</span>
        </>
      }
      flags={
        <>
          {skipped === undefined ? null : <Flag>did not fire</Flag>}
          {skipped === undefined && hook.status === 0 ? (
            <Flag tone="bad">never answered</Flag>
          ) : null}
          {hook.stoppedTheCall ? <Flag tone="bad">stopped the call</Flag> : null}
        </>
      }
      turn={turnCell(exchange.turn, 'hook')}
      status={
        skipped === undefined
          ? hook.status === 0
            ? UNANSWERED
            : { text: String(hook.status), tone }
          : { text: '—', tone: 'neutral' }
      }
      size={hook.response.length === 0 ? null : weigh(hook.response)}
      ms={skipped === undefined ? hook.latencyMs : null}
      slowest={slowest}
      open={open}
      flash={flash}
      onToggle={onToggle}
    >
      <Panes>
        <Pane title="Request">
          <RequestLine method={hook.method} url={hook.url} />
          {skipped === undefined ? null : (
            <p className="text-muted text-sm">
              Nothing was sent: this hook fires under <span className="font-mono">{skipped}</span>,
              and that came back false for this call. No credential was resolved, and the tool call
              went ahead untouched — the address above is the template it would have rendered.
            </p>
          )}
          <Facts>
            <Fact label="content-type" value={header(hook.headers, 'content-type')} />
            <Fact
              label="sent"
              value={hook.request.length === 0 ? undefined : formatBytes(weigh(hook.request))}
            />
            <Fact label="phase" value={hook.phase} />
          </Facts>
          <Headers headers={hook.headers} />
          {hook.request.length > 0 ? <Body text={hook.request} /> : null}

          {/*
            The parts, by the field each went out under. The field is the half the
            endpoint actually reads — a form carrying the right file under the
            wrong name is refused exactly like one carrying no file at all.
          */}
          {hook.files.length > 0 ? (
            <ul className="space-y-0.5 text-xs">
              {hook.files.map((file) => (
                <li
                  key={`${file.field}·${file.id}`}
                  className="flex flex-wrap items-baseline gap-2"
                >
                  <Badge tone="neutral">{file.field}</Badge>
                  <span className="font-mono">{file.name}</span>
                  <span className="text-faint">
                    {file.contentType} · {formatBytes(file.size)}
                  </span>
                </li>
              ))}
            </ul>
          ) : null}

          {/*
            And when there was neither. Said rather than left blank: an empty
            Request pane reads as something the panel failed to show, and the
            answer — this action declares no body — is one somebody would
            otherwise go looking for in `mcp/`.
          */}
          {skipped === undefined && hook.request.length === 0 && hook.files.length === 0 ? (
            <p className="text-muted text-sm">
              No body: this action declares neither <span className="font-mono">json:</span> nor{' '}
              <span className="font-mono">multipart:</span>, so the request carried nothing.
            </p>
          ) : null}
        </Pane>

        <Pane title="Response">
          {skipped === undefined ? (
            <ResponseLine
              status={hook.status === 0 ? 'never answered' : hook.status}
              tone={tone}
              ms={hook.latencyMs}
            />
          ) : (
            <p className="font-mono text-faint text-xs">nothing was sent</p>
          )}
          <Facts>
            <Fact label="content-type" value={header(hook.responseHeaders, 'content-type')} />
            <Fact
              label="received"
              value={hook.response.length === 0 ? undefined : formatBytes(weigh(hook.response))}
            />
            <Fact label="retry-after" value={header(hook.responseHeaders, 'retry-after')} />
          </Facts>
          <Headers headers={hook.responseHeaders} />

          {hook.error ? (
            <p className="flex flex-wrap items-baseline gap-2 text-xs">
              <Badge tone="bad">{hook.stoppedTheCall ? 'refused' : 'failed, stepped over'}</Badge>
              <span className="text-muted">{hook.error}</span>
            </p>
          ) : null}

          {hook.response.length > 0 ? (
            <Body text={hook.response} />
          ) : (
            <p className="text-muted text-sm">{nothingCameBack(hook)}</p>
          )}
        </Pane>
      </Panes>

      {hook.error ? (
        <Strip title="Consequence">
          <p className="text-muted text-xs">
            <Consequence hook={hook} />
          </p>
        </Strip>
      ) : null}
    </Card>
  )
}

/**
 * What a hook's failure cost the tool call it fired on.
 *
 * The error above says what went wrong at the hook's own address; this says what
 * it did to the run, which is the half a reader has to act on. The three cases
 * really are three different events: a gate that stopped a call before it went
 * out, a report on a call that already ran and cannot be taken back, and a
 * failure the file asked to be stepped over.
 */
function Consequence({ hook }: { hook: HookRecord }) {
  if (!hook.stoppedTheCall) {
    return (
      <>
        The tool call was unaffected: this hook is{' '}
        <span className="font-mono">on_error: continue</span>, so the failure is recorded and
        nothing else.
      </>
    )
  }
  return hook.phase === 'before' ? (
    <>
      The tool call never went out: this hook is <span className="font-mono">on_error: fail</span>{' '}
      and fires before the call, so the failure is the refusal.
    </>
  ) : (
    <>
      The tool call had already gone out and its result stands —{' '}
      <span className="font-mono">on_error: fail</span> after the call reports the failure, it
      cannot undo one.
    </>
  )
}

/**
 * The empty-response line, which has three reasons to be empty and one used to
 * cover all of them.
 *
 * "Nothing, which a hook is entitled to answer" is true of a `204` and a lie
 * about a request that never got an answer at all — and it was printed under the
 * error saying so, which is exactly where a reader stops trusting the panel.
 */
function nothingCameBack(hook: HookRecord): string {
  if (hook.skipped !== undefined) {
    return 'Nothing, because nobody was asked.'
  }
  if (hook.status === 0) {
    return 'Nothing came back: the request never reached an answer, so there is no body to read.'
  }
  return 'Nothing, which a hook is entitled to answer.'
}

/**
 * One tool invocation, in the same two halves — three, when it captured.
 *
 * The arguments the model produced are the request and what the tool handed back
 * is the response; the schema check is `mire`'s reading of the first, so it sits
 * in the band underneath with the other readings. A call that never left the
 * process says so, because a simulated result that looks plausible is the
 * easiest way to believe an integration works when nothing has been wired up.
 *
 * **Captured** is the band that only shows up when the answering server's
 * `capture:` pulled something out of that answer. It is the one part that is not
 * a wire: it is what a later hook's URL, header or body will render, and reading
 * it here is the difference between a rendered address you can explain and one
 * you cannot. A rule that matched nothing captures nothing, so the band is
 * simply absent — which is the same answer, and the log says which path was
 * tried.
 */
function ToolCard({
  exchange,
  slowest,
  open,
  flash,
  onToggle,
}: {
  exchange: ToolExchange
  slowest: number
  open: boolean
  flash: boolean
  onToggle: () => void
}) {
  const tool: ToolInvocation = exchange.invocation
  const captured = Object.entries(tool.captured)
  // Not `statusTone`, for the reason the protocol card gives: nothing is ever
  // expected to refuse a tool call, so a `401` here is bad news whoever the run
  // was calling as.
  const tone: Tone =
    tool.status === undefined || tool.status === 0 || tool.status >= 400 ? 'bad' : 'good'

  return (
    <Card
      id={exchange.id}
      label={`${turnLabel(exchange.turn, 'Call')} · ${tool.call.name}`}
      kind="tool"
      op={tool.call.name}
      // A call that never left the process has no server to name, so the column
      // that names one is left empty rather than filled with a stand-in.
      where={tool.source === 'mcp' ? (tool.server ?? 'mcp') : ''}
      flags={
        <>
          {tool.source === 'mcp' ? null : <Flag>simulated, nothing executed</Flag>}
          {tool.status === 0 ? <Flag tone="bad">never answered</Flag> : null}
          {tool.error ? <Flag tone="bad">tool failed</Flag> : null}
          {tool.schemaErrors.length > 0 ? <Flag tone="warn">schema</Flag> : null}
          {tool.reportedError ? <Flag tone="warn">reported a problem</Flag> : null}
        </>
      }
      turn={turnCell(exchange.turn, 'call')}
      status={
        // A tool nothing sent — simulated, or refused by a gate — has no status
        // to report, and the flags beside it already say which.
        tool.status === undefined
          ? { text: '—', tone: 'neutral' }
          : tool.status === 0
            ? UNANSWERED
            : { text: String(tool.status), tone }
      }
      size={tool.result.length === 0 ? null : weigh(tool.result)}
      ms={tool.latencyMs ?? null}
      slowest={slowest}
      open={open}
      flash={flash}
      onToggle={onToggle}
    >
      <Panes>
        <Pane title="Request">
          <p className="font-mono text-xs">
            {tool.call.name}
            {tool.call.id ? <span className="text-faint"> · id {tool.call.id}</span> : null}
          </p>
          <Tree value={tool.call.arguments} />
        </Pane>

        <Pane title="Response">
          {tool.status === undefined ? (
            <p className="font-mono text-faint text-xs">no status — nothing left the process</p>
          ) : (
            <ResponseLine
              status={tool.status === 0 ? 'never answered' : tool.status}
              tone={tone}
              ms={tool.latencyMs ?? null}
            />
          )}
          <Facts>
            <Fact
              label="received"
              value={tool.result.length === 0 ? undefined : formatBytes(weigh(tool.result))}
            />
          </Facts>

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
        </Pane>
      </Panes>

      <Strip title="Schema">
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
          <Badge tone="good">arguments match the schema</Badge>
        )}
      </Strip>

      {captured.length > 0 ? (
        <Strip title="Captured">
          {/* Name, then value, laid out the way the tree lays out a field —
              because that is what it is: a field of the answer above, under the
              name a template will call it by. */}
          <ul className="min-w-0 overflow-x-auto font-mono text-[11.5px]">
            {captured.map(([name, value]) => (
              <li key={name} className="py-px">
                <span className="text-muted">{name}</span>
                <span className="text-faint">: </span>
                <JsonTree value={value} />
              </li>
            ))}
          </ul>
        </Strip>
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
        <Badge>{formatBytes(stream.bytes)}</Badge>
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
