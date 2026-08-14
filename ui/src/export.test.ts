import { describe, expect, it } from 'vitest'
import type { Message } from './api'
import type { Exchange } from './conversation'
import { exportFilename, runExport } from './export'

const AT = new Date('2026-08-14T09:31:07.482Z')

describe('exportFilename', () => {
  it('is sortable, and safe on a filesystem that refuses colons', () => {
    expect(exportFilename('chat', AT)).toBe('mire-chat-2026-08-14T09-31-07.json')
  })

  it('still names a file when no profile was selected', () => {
    expect(exportFilename(null, AT)).toBe('mire-run-2026-08-14T09-31-07.json')
  })
})

describe('runExport', () => {
  it('carries the run whole, with what it was pointed at', () => {
    const messages: Message[] = [{ role: 'user', content: 'ping' }]
    const exchanges: Exchange[] = []

    const payload = runExport({
      profile: 'chat',
      endpoint: 'https://models.internal/v1/chat/completions',
      identity: 'workload',
      messages,
      exchanges,
      at: AT,
    })

    expect(payload).toEqual({
      tool: 'mire',
      exportedAt: '2026-08-14T09:31:07.482Z',
      profile: 'chat',
      endpoint: 'https://models.internal/v1/chat/completions',
      identity: 'workload',
      messages,
      exchanges,
    })
  })

  it('summarises nothing: the reader wants the part you found boring', () => {
    const exchanges = [
      {
        kind: 'tool',
        id: 'tool-1',
        turn: 1,
        invocation: {
          call: { id: 'c1', name: 'get_weather', arguments: { city: 'Paris' } },
          source: 'mcp',
          server: 'weather',
          reportedError: false,
          schemaErrors: [],
          result: '{"temp": 21}',
        },
      },
    ] as Exchange[]

    const payload = runExport({
      profile: 'chat',
      endpoint: null,
      identity: null,
      messages: [],
      exchanges,
      at: AT,
    })

    expect(JSON.parse(JSON.stringify(payload)).exchanges).toEqual(exchanges)
  })
})
