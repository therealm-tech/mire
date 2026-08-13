import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { App } from './App'
import { describeStop, statusTone } from './conversation'

const PROFILES = {
  profiles: [
    {
      name: 'chat',
      kind: 'chat',
      url: 'https://models.internal/v1/chat/completions',
      auth: null,
      mcp: [],
      source: '/tmp/chat.yaml',
      hasDecode: true,
    },
    {
      name: 'embed',
      kind: 'embedding',
      url: 'https://models.internal/v1/embeddings',
      auth: null,
      mcp: [],
      source: '/tmp/embed.yaml',
      hasDecode: true,
    },
    // Names a credential this tab has to be asked for, and three MCP servers.
    {
      name: 'guarded',
      kind: 'chat',
      url: 'http://127.0.0.1:11435/v1/messages',
      auth: 'pasted',
      mcp: ['dev', 'keyed', 'ghost'],
      source: '/tmp/guarded.yaml',
      hasDecode: true,
    },
    // Calls as a human, which is the one identity nobody can put in a file.
    {
      name: 'as-me',
      kind: 'chat',
      url: 'https://models.internal/v1/as-me',
      auth: 'me',
      mcp: [],
      source: '/tmp/as-me.yaml',
      hasDecode: true,
    },
    // Broken on purpose: `gateway` may only be sent to 127.0.0.1, and this
    // points somewhere else. Every call it makes is refused before it goes out.
    {
      name: 'pinned',
      kind: 'chat',
      url: 'https://models.internal/v1/pinned',
      auth: 'gateway',
      mcp: [],
      source: '/tmp/pinned.yaml',
      hasDecode: true,
    },
  ],
  issues: [],
}

const AUTH = {
  providers: [
    {
      name: 'anonymous',
      kind: 'anonymous',
      needsValue: false,
      needsLogin: false,
      allowedHosts: [],
    },
    { name: 'pasted', kind: 'token', needsValue: true, needsLogin: false, allowedHosts: [] },
    // Pinned to the local gateway, so it is a choice for `guarded` and not one
    // for the profiles pointing at models.internal.
    {
      name: 'gateway',
      kind: 'token',
      needsValue: false,
      needsLogin: false,
      allowedHosts: ['127.0.0.1'],
    },
    { name: 'me', kind: 'oidc_browser', needsValue: false, needsLogin: true, allowedHosts: [] },
  ],
  issues: [],
}

/**
 * Two servers, authenticating in the two ways `mcp.yaml` allows: `dev` names a
 * provider outright, `keyed` reaches for one from inside a header template. Both
 * are settled here rather than chosen per call, which is the whole point of
 * showing them apart from the selector.
 */
const MCP = {
  servers: [
    {
      name: 'dev',
      url: 'http://127.0.0.1:11436/mcp',
      auth: 'me',
      tools: ['get_weather'],
      headers: [],
      usesAuth: [],
    },
    {
      name: 'keyed',
      url: 'https://files.internal/mcp',
      tools: [],
      headers: ['x-api-key'],
      usesAuth: ['gateway'],
    },
  ],
  issues: [],
}

function completion(status: number) {
  return {
    profile: 'chat',
    auth: 'anonymous',
    request: {
      method: 'POST',
      url: 'https://models.internal/v1/chat/completions',
      headers: { authorization: '***' },
      body: '{"messages":[]}',
    },
    curl: "curl -sS -X POST 'https://models.internal/v1/chat/completions'",
    response: {
      http: { status, headers: {}, latencyMs: 12 },
      raw: { choices: [{ message: { content: 'pong' } }] },
      elided: false,
      decoded: {
        kind: 'completion',
        content: status === 200 ? 'pong' : null,
        toolCalls: [],
        finishReason: 'stop',
        usage: null,
      },
      decode: { matched: { content: '$.choices[0].message.content' }, missed: {}, issues: [] },
    },
    retriedAfterUnauthorized: false,
  }
}

/**
 * A turn that answered and asked for nothing, which is what a chat profile with
 * no tools produces: the loop stops on turn one.
 */
function answerTurn(status: number, content: string | null = 'pong') {
  const call = completion(status)
  return {
    index: 1,
    call: {
      ...call,
      response: { ...call.response, decoded: { ...call.response.decoded, content } },
    },
    tools: [],
    decision: {
      decision: 'stop',
      stop: { outcome: 'stopped', reason: { predicate: 'noToolCalls' } },
    },
  }
}

