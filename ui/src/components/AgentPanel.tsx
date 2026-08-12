import { useState } from 'react'
import type { StopOutcome, ToolInvocation, Trace, Turn } from '../api'
import { Badge, Code, CopyButton, Panel, type Tone } from './primitives'

/** Every way the loop can end, in a sentence. */
export function describeStop(stop: StopOutcome): { tone: Tone; text: string } {
  switch (stop.outcome) {
    case 'stopped':
      return stop.reason.predicate === 'noToolCalls'
        ? { tone: 'good', text: 'Stopped: the model asked for no more tools.' }
        : { tone: 'good', text: `Stopped: finish reason "${stop.reason.value}".` }
    case 'maxIterations':
      return { tone: 'warn', text: `Ran out of turns after ${stop.limit}.` }
    case 'deadline':
      return { tone: 'warn', text: `Ran out of time after ${stop.afterMs} ms.` }
    case 'repeatedCall':
      return {
        tone: 'bad',
        text: `The model asked for "${stop.tool}" again with the same arguments on turn ${stop.atTurn} — a loop, not progress.`,
      }
    case 'predicateNeverEvaluable':
      return {
        tone: 'bad',
        text: `\`${stop.predicate}\` could never be evaluated in ${stop.turns} turns — the endpoint never reported one. The loop was not slow, it was unfalsifiable.`,
      }
  }
}

export function AgentPanel({
  turns,
  trace,
  running,
}: {
  turns: Turn[]
  trace: Trace | null
  running: boolean
}) {
  if (turns.length === 0 && !running) {
    return null
  }

  return (
    <Panel
      title="Agent"
      actions={
        trace ? (
          <span className="flex items-center gap-2">
            <span className="text-stone-500 text-xs dark:text-stone-400">
              {trace.turns.length} turns · {trace.durationMs} ms
            </span>
            <CopyButton text={JSON.stringify(trace, null, 2)} label="Export trace" />
          </span>
        ) : (
          <span className="text-stone-500 text-xs dark:text-stone-400">running…</span>
        )
      }
    >
      <ol className="space-y-2">
        {turns.map((turn) => (
          <TurnCard key={turn.index} turn={turn} />
        ))}
      </ol>

      {trace ? <Verdict stop={trace.stop} /> : null}
    </Panel>
  )
}

function Verdict({ stop }: { stop: StopOutcome }) {
  const { tone, text } = describeStop(stop)
  return (
    <p className="mt-3 flex flex-wrap items-baseline gap-2 text-sm">
      <Badge tone={tone}>{stop.outcome}</Badge>
      <span>{text}</span>
    </p>
  )
}

/** One turn, closed by default: a timeline you can read before you dig. */
function TurnCard({ turn }: { turn: Turn }) {
  const [open, setOpen] = useState(false)
  const decoded = turn.call.response?.decoded
  const content = decoded?.kind === 'completion' ? decoded.content : null
  const failedSchema = turn.tools.some((tool) => tool.schemaErrors.length > 0)
  const failedTool = turn.tools.some((tool) => tool.error)

  return (
    <li className="rounded border border-stone-200 dark:border-stone-800">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full flex-wrap items-baseline gap-2 px-2 py-1.5 text-left"
      >
        <span className="font-medium text-sm">
          {open ? '▾' : '▸'} Turn {turn.index}
        </span>
        <Badge tone={turn.call.response && turn.call.response.http.status < 300 ? 'good' : 'bad'}>
          {turn.call.response?.http.status ?? '—'}
        </Badge>
        <span className="text-stone-500 text-xs dark:text-stone-400">
          {turn.call.response?.http.latencyMs ?? 0} ms
        </span>
        {turn.tools.length > 0 ? (
          <Badge tone={failedTool ? 'bad' : failedSchema ? 'warn' : 'neutral'}>
            {turn.tools.length} tool{turn.tools.length > 1 ? 's' : ''}
          </Badge>
        ) : null}
        {turn.decision.decision === 'stop' ? <Badge tone="good">last</Badge> : null}
        <span className="min-w-0 flex-1 truncate text-right text-stone-500 text-xs dark:text-stone-400">
          {content ?? turn.tools.map((tool) => tool.call.name).join(', ')}
        </span>
      </button>

      {open ? (
        <div className="space-y-3 border-stone-200 border-t p-2 dark:border-stone-800">
          {content ? <p className="whitespace-pre-wrap text-sm">{content}</p> : null}

          {turn.tools.map((tool) => (
            <ToolCard key={`${tool.call.id ?? ''}${tool.call.name}`} tool={tool} />
          ))}

          <details>
            <summary className="cursor-pointer text-stone-500 text-xs dark:text-stone-400">
              Request sent
            </summary>
            <Code>{turn.call.request.body}</Code>
          </details>
        </div>
      ) : null}
    </li>
  )
}

function ToolCard({ tool }: { tool: ToolInvocation }) {
  return (
    <div className="rounded bg-stone-100 p-2 dark:bg-stone-950">
      <p className="font-mono text-xs">
        {tool.call.name}({JSON.stringify(tool.call.arguments)})
      </p>

      <p className="mt-1 flex flex-wrap items-center gap-1.5 text-xs">
        {tool.source === 'mcp' ? (
          <>
            <Badge tone="warn">called for real</Badge>
            <span className="text-stone-600 dark:text-stone-400">
              via <span className="font-mono">{tool.server}</span>
              {tool.latencyMs === undefined ? null : ` · ${tool.latencyMs} ms`}
            </span>
          </>
        ) : (
          <Badge tone="neutral">simulated, nothing executed</Badge>
        )}
      </p>

      {tool.reportedError ? (
        <p className="mt-1 text-xs">
          <Badge tone="warn">the tool reported a problem</Badge>{' '}
          <span className="text-stone-600 dark:text-stone-400">
            which the model is meant to react to
          </span>
        </p>
      ) : null}

      {tool.schemaErrors.length > 0 ? (
        <ul className="mt-1 space-y-0.5">
          {tool.schemaErrors.map((error) => (
            <li key={error} className="text-xs">
              <Badge tone="warn">schema</Badge>{' '}
              <span className="text-stone-600 dark:text-stone-400">{error}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="mt-1 text-xs">
          <Badge tone="good">arguments match the schema</Badge>
        </p>
      )}

      {tool.error ? (
        <p className="mt-1 text-xs">
          <Badge tone="bad">tool failed</Badge>{' '}
          <span className="text-stone-600 dark:text-stone-400">{tool.error}</span>
        </p>
      ) : null}

      <p className="mt-1 font-mono text-[11px] text-stone-600 dark:text-stone-400">
        → {tool.result}
      </p>
    </div>
  )
}
