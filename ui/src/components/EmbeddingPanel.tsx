import type { CheckOutcome, Embedding } from '../api'
import { Badge, type Tone } from './primitives'

const CHECK_LABELS: Record<keyof Embedding['checks'], string> = {
  count: 'count',
  dimensions: 'dimensions',
  finite: 'finite values',
  nonZeroNorm: 'non-zero norm',
  determinism: 'determinism',
}

function checkTone(outcome: CheckOutcome): Tone {
  if (outcome.status === 'pass') return 'good'
  if (outcome.status === 'fail') return 'bad'
  return 'neutral'
}

function checkNote(outcome: CheckOutcome): string | null {
  if (outcome.status === 'fail') return outcome.detail
  if (outcome.status === 'skipped') return outcome.reason
  return null
}

function describeDimensions(dimensions: Embedding['dimensions']): string {
  switch (dimensions.kind) {
    case 'uniform':
      return String(dimensions.value)
    case 'ragged':
      return `ragged — ${dimensions.values.join(', ')}`
    case 'unknown':
      return 'none decoded'
  }
}

/**
 * Shape first. A vector is a wall of floats and reading it tells you nothing;
 * its width, its norm and its distribution tell you everything.
 */
export function EmbeddingPanel({ embedding }: { embedding: Embedding }) {
  const checks = Object.entries(embedding.checks) as [keyof Embedding['checks'], CheckOutcome][]

  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Stat label="vectors" value={String(embedding.count)} />
        <Stat label="dimensions" value={describeDimensions(embedding.dimensions)} />
        <Stat label="encoding" value={embedding.encoding} />
        <Stat label="tokens" value={embedding.usage?.totalTokens?.toString() ?? '—'} />
      </dl>

      <div className="space-y-1">
        {checks.map(([name, outcome]) => {
          const note = checkNote(outcome)
          return (
            <div key={name} className="flex flex-wrap items-baseline gap-2 text-xs">
              <Badge tone={checkTone(outcome)}>{outcome.status}</Badge>
              <span className="font-medium">{CHECK_LABELS[name]}</span>
              {note ? <span className="text-muted">{note}</span> : null}
            </div>
          )
        })}
      </div>

      <ul className="space-y-2">
        {embedding.vectors.map((vector) => (
          <li key={vector.index} className="rounded border border-line p-2">
            <div className="flex flex-wrap items-baseline gap-2 text-xs">
              <span className="font-medium">#{vector.index}</span>
              <span className="text-muted">
                {vector.dimensions} dims · norm {vector.norm.toFixed(4)}
              </span>
              {vector.finite ? null : <Badge tone="bad">non-finite values</Badge>}
            </div>
            <p className="mt-1 truncate font-mono text-[11px] text-muted">
              [
              {vector.sample.map((value) => (value === null ? 'NaN' : value.toFixed(4))).join(', ')}
              {vector.sample.length < vector.dimensions ? ', …' : ''}]
            </p>
            <Histogram
              buckets={vector.histogram.buckets}
              min={vector.histogram.min}
              max={vector.histogram.max}
            />
          </li>
        ))}
      </ul>

      {embedding.full ? (
        <p className="text-muted text-xs">
          Full vectors attached ({embedding.full.length} × {embedding.full[0]?.length ?? 0}).
        </p>
      ) : null}
    </div>
  )
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-muted text-xs">{label}</dt>
      <dd className="font-medium font-mono text-sm">{value}</dd>
    </div>
  )
}

function Histogram({ buckets, min, max }: { buckets: number[]; min: number; max: number }) {
  const peak = Math.max(...buckets, 1)
  const width = (max - min) / Math.max(buckets.length, 1)

  return (
    <div className="mt-2">
      <div className="flex h-10 items-end gap-px" role="img" aria-label="value distribution">
        {buckets.map((count, index) => {
          const from = min + width * index
          return (
            <div
              key={`${from}`}
              className="flex-1 rounded-t bg-line-strong"
              style={{ height: `${Math.max((count / peak) * 100, 2)}%` }}
              title={`${count} values in [${from.toFixed(3)}, ${(from + width).toFixed(3)}]`}
            />
          )
        })}
      </div>
      <div className="flex justify-between font-mono text-[10px] text-faint">
        <span>{min.toFixed(3)}</span>
        <span>{max.toFixed(3)}</span>
      </div>
    </div>
  )
}