/**
 * The event stream `POST /api/agent` emits: one `turn` per turn, then the trace.
 *
 * Every chat send goes through this now, so the tests speak it too rather than
 * the single-shot shape the UI no longer asks for.
 */
function agentStream(turns: ReturnType<typeof answerTurn>[]): string {
  const trace = {
    profile: 'chat',
    auth: 'anonymous',
    turns,
    stop: { outcome: 'stopped', reason: { predicate: 'noToolCalls' } },
    durationMs: 120,
  }

  return [
    ...turns.flatMap((turn) => [
      'event: turn',
      `data: ${JSON.stringify({ event: 'turn', ...turn })}`,
      '',
    ]),
    'event: done',
    `data: ${JSON.stringify({ event: 'done', ...trace })}`,
    '',
    '',
  ].join('\n')
}

/** An SSE response, as `mire` serves both streaming endpoints. */
function sse(text: string): Response {
  return new Response(text, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  })
}

/**
 * The `<li>` one exchange renders, found by the label on its toggle.
 *
 * Needed now that a run puts five cards on the page: `authorization: ***` and
 * `"city": "Paris"` are each true of more than one of them, and an unscoped
 * assertion would be asserting the wrong card as readily as the right one.
 */
function card(label: RegExp): HTMLElement {
  const toggle = screen.getByRole('button', { name: label })
  const item = toggle.closest('li')
  if (!item) {
    throw new Error(`no card labelled ${label}`)
  }
  return item
}

/** The `<section>` a titled panel renders, for assertions scoped to one panel. */
function panel(title: string): HTMLElement {
  const heading = screen.getByRole('heading', { name: title })
  const section = heading.closest('section')
  if (!section) {
    throw new Error(`no panel titled ${title}`)
  }
  return section
}

/**
 * Answers each route with a canned payload, like `mire` would. A string payload
 * is served as an event stream, which is what the two streaming routes return.
 */
function mockApi(routes: Record<string, unknown>) {
  return vi.fn((input: RequestInfo | URL) => {
    const url = String(input)
    const match = Object.entries(routes).find(([suffix]) => url.endsWith(suffix))
    if (!match) {
      throw new Error(`unexpected fetch: ${url}`)
    }
    if (typeof match[1] === 'string') {
      return Promise.resolve(sse(match[1]))
    }
    return Promise.resolve(
      new Response(JSON.stringify(match[1]), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    )
  })
}

/** Records every `api/agent` body, so a test can assert what went out. */
function recordingApi(answers: string[]) {
  const sent: Array<Record<string, unknown>> = []
  let turn = 0

  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.endsWith('api/profiles')) {
      return Promise.resolve(Response.json(PROFILES))
    }
    if (url.endsWith('api/auth')) {
      return Promise.resolve(Response.json(AUTH))
    }
    if (url.endsWith('api/mcp')) {
      return Promise.resolve(Response.json(MCP))
    }
    if (url.endsWith('api/agent')) {
      sent.push(JSON.parse(String(init?.body)) as Record<string, unknown>)
      const answer = answers[turn] ?? 'pong'
      turn += 1
      return Promise.resolve(sse(agentStream([answerTurn(200, answer)])))
    }
    throw new Error(`unexpected fetch: ${url}`)
  })

  return { fetchMock, sent }
}

