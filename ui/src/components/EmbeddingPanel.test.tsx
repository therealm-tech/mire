import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import type { Embedding, VectorSummary } from '../api'
import { EmbeddingPanel } from './EmbeddingPanel'

function vector(item: number, position: number, index: number): VectorSummary {
  return {
    index,
    item,
    position,
    dimensions: 1024,
    norm: 1,
    sample: [0.1, 0.2],
    finite: true,
    histogram: { min: 0, max: 1, buckets: [1, 2, 3] },
  }
}

function embedding(overrides: Partial<Embedding>): Embedding {
  return {
    kind: 'embedding',
    count: 1,
    vectorCount: 1,
    vectorsPerItem: [1],
    dimensions: { kind: 'uniform', value: 1024 },
    encoding: 'float',
    usage: null,
    vectors: [vector(0, 0, 0)],
    checks: {
      count: { status: 'pass' },
      dimensions: { status: 'pass' },
      finite: { status: 'pass' },
      nonZeroNorm: { status: 'pass' },
      determinism: { status: 'skipped', reason: 'send `repeat: 2` to check this' },
    },
    ...overrides,
  }
}

describe('EmbeddingPanel', () => {
  it('names the vectors by input when there is one of each', () => {
    render(
      <EmbeddingPanel
        embedding={embedding({
          count: 2,
          vectorCount: 2,
          vectorsPerItem: [1, 1],
          vectors: [vector(0, 0, 0), vector(1, 0, 1)],
        })}
      />,
    )

    expect(screen.getByText('vectors')).toBeInTheDocument()
    expect(screen.getByText('#0')).toBeInTheDocument()
    expect(screen.getByText('#1')).toBeInTheDocument()
  })

  it('groups a multi-vector answer under the input it belongs to', () => {
    render(
      <EmbeddingPanel
        embedding={embedding({
          count: 2,
          vectorCount: 5,
          vectorsPerItem: [3, 2],
          vectors: [
            vector(0, 0, 0),
            vector(0, 1, 1),
            vector(0, 2, 2),
            vector(1, 0, 3),
            vector(1, 1, 4),
          ],
        })}
      />,
    )

    // Two items, five vectors: both counts are worth reading, and the second is
    // what a flattened rendering would have shown as the first.
    expect(screen.getByText('items')).toBeInTheDocument()
    expect(screen.getByText('3 vectors')).toBeInTheDocument()
    expect(screen.getByText('#0.2')).toBeInTheDocument()
    expect(screen.getByText('#1.0')).toBeInTheDocument()
  })

  it('says how many of an item’s vectors it stopped at', () => {
    render(
      <EmbeddingPanel
        embedding={embedding({
          vectorCount: 512,
          vectorsPerItem: [512],
          vectors: [vector(0, 0, 0), vector(0, 1, 1)],
        })}
      />,
    )

    expect(screen.getByText(/512 vectors · first 2 shown/)).toBeInTheDocument()
  })
})
