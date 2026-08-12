import type { CallOutcome, DecodeTrace, StreamView } from '../api'
import { EmbeddingPanel } from './EmbeddingPanel'
import { JsonTree } from './JsonTree'
import { Badge, Code, CopyButton, Panel, type Tone } from './primitives'

/**
 * A `401` is only bad news when you were expecting to be let in. Asked
 * anonymously, it is the answer you wanted, and it shows up green.
 */
export function statusTone(status: number, expectUnauthorized: boolean): Tone {
  if (status >= 200 && status < 300) return 'good'
  if (expectUnauthorized && (status === 401 || status === 403)) return 'good'
  if (status >= 400) return 'bad'
  return 'warn'
}

export function ResponsePanel({
  outcome,
  expectUnauthorized,
}: {
  outcome: CallOutcome
  expectUnauthorized: boolean
}) {
  const response = outcome.response
  if (!response) {
    return (
      <Panel title="Response">
        <p className="text-stone-500 text-sm dark:text-stone-400">Dry run — nothing was sent.</p>
      </Panel>
    )
  }

  const { http, decoded, decode } = response
  const tone = statusTone(http.status, expectUnauthorized)
  const protectedAsExpected = expectUnauthorized && (http.status === 401 || http.status === 403)

  return (
    <div className="space-y-3">
      <Panel
        title="Response"
        actions={
          <span className="flex items-center gap-2">
            <Badge tone={tone}>{http.status}</Badge>
            <span className="text-stone-500 text-xs dark:text-stone-400">{http.latencyMs} ms</span>
            {http.ttftMs === undefined ? null : (
              <Badge tone="neutral">first token {http.ttftMs} ms</Badge>
            )}
            {outcome.retriedAfterUnauthorized ? (
              <Badge tone="warn">credential refreshed, replayed once</Badge>
            ) : null}
          </span>
        }
      >
        {protectedAsExpected ? (
          <p className="mb-3 text-emerald-700 text-sm dark:text-emerald-300">
            The route is protected — that is a pass, not a failure.
          </p>
        ) : null}

        {decoded?.kind === 'completion' ? (
          <div className="space-y-3">
            {decoded.content === null ? (
              <p className="text-stone-500 text-sm dark:text-stone-400">
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

            {decoded.toolCalls.length > 0 ? (
              <div className="space-y-1">
                <h3 className="font-semibold text-xs">Tool calls</h3>
                {decoded.toolCalls.map((toolCall) => (
                  <Code key={`${toolCall.id ?? ''}${toolCall.name}`}>
                    {`${toolCall.name}(${JSON.stringify(toolCall.arguments)})`}
                  </Code>
                ))}
              </div>
            ) : null}
          </div>
        ) : null}

        {decoded?.kind === 'embedding' ? <EmbeddingPanel embedding={decoded} /> : null}

        {response.jsonError ? (
          <div className="space-y-1">
            <Badge tone="warn">not JSON</Badge>
            <p className="text-stone-500 text-xs dark:text-stone-400">{response.jsonError}</p>
            {response.bodyText ? <Code>{response.bodyText}</Code> : null}
          </div>
        ) : null}
      </Panel>

      {response.stream ? <StreamPanel stream={response.stream} /> : null}

      <DecodeTracePanel trace={decode} />

      <Panel
        title="Raw response"
        actions={
          <span className="flex items-center gap-2">
            {response.elided ? <Badge tone="neutral">vectors elided</Badge> : null}
            {response.bodyText ? <CopyButton text={response.bodyText} /> : null}
          </span>
        }
      >
        <div className="overflow-x-auto font-mono text-xs">
          <JsonTree value={response.raw} />
        </div>
      </Panel>
    </div>
  )
}

/**
 * What the stream did, rather than what it said.
 *
 * The counters are the half a non-streaming call cannot answer: whether chunks
 * really arrived separately, and whether the endpoint ended the stream or it
 * merely stopped — which is what a proxy cutting a long generation looks like.
 */
function StreamPanel({ stream }: { stream: StreamView }) {
  return (
    <Panel
      title="Stream"
      actions={
        stream.terminated ? (
          <Badge tone="good">ended cleanly</Badge>
        ) : (
          <Badge tone="warn">stopped without ending</Badge>
        )
      }
    >
      <div className="flex flex-wrap gap-2 text-xs">
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
        <p className="mt-2 text-stone-500 text-xs dark:text-stone-400">
          No end sentinel and no stop reason: the connection went quiet rather than finishing.
          Whatever arrived is above.
        </p>
      )}
    </Panel>
  )
}

function DecodeTracePanel({ trace }: { trace: DecodeTrace }) {
  const matched = Object.entries(trace.matched)
  const missed = Object.entries(trace.missed)

  if (matched.length === 0 && missed.length === 0 && trace.issues.length === 0) {
    return null
  }

  return (
    <Panel title="Decode">
      <ul className="space-y-1 text-xs">
        {matched.map(([field, path]) => (
          <li key={field} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="good">matched</Badge>
            <span className="font-medium">{field}</span>
            <span className="font-mono text-stone-600 dark:text-stone-400">{path}</span>
          </li>
        ))}
        {missed.map(([field, paths]) => (
          <li key={field} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="warn">missed</Badge>
            <span className="font-medium">{field}</span>
            <span className="font-mono text-stone-600 dark:text-stone-400">
              {paths.join('  ·  ')}
            </span>
          </li>
        ))}
        {trace.issues.map((issue) => (
          <li key={`${issue.field}${issue.path}`} className="flex flex-wrap items-baseline gap-2">
            <Badge tone="bad">wrong shape</Badge>
            <span className="font-medium">{issue.field}</span>
            <span className="font-mono text-stone-600 dark:text-stone-400">{issue.path}</span>
            <span className="text-stone-500 dark:text-stone-400">{issue.message}</span>
          </li>
        ))}
      </ul>
      {missed.length > 0 ? (
        <p className="mt-2 text-stone-500 text-xs dark:text-stone-400">
          Pick the right path in the raw tree below, then add it to the profile's{' '}
          <span className="font-mono">decode:</span> block — it reloads without a restart.
        </p>
      ) : null}
    </Panel>
  )
}