beforeEach(() => {
  vi.stubGlobal('fetch', mockApi({ 'api/profiles': PROFILES, 'api/auth': AUTH, 'api/mcp': MCP }))
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('statusTone', () => {
  it('treats an expected 401 as good news', () => {
    expect(statusTone(401, true)).toBe('good')
    expect(statusTone(403, true)).toBe('good')
  })

  it('treats an unexpected 401 as a failure', () => {
    expect(statusTone(401, false)).toBe('bad')
  })

  it('treats a 2xx as good either way', () => {
    expect(statusTone(200, false)).toBe('good')
    expect(statusTone(204, true)).toBe('good')
  })
})

describe('App', () => {
  it('lists the profiles and the identity the selected one calls with', async () => {
    const user = userEvent.setup()
    render(<App />)

    // Anchored: a profile of kind `chat` carries the word in its badge too.
    expect(await screen.findByRole('button', { name: /^chat/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /embed/ })).toBeInTheDocument()

    // `chat` declares no `auth:`, which resolves to the anonymous provider —
    // shown, and said out loud rather than left to be inferred from a blank.
    // `toHaveTextContent`, because the sentence is split around a `<span>`.
    expect(within(screen.getByTestId('model-auth')).getByText('anonymous')).toBeInTheDocument()
    expect(screen.getByTestId('model-auth')).toHaveTextContent('no auth: in this profile')

    // Following the profile, because that is where the identity is declared.
    await user.click(screen.getByRole('button', { name: /as-me/ }))
    expect(within(screen.getByTestId('model-auth')).getByText('me')).toBeInTheDocument()
  })

  it('prompts for a credential only when the provider needs one', async () => {
    const user = userEvent.setup()
    render(<App />)

    await screen.findByRole('button', { name: /^chat/ })
    expect(screen.queryByPlaceholderText('paste the credential')).not.toBeInTheDocument()

    // `guarded` names `pasted`, whose value the registry does not know.
    await user.click(screen.getByRole('button', { name: /guarded/ }))
    expect(screen.getByPlaceholderText('paste the credential')).toBeInTheDocument()
  })

  it('shows an expected 401 as a pass rather than a failure', async () => {
    const user = userEvent.setup()
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/agent': agentStream([answerTurn(401, null)]),
      }),
    )
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      // Scoped to the paragraph, and to wording the auth panel's own hint does
      // not share: a bare regex matches ancestors too.
      expect(
        screen.getByText(/that is a pass, not a failure/i, { selector: 'p' }),
      ).toBeInTheDocument()
    })
  })

  it('offers no way to call as somebody else', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))

    // The identity is the profile's. Nothing in the panel switches it — the only
    // buttons here are the ones that fetch a session or drop it.
    const buttons = within(panel('Auth'))
      .queryAllByRole('button')
      .map((button) => button.textContent ?? '')
    expect(buttons.filter((label) => /^(anonymous|pasted|gateway|me)$/.test(label))).toEqual([])
  })

  it('never puts an auth of its own on the wire', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // The profile says who it calls as, and the server reads the same file. A
    // copy travelling alongside is a second thing that can disagree with it.
    expect(sent[0]).not.toHaveProperty('auth')
  })

  it('says when a profile names a credential that cannot reach it', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /pinned/ }))

    expect(screen.getByText('out of allowed_hosts')).toBeInTheDocument()
    expect(screen.getByText(/refused before anything goes out/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /guarded/ }))
    expect(screen.queryByText('out of allowed_hosts')).not.toBeInTheDocument()
  })

  it('lists the MCP identities apart from the model one', async () => {
    const user = userEvent.setup()
    render(<App />)

    // A profile with no `mcp:` has nothing to say here, so it says nothing.
    await screen.findByRole('button', { name: /^chat/ })
    expect(screen.queryByRole('heading', { name: 'MCP servers' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /guarded/ }))
    expect(screen.getByRole('heading', { name: 'MCP servers' })).toBeInTheDocument()

    // `guarded` authenticates the model with `pasted`; none of that reaches the
    // servers, which answer to their own entries.
    expect(within(screen.getByTestId('model-auth')).getByText('pasted')).toBeInTheDocument()

    // Named outright, and the browser provider it names has no session — so a
    // tool call would be refused before anything went out.
    const dev = screen.getByTestId('mcp-dev')
    const devIdentities = within(screen.getByTestId('mcp-dev-identities'))
    expect(devIdentities.getByText('me')).toBeInTheDocument()
    expect(devIdentities.getByText('not signed in')).toBeInTheDocument()

    // Reached from a header template rather than declared as `auth:`.
    const keyed = screen.getByTestId('mcp-keyed')
    expect(within(keyed).getByText('gateway')).toBeInTheDocument()
    expect(within(keyed).getByText(/in a header template/)).toBeInTheDocument()

    // Nothing here selects a credential — the only button is the one that gets
    // a session, and a server that needs none offers nothing at all.
    expect(within(dev).getAllByRole('button')).toHaveLength(1)
    expect(within(dev).getByRole('button', { name: /Sign in to me/ })).toBeInTheDocument()
    expect(within(keyed).queryByRole('button')).not.toBeInTheDocument()

    // A profile naming a server that `mcp.yaml` never declared says so.
    expect(screen.getByText(/declared in no/)).toBeInTheDocument()
  })

  it('switches to the embedding input when an embedding profile is selected', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /embed/ }))
    expect(screen.getByText('One text per line')).toBeInTheDocument()
    expect(screen.getByText(/2\+ checks determinism/)).toBeInTheDocument()
  })
})

