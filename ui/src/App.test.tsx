import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { App } from './App'
import { describeStop } from './components/AgentPanel'
import { statusTone } from './components/ResponsePanel'

const PROFILES = {
  profiles: [
    {
      name: 'chat',
      kind: 'chat',
      url: 'https://models.internal/v1/chat/completions',
      auth: null,
      source: '/tmp/chat.yaml',
      hasDecode: true,
    },
    {
      name: 'embed',
      kind: 'embedding',
      url: 'https://models.internal/v1/embeddings',
      auth: null,
      source: '/tmp/embed.yaml',
      hasDecode: true,
    },
  ],
  issues: [],
}

const AUTH = {
  providers: [
    { name: 'anonymous', kind: 'anonymous', needsValue: false, needsLogin: false },
    { name: 'pasted', kind: 'token', needsValue: true, needsLogin: false },
    { name: 'me', kind: 'oidc_browser', needsValue: false, needsLogin: true },
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

beforeEach(() => {
  vi.stubGlobal('fetch', mockApi({ 'api/profiles': PROFILES, 'api/auth': AUTH }))
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
  it('lists the profiles and the auth providers', async () => {
    render(<App />)

    expect(await screen.findByRole('button', { name: /chat/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /embed/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /anonymous/ })).toBeInTheDocument()
  })

  it('prompts for a credential only when the provider needs one', async () => {
    const user = userEvent.setup()
    render(<App />)

    await screen.findByRole('button', { name: /anonymous/ })
    expect(screen.queryByPlaceholderText('paste the credential')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /pasted/ }))
    expect(screen.getByPlaceholderText('paste the credential')).toBeInTheDocument()
  })

  it('shows an expected 401 as a pass rather than a failure', async () => {
    const user = userEvent.setup()
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/agent': agentStream([answerTurn(401, null)]),
      }),
    )
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      // Scoped to the paragraph, and to wording the auth selector's own hint
      // does not share: a bare regex matches ancestors too.
      expect(
        screen.getByText(/that is a pass, not a failure/i, { selector: 'p' }),
      ).toBeInTheDocument()
    })
  })

  it('renders the decoded content and the curl equivalent', async () => {
    const user = userEvent.setup()
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/agent': agentStream([answerTurn(200)]),
      }),
    )
    render(<App />)

    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // Scoped: the answer also joins the conversation, which is a different
    // panel making a different claim about the same text.
    await waitFor(() => {
      expect(within(panel('Response')).getByText('pong')).toBeInTheDocument()
    })
    expect(screen.getByRole('button', { name: 'Copy as curl' })).toBeInTheDocument()
    // The masked credential is what the UI was handed, and what it shows.
    expect(screen.getByText(/authorization: \*\*\*/)).toBeInTheDocument()
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

  it('shows the text as it streams, then the timings once it is done', async () => {
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
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url.endsWith('api/profiles')) {
          return Promise.resolve(Response.json(PROFILES))
        }
        if (url.endsWith('api/auth')) {
          return Promise.resolve(Response.json(AUTH))
        }
        return Promise.resolve(
          new Response(stream, { status: 200, headers: { 'content-type': 'text/event-stream' } }),
        )
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Stream' }))

    // The deltas add up in the live panel — scoped to it, because once `done`
    // lands the decoded answer says the same word one panel further down.
    await waitFor(() => {
      expect(within(panel('Streaming')).getByText('pong')).toBeInTheDocument()
    })

    // And the numbers a streaming test is actually for.
    await waitFor(() => {
      expect(screen.getByText(/first token 40 ms/)).toBeInTheDocument()
    })
    expect(screen.getByText('3 chunks')).toBeInTheDocument()
    expect(screen.getByText(/ended cleanly/)).toBeInTheDocument()
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
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url.endsWith('api/profiles')) {
          return Promise.resolve(Response.json(PROFILES))
        }
        if (url.endsWith('api/auth')) {
          return Promise.resolve(Response.json(AUTH))
        }
        return Promise.resolve(
          new Response(stream, { status: 200, headers: { 'content-type': 'text/event-stream' } }),
        )
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Stream' }))

    await waitFor(() => {
      expect(screen.getByText(/stopped without ending/i)).toBeInTheDocument()
    })
    // What arrived is still shown: a truncated answer is the finding.
    expect(within(panel('Response')).getByText('half a sen')).toBeInTheDocument()
  })

  it('renders each streamed turn as a card and the verdict at the end', async () => {
    const user = userEvent.setup()
    const stream = [
      'event: turn',
      `data: ${JSON.stringify({ event: 'turn', ...turnFixture() })}`,
      '',
      'event: done',
      `data: ${JSON.stringify({ event: 'done', ...traceFixture() })}`,
      '',
      '',
    ].join('\n')

    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url.endsWith('api/profiles')) {
          return Promise.resolve(Response.json(PROFILES))
        }
        if (url.endsWith('api/auth')) {
          return Promise.resolve(Response.json(AUTH))
        }
        return Promise.resolve(
          new Response(stream, {
            status: 200,
            headers: { 'content-type': 'text/event-stream' },
          }),
        )
      }),
    )

    render(<App />)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1/ })).toBeInTheDocument()
    })
    expect(screen.getByText(/asked for no more tools/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Export trace' })).toBeInTheDocument()

    // A closed card still says what the turn did; the detail is behind the click.
    // Scoped to the trace: the conversation panel names the same tool call.
    expect(within(panel('Agent')).getByText(/get_weather/)).toBeInTheDocument()
    expect(screen.queryByText(/arguments match the schema/i)).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /Turn 1/ }))
    expect(screen.getByText(/arguments match the schema/i)).toBeInTheDocument()
    expect(screen.getByText(/"temp": 21/)).toBeInTheDocument()
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
    decision: { decision: 'continue', tools: 1 },
  }
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

  it('offers a sign-in button only for a provider that needs one', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /anonymous/ }))
    expect(screen.queryByRole('button', { name: 'Sign in' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /^me/ }))
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

    await user.click(await screen.findByRole('button', { name: /^me/ }))
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

    await user.click(await screen.findByRole('button', { name: /^me/ }))

    // The reason survives the tab that closed, which is the whole point.
    expect(screen.getByText(/refused the login/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /asking for credentials/ }))
    await waitFor(() => expect(prompts).toContain('login'))
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

    await user.click(await screen.findByRole('button', { name: /^me/ }))
    expect(screen.getByText('gleroy')).toBeInTheDocument()
    expect(screen.getByText('expires in 4 min')).toBeInTheDocument()
    expect(screen.getByText(/granted: openid profile/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Sign out' }))

    expect(await screen.findByRole('button', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.queryByText('gleroy')).not.toBeInTheDocument()
  })
})

describe('conversation', () => {
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

  it('empties the box on success so the next turn starts clean', async () => {
    const { fetchMock } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'hello')
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => expect(screen.getByRole('textbox', { name: /message/i })).toHaveValue(''))
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
    await user.click(screen.getByRole('button', { name: 'Remove turn 2' }))

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
    expect(screen.getByRole('heading', { name: 'Conversation' })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'New conversation' }))
    expect(screen.queryByRole('heading', { name: 'Conversation' })).not.toBeInTheDocument()

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