describe('describeStop', () => {
  it('reads a normal stop as good news', () => {
    expect(describeStop({ outcome: 'stopped', reason: { predicate: 'noToolCalls' } }).tone).toBe(
      'good',
    )
  })

  it('calls a repeated call a loop rather than progress', () => {
    const { tone, text } = describeStop({
      outcome: 'repeatedCall',
      tool: 'get_weather',
      atTurn: 2,
    })
    expect(tone).toBe('bad')
    expect(text).toMatch(/loop, not progress/)
  })

  it('says an unevaluable predicate was unfalsifiable, not slow', () => {
    const { tone, text } = describeStop({
      outcome: 'predicateNeverEvaluable',
      predicate: 'stop_when.finish_reason_in',
      turns: 3,
    })
    expect(tone).toBe('bad')
    expect(text).toMatch(/unfalsifiable/)
  })

  it('separates running out of turns from running out of time', () => {
    expect(describeStop({ outcome: 'maxIterations', limit: 6 }).text).toMatch(/turns/)
    expect(describeStop({ outcome: 'deadline', afterMs: 1200 }).text).toMatch(/time/)
  })
})

describe('agent mode', () => {
  it('offers the loop controls for a chat profile and not for an embedding one', async () => {
    const user = userEvent.setup()
    render(<App />)

    expect(await screen.findByRole('button', { name: 'Stream' })).toBeInTheDocument()
    expect(screen.getByLabelText(/max turns/)).toBeInTheDocument()

    // There is no second turn of an embedding, so there is nothing to loop.
    await user.click(screen.getByRole('button', { name: /embed/ }))
    expect(screen.queryByRole('button', { name: 'Stream' })).not.toBeInTheDocument()
    expect(screen.queryByLabelText(/max turns/)).not.toBeInTheDocument()
  })

  it('sends a chat profile through the loop and an embedding one straight out', async () => {
    const user = userEvent.setup()
    const urls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        urls.push(url)
        if (url.endsWith('api/profiles')) {
          return Promise.resolve(Response.json(PROFILES))
        }
        if (url.endsWith('api/auth')) {
          return Promise.resolve(Response.json(AUTH))
        }
        if (url.endsWith('api/mcp')) {
          return Promise.resolve(Response.json(MCP))
        }
        if (url.endsWith('api/agent')) {
          return Promise.resolve(sse(agentStream([answerTurn(200)])))
        }
        return Promise.resolve(Response.json(completion(200)))
      }),
    )

    render(<App />)

    // There is one send button, and for a chat profile it is the loop.
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(urls.some((url) => url.endsWith('api/agent'))).toBe(true))

    await user.click(screen.getByRole('button', { name: /embed/ }))
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(urls.some((url) => url.endsWith('api/call'))).toBe(true))
  })

  it('answers in the transcript and records the timings underneath', async () => {
    const user = userEvent.setup()
    const outcome = completion(200)
    const streamed = {
      ...outcome,
      response: {
        ...outcome.response,
        http: { ...outcome.response.http, ttftMs: 40 },
        decoded: { ...outcome.response.decoded, content: 'pong' },
        stream: {
          framing: 'sse',
          chunks: 3,
          deltas: 2,
          unparsable: 0,
          bytes: 120,
          terminated: true,
          firstChunkMs: 12,
        },
      },
    }
    const stream = [
      'event: open',
      `data: ${JSON.stringify({ event: 'open', status: 200, headers: {} })}`,
      '',
      'event: delta',
      `data: ${JSON.stringify({ event: 'delta', text: 'po' })}`,
      '',
      'event: delta',
      `data: ${JSON.stringify({ event: 'delta', text: 'ng' })}`,
      '',
      'event: done',
      `data: ${JSON.stringify({ event: 'done', ...streamed })}`,
      '',
      '',
    ].join('\n')

    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/call/stream': stream,
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Stream' }))

    // The answer joins the conversation as a turn rather than staying in a
    // panel of its own: the deltas were a preview of this bubble.
    await waitFor(() => {
      expect(within(panel('Conversation')).getByText('pong')).toBeInTheDocument()
    })

    // And the numbers a streaming test is actually for, one panel down.
    const traffic = within(panel('Traffic'))
    expect(traffic.getByText(/first token 40 ms/)).toBeInTheDocument()
    expect(traffic.getByText('3 chunks')).toBeInTheDocument()
    expect(traffic.getByText(/ended cleanly/)).toBeInTheDocument()
  })

  it('says so when a stream stops without ending', async () => {
    const user = userEvent.setup()
    const outcome = completion(200)
    const cut = {
      ...outcome,
      response: {
        ...outcome.response,
        decoded: { ...outcome.response.decoded, content: 'half a sen' },
        stream: {
          framing: 'sse',
          chunks: 1,
          deltas: 1,
          unparsable: 0,
          bytes: 40,
          terminated: false,
        },
      },
    }
    const stream = [
      'event: done',
      `data: ${JSON.stringify({ event: 'done', ...cut })}`,
      '',
      '',
    ].join('\n')

    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/call/stream': stream,
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Stream' }))

    await waitFor(() => {
      expect(screen.getByText(/stopped without ending/i)).toBeInTheDocument()
    })
    // What arrived is still shown: a truncated answer is the finding.
    expect(within(panel('Conversation')).getByText('half a sen')).toBeInTheDocument()
  })

  it('shows the tools a run called in the transcript and the verdict it ended on', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // What the run did on its own sits between the question and the answer,
    // where it happened, rather than in a panel somewhere else.
    const conversation = within(panel('Conversation'))
    await waitFor(() => {
      expect(conversation.getByText('get_weather')).toBeInTheDocument()
    })
    expect(conversation.getByText(/called for real via/)).toBeInTheDocument()
    expect(conversation.getByText(/asked for no more tools/i)).toBeInTheDocument()
  })

  it('shows what the endpoint said when it refused the turn', async () => {
    const user = userEvent.setup()
    const refused = {
      ...turnFixture(),
      call: {
        ...completion(400),
        response: {
          ...completion(400).response,
          http: { status: 400, headers: {}, latencyMs: 12 },
          bodyText: '{"error":"maximum context length is 32768 tokens"}',
        },
      },
      tools: [],
      // What a refusal really produces: nothing decodes, so the loop reads it as
      // a model that asked for no tools and calls the run done.
      decision: {
        decision: 'stop',
        stop: { outcome: 'stopped', reason: { predicate: 'noToolCalls' } },
      },
    }

    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/agent': [
          'event: turn',
          `data: ${JSON.stringify({ event: 'turn', ...refused })}`,
          '',
          '',
        ].join('\n'),
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // A refusal opens its own body: a red `400` with nothing under it is the
    // whole problem this answers.
    await waitFor(() => {
      expect(within(panel('Traffic')).getByText(/maximum context length/)).toBeInTheDocument()
    })
  })
})

function turnFixture() {
  return {
    index: 1,
    call: {
      ...completion(200),
      response: {
        ...completion(200).response,
        decoded: {
          kind: 'completion',
          content: null,
          toolCalls: [{ id: 'c1', name: 'get_weather', arguments: { city: 'Paris' } }],
          finishReason: 'tool_calls',
          usage: null,
        },
      },
    },
    tools: [
      {
        call: { id: 'c1', name: 'get_weather', arguments: { city: 'Paris' } },
        source: 'mcp',
        server: 'weather',
        latencyMs: 42,
        reportedError: false,
        schemaErrors: [],
        result: '{"temp": 21}',
      },
    ],
    mcp: [mcpExchange('tools/call', '{"jsonrpc":"2.0","id":1,"result":{"temp":21}}')],
    decision: { decision: 'continue', tools: 1 },
  }
}

/** One JSON-RPC round trip, as `POST /api/agent` reports it. */
function mcpExchange(method: string, response: string) {
  return {
    server: 'weather',
    url: 'http://127.0.0.1:11436/mcp',
    method,
    revision: '2026-07-28',
    notification: false,
    headers: { 'mcp-method': method, authorization: '***' },
    request: `{"jsonrpc":"2.0","id":1,"method":"${method}","params":{"city":"Paris"}}`,
    status: 200,
    streaming: false,
    response,
    latencyMs: 12,
  }
}

/** The handshake and the listing, which happen before the loop starts. */
function setupEvent() {
  return [
    'event: setup',
    `data: ${JSON.stringify({
      event: 'setup',
      mcp: [
        mcpExchange('initialize', '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"x"}}'),
        mcpExchange('tools/list', '{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}'),
      ],
    })}`,
    '',
  ]
}

function traceFixture() {
  return {
    profile: 'chat',
    auth: 'anonymous',
    turns: [turnFixture()],
    stop: { outcome: 'stopped', reason: { predicate: 'noToolCalls' } },
    durationMs: 120,
  }
}

/** A run that calls one MCP tool, so every kind of exchange is on the wire. */
function toolRunApi() {
  const stream = [
    ...setupEvent(),
    'event: turn',
    `data: ${JSON.stringify({ event: 'turn', ...turnFixture() })}`,
    '',
    'event: done',
    `data: ${JSON.stringify({ event: 'done', ...traceFixture() })}`,
    '',
    '',
  ].join('\n')

  return mockApi({
    'api/profiles': PROFILES,
    'api/auth': AUTH,
    'api/mcp': MCP,
    'api/agent': stream,
  })
}

describe('traffic', () => {
  it('says nothing has been on the wire before anything has', async () => {
    render(<App />)

    expect(await screen.findByRole('heading', { name: 'Traffic' })).toBeInTheDocument()
    expect(screen.getByText(/Nothing on the wire yet/)).toBeInTheDocument()
  })

  it('records the model call and the tool call as separate exchanges', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // One card for what was asked of the model, one for what the tool was asked.
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })
    expect(screen.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    // Two of those, plus the handshake, the listing and the `tools/call` under
    // the tool: five wires touched, five cards.
    expect(within(panel('Traffic')).getByText('5 exchanges')).toBeInTheDocument()
  })

  it('records what was said to the MCP server before the loop began', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // No tool was called by these, so a tool listing could never show them —
    // and a server that refuses the handshake produces nothing else at all.
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Setup · initialize/ })).toBeInTheDocument()
    })
    expect(screen.getByRole('button', { name: /Setup · tools\/list/ })).toBeInTheDocument()
    // The tool call's own JSON-RPC belongs to the turn that made it.
    expect(screen.getByRole('button', { name: /Turn 1 · tools\/call/ })).toBeInTheDocument()

    const handshake = within(card(/Setup · initialize/))
    expect(handshake.getByText('protocolVersion')).toBeInTheDocument()
    expect(handshake.getByText('"x"')).toBeInTheDocument()
    expect(handshake.getAllByText(/mcp-method:/).length).toBeGreaterThan(0)
  })

  it('shows every body as a tree, the way the raw response always was', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })

    // The request that went out is one line of JSON. Finding a field in it is
    // the job, so it gets the same foldable tree the response always had.
    const model = within(card(/Turn 1 · model/))
    expect(model.getByText('messages')).toBeInTheDocument()
    // A branch is a button, because folding it is the point.
    expect(model.getByRole('button', { name: /array · 0/ })).toBeInTheDocument()

    // Same for the JSON-RPC underneath a tool, in both directions.
    const protocol = within(card(/Turn 1 · tools\/call/))
    expect(protocol.getByText('method')).toBeInTheDocument()
    expect(protocol.getByText('"tools/call"')).toBeInTheDocument()
    expect(protocol.getByText('temp')).toBeInTheDocument()
  })

  it('shows the request, the decode and the response of the model call', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })
    const model = within(card(/Turn 1 · model/))

    // The request, credentials already masked, with its `curl` equivalent.
    expect(model.getByText(/authorization: \*\*\*/)).toBeInTheDocument()
    expect(model.getByRole('button', { name: 'Copy as curl' })).toBeInTheDocument()

    // The decode: which configured path resolved which field.
    expect(model.getByText('$.choices[0].message.content')).toBeInTheDocument()

    // And the response the decoder read it out of.
    expect(model.getByText(/finish: tool_calls/)).toBeInTheDocument()
  })

  it('shows the arguments, the schema check and the result of the tool call', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    })
    const tool = within(card(/Turn 1 · get_weather/))

    expect(tool.getByText('city')).toBeInTheDocument()
    expect(tool.getByText('"Paris"')).toBeInTheDocument()
    expect(tool.getByText(/arguments match the schema/i)).toBeInTheDocument()
    expect(tool.getByText('temp')).toBeInTheDocument()
    expect(tool.getByText('21')).toBeInTheDocument()
    // Whether it really ran is the first thing the card says.
    expect(tool.getByText(/called for real/)).toBeInTheDocument()
  })

  it('folds every exchange away and back', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })

    await user.click(screen.getByRole('button', { name: 'Collapse all' }))
    expect(screen.queryByText(/arguments match the schema/i)).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Expand all' }))
    expect(screen.getByText(/arguments match the schema/i)).toBeInTheDocument()
  })

  it('keeps the traffic of every turn, so two turns can be compared', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/agent': agentStream([answerTurn(200)]),
      }),
    )
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(within(panel('Traffic')).getByText('1 exchange')).toBeInTheDocument()
    })

    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(within(panel('Traffic')).getByText('2 exchanges')).toBeInTheDocument()
    })

    // And it can be emptied on purpose, which is not the same thing as being
    // emptied for you every time you press Send.
    await user.click(screen.getByRole('button', { name: 'Clear' }))
    expect(screen.getByText(/Nothing on the wire yet/)).toBeInTheDocument()
  })
})

describe('browser login', () => {
  /** The signed-in half of the auth listing. */
  const SIGNED_IN = {
    ...AUTH,
    providers: AUTH.providers.map((provider) =>
      provider.name === 'me'
        ? {
            ...provider,
            session: {
              subject: 'gleroy',
              scope: 'openid profile',
              expiresInS: 240,
              canRefresh: true,
            },
          }
        : provider,
    ),
  }

  it('offers a sign-in button only where the profile calls as a human', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /^chat/ }))
    expect(screen.queryByRole('button', { name: 'Sign in' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /as-me/ }))
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeInTheDocument()
  })

  it('sends the identity provider a callback built from the page URL, not the server', async () => {
    const popup = { location: { href: '' }, closed: false, close: vi.fn() }
    vi.stubGlobal(
      'open',
      vi.fn(() => popup),
    )

    const seen: { url: string; body: unknown }[] = []
    let signedIn = false
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input)
        seen.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null })

        let payload: unknown = PROFILES
        if (url.endsWith('/login')) {
          payload = {
            authorizationUrl: 'https://idp.example/authorize?state=abc',
            redirectUri: `${document.baseURI}auth/callback`,
            state: 'abc',
          }
          signedIn = true
        } else if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = signedIn ? SIGNED_IN : AUTH
        }

        return Promise.resolve(
          new Response(JSON.stringify(payload), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /as-me/ }))
    await user.click(screen.getByRole('button', { name: 'Sign in' }))

    const login = await waitFor(() => {
      const found = seen.find((entry) => entry.url.endsWith('/api/auth/me/login'))
      expect(found).toBeDefined()
      return found
    })

    // The whole point: the callback follows the browser. Under a notebook proxy
    // this is the proxied URL, which the server cannot work out on its own.
    expect(login?.body).toEqual({
      redirectUri: new URL('auth/callback', document.baseURI).toString(),
    })
    // And the popup was pointed at the identity provider rather than navigated to.
    expect(popup.location.href).toBe('https://idp.example/authorize?state=abc')

    expect(await screen.findByText('gleroy', undefined, { timeout: 4000 })).toBeInTheDocument()
  })

  it('shows why a login failed and offers to force a fresh prompt', async () => {
    const withError = {
      ...AUTH,
      providers: AUTH.providers.map((provider) =>
        provider.name === 'me'
          ? { ...provider, lastError: 'auth `me`: the identity provider refused the login' }
          : provider,
      ),
    }

    const prompts: unknown[] = []
    vi.stubGlobal(
      'open',
      vi.fn(() => ({ location: { href: '' }, closed: false, close: vi.fn() })),
    )
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input)
        let payload: unknown = PROFILES
        if (url.endsWith('/login')) {
          prompts.push(init?.body ? JSON.parse(String(init.body)).prompt : undefined)
          payload = {
            authorizationUrl: 'https://idp.example/authorize',
            redirectUri: 'x',
            state: 's',
          }
        } else if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = withError
        }
        return Promise.resolve(
          new Response(JSON.stringify(payload), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /as-me/ }))

    // The reason survives the tab that closed, which is the whole point.
    expect(screen.getByText(/refused the login/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /asking for credentials/ }))
    await waitFor(() => expect(prompts).toContain('login'))
  })

  it('signs in from an MCP row without touching the model credential', async () => {
    const popup = { location: { href: '' }, closed: false, close: vi.fn() }
    vi.stubGlobal(
      'open',
      vi.fn(() => popup),
    )

    const logins: string[] = []
    let signedIn = false
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        let payload: unknown = PROFILES
        if (url.endsWith('/login')) {
          logins.push(url)
          payload = {
            authorizationUrl: 'https://idp.example/authorize',
            redirectUri: 'x',
            state: 's',
          }
          signedIn = true
        } else if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = signedIn ? SIGNED_IN : AUTH
        }
        return Promise.resolve(
          new Response(JSON.stringify(payload), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    await user.click(
      within(screen.getByTestId('mcp-dev')).getByRole('button', { name: /Sign in to me/ }),
    )
    expect(logins.some((url) => url.endsWith('/api/auth/me/login'))).toBe(true)

    // The session lands, so the row stops asking for one.
    await waitFor(
      () =>
        expect(within(screen.getByTestId('mcp-dev')).queryByRole('button')).not.toBeInTheDocument(),
      { timeout: 4000 },
    )

    // And the model still calls as what the profile says. A server needing a
    // session is not an opinion about the model's identity.
    expect(within(screen.getByTestId('model-auth')).getByText('pasted')).toBeInTheDocument()
  })

  it('shows the session and lets you drop it', async () => {
    let signedIn = true
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        let payload: unknown = PROFILES
        if (url.endsWith('/logout')) {
          signedIn = false
          payload = { signedOut: true }
        } else if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = signedIn ? SIGNED_IN : AUTH
        }
        return Promise.resolve(
          new Response(JSON.stringify(payload), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /as-me/ }))
    expect(screen.getByText('gleroy')).toBeInTheDocument()
    expect(screen.getByText('expires in 4 min')).toBeInTheDocument()
    expect(screen.getByText(/granted: openid profile/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Sign out' }))

    expect(await screen.findByRole('button', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.queryByText('gleroy')).not.toBeInTheDocument()
  })
})

describe('conversation', () => {
  /** The message box, once the configuration has landed and it exists. */
  async function type(user: ReturnType<typeof userEvent.setup>, text: string) {
    const box = await screen.findByRole('textbox', { name: /message/i })
    await user.clear(box)
    await user.type(box, text)
  }

  it('carries the whole exchange into the next turn', async () => {
    const { fetchMock, sent } = recordingApi(['Paris is the capital.', 'About 2.1 million.'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'Capital of France?')
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // Turn one is just the question.
    expect(sent[0]?.messages).toEqual([{ role: 'user', content: 'Capital of France?' }])

    await type(user, 'And its population?')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    // Turn two carries what came before it — which is the whole feature, and
    // the reason `mire` needs no session of its own to hold a conversation.
    expect(sent[1]?.messages).toEqual([
      { role: 'user', content: 'Capital of France?' },
      { role: 'assistant', content: 'Paris is the capital.' },
      { role: 'user', content: 'And its population?' },
    ])
  })

  it('shows the question straight away and the answer beside it', async () => {
    const { fetchMock } = recordingApi(['Paris is the capital.'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'Capital of France?')
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    expect(conversation.getByText('Capital of France?')).toBeInTheDocument()
    await waitFor(() => {
      expect(conversation.getByText('Paris is the capital.')).toBeInTheDocument()
    })
  })

  it('empties the box so the next turn starts clean', async () => {
    const { fetchMock } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'hello')
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => expect(screen.getByRole('textbox', { name: /message/i })).toHaveValue(''))
  })

  it('sends on Enter and starts a line on Shift+Enter', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    const box = await screen.findByRole('textbox', { name: /message/i })
    await user.clear(box)
    await user.type(box, 'one{Shift>}{Enter}{/Shift}two')
    expect(box).toHaveValue('one\ntwo')

    await user.type(box, '{Enter}')
    await waitFor(() => expect(sent).toHaveLength(1))
    expect(sent[0]?.messages).toEqual([{ role: 'user', content: 'one\ntwo' }])
  })

  it('drops a turn that is removed, and sends what is left', async () => {
    const { fetchMock, sent } = recordingApi(['first answer', 'second answer'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // Removing the model's answer asks the far more interesting question:
    // does it still say that if it never said it the first time?
    await user.click(await screen.findByRole('button', { name: 'Remove turn 2' }))

    await type(user, 'two')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]?.messages).toEqual([
      { role: 'user', content: 'one' },
      { role: 'user', content: 'two' },
    ])
  })

  it('starts over when the conversation is reset', async () => {
    const { fetchMock, sent } = recordingApi(['pong', 'pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))
    expect(within(panel('Conversation')).getByText('one')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'New conversation' }))
    expect(within(panel('Conversation')).queryByText('one')).not.toBeInTheDocument()
    expect(screen.getByText(/Nothing said yet/)).toBeInTheDocument()

    await type(user, 'two')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]?.messages).toEqual([{ role: 'user', content: 'two' }])
  })

  it('never offers a conversation for an embedding profile', async () => {
    const { fetchMock } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /embed/ }))
    // There is no such thing as a second turn of an embedding.
    expect(screen.queryByRole('heading', { name: 'Conversation' })).not.toBeInTheDocument()
  })
})
