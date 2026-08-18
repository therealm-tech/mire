import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { App } from './App'
import type { HookRecord } from './api'
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
  // What this build speaks, newest first, as the server reports it. The UI keeps
  // no list of its own.
  revisions: ['2026-07-28', '2025-11-25', '2025-06-18', '2025-03-26'],
  issues: [],
}

/**
 * What `prompts.yaml` declares. One that loads and one entry that did not, so
 * the picker and the complaint next to it are both exercised by the default
 * fixture rather than by a special-case mock.
 */
const PROMPTS = {
  prompts: [
    { name: 'smallest signal', text: 'ping' },
    { name: 'two lines', text: 'one\ntwo' },
  ],
  issues: [
    {
      file: '/tmp/prompts.yaml',
      message: 'prompt `hollow`: text: a prompt with no text puts nothing in the box',
      line: null,
      column: null,
    },
  ],
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

/**
 * A stream that opens and then says nothing until the caller gives up.
 *
 * `fetch` wires its signal to the body, so a real abort surfaces as the reader
 * rejecting; a mock that resolved and then sat there would test a hang rather
 * than a cancellation.
 */
function stalled(signal: AbortSignal | null | undefined): Response {
  const body = new ReadableStream({
    start(controller) {
      signal?.addEventListener('abort', () => {
        controller.error(new DOMException('The operation was aborted.', 'AbortError'))
      })
    },
  })
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  })
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

/**
 * Unfolds one exchange and hands back its card.
 *
 * Cards arrive folded, so every assertion about a request, a decode or a body
 * has to open the card first — which is also the gesture a reader makes, and a
 * test that skipped it would be asserting against a panel nobody sees.
 */
async function openCard(
  user: ReturnType<typeof userEvent.setup>,
  label: RegExp,
): Promise<HTMLElement> {
  await user.click(screen.getByRole('button', { name: label }))
  return card(label)
}

/**
 * Picks a mode in the composer, which is what decides where **Send** goes.
 *
 * **Agent** is the default and the loop; **Chat** is one streamed turn that
 * answers no tools. Said out loud in every test that depends on it, default or
 * not: a test that reads as "the loop" because nobody has touched the dropdown
 * is a test that quietly changes meaning the day the default does.
 */
async function agentMode(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.selectOptions(await screen.findByLabelText('mode'), 'agent')
}

async function chatMode(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.selectOptions(await screen.findByLabelText('mode'), 'chat')
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
  // `_init` is never read here, and is in the signature so that a test can look
  // at what a route was actually sent — a `FormData` body has no other witness.
  return vi.fn((input: RequestInfo | URL, _init?: RequestInit) => {
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
    if (url.endsWith('api/prompts')) {
      return Promise.resolve(Response.json(PROMPTS))
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

/**
 * Unfolds the auth detail.
 *
 * It is shut unless something needs acting on, so a test that reads the panel
 * has to open it — the same click the page asks for. Already-open is not a
 * failure: a profile blocked on a field opens it on arrival.
 */
async function openAuth(user: ReturnType<typeof userEvent.setup>) {
  const toggle = await screen.findByRole('button', { name: /^(Auth|Hide auth)$/ })
  if (toggle.textContent === 'Auth') {
    await user.click(toggle)
  }
}

beforeEach(() => {
  vi.stubGlobal(
    'fetch',
    mockApi({ 'api/profiles': PROFILES, 'api/auth': AUTH, 'api/mcp': MCP, 'api/prompts': PROMPTS }),
  )
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

    await openAuth(user)

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
        'api/prompts': PROMPTS,
        'api/agent': agentStream([answerTurn(401, null)]),
      }),
    )
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })
    // Scoped to the paragraph, and to wording the auth panel's own hint does
    // not share: a bare regex matches ancestors too.
    const model = within(await openCard(user, /Turn 1 · model/))
    expect(model.getByText(/that is a pass, not a failure/i, { selector: 'p' })).toBeInTheDocument()
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

    await agentMode(user)
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
    await openAuth(user)

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

  it('takes the servers out of the picture on a chat, which calls no tool', async () => {
    const user = userEvent.setup()
    render(<App />)

    // Agent mode: three servers, their identities, and the revision to reach
    // them in — all of it about the run that is about to happen.
    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    expect(screen.getByRole('heading', { name: 'MCP servers' })).toBeInTheDocument()
    expect(screen.getByLabelText('Protocol')).toBeInTheDocument()
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent('3 MCP servers')
    expect(screen.getByText(/answer 409 until/)).toBeInTheDocument()

    // Chat mode: one turn, which sets nothing up and calls nothing. None of it
    // is this run's business — least of all a 409 it cannot be given.
    await chatMode(user)
    expect(screen.queryByRole('heading', { name: 'MCP servers' })).not.toBeInTheDocument()
    expect(screen.queryByLabelText('Protocol')).not.toBeInTheDocument()
    expect(screen.getByLabelText('What the next call will do')).not.toHaveTextContent('MCP server')
    expect(screen.queryByText(/answer 409 until/)).not.toBeInTheDocument()
    expect(screen.queryByText(/declared in no/)).not.toBeInTheDocument()
  })

  it('offers the revisions the server says it speaks, and negotiates by default', async () => {
    const user = userEvent.setup()
    render(<App />)

    // Nothing to speak to, nothing to choose.
    await screen.findByRole('button', { name: /^chat/ })
    expect(screen.queryByLabelText('Protocol')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /guarded/ }))
    const protocol = screen.getByLabelText('Protocol')

    // The list comes from `GET /api/mcp` rather than from a copy kept here, so
    // it cannot offer a revision this build was never taught.
    expect(
      within(protocol)
        .getAllByRole('option')
        .map((option) => option.textContent),
    ).toEqual(['auto', '2026-07-28', '2025-11-25', '2025-06-18', '2025-03-26'])
    expect(protocol).toHaveValue('')
    expect(screen.getByText(/Negotiated per server/)).toBeInTheDocument()
  })

  it('states the chosen revision on the wire and says nothing on auto', async () => {
    const { fetchMock, sent } = recordingApi(['pong', 'pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    // First, because a stream speaks to no server: the selector is inert until
    // the run is one that can call a tool.
    await agentMode(user)
    await user.selectOptions(screen.getByLabelText('Protocol'), '2025-03-26')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    expect(sent[0]?.mcpProtocol).toBe('2025-03-26')

    // Back to auto, and the field goes away entirely: its absence is what tells
    // the server to settle the revision itself.
    await user.selectOptions(screen.getByLabelText('Protocol'), '')
    await user.type(screen.getByRole('textbox', { name: /message/i }), 'again')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]).not.toHaveProperty('mcpProtocol')
  })

  it('takes a server out of the run when it is switched off, and puts it back', async () => {
    const user = userEvent.setup()
    render(<App />)

    // Nothing to switch off on a profile that names no server.
    await screen.findByRole('button', { name: /^chat/ })
    expect(screen.queryByRole('checkbox', { name: 'dev' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /guarded/ }))
    await agentMode(user)

    // Every server the profile names, all on: the file's word, unedited.
    expect(
      within(screen.getByRole('group', { name: 'Servers' }))
        .getAllByRole('checkbox')
        .map((box) => (box as HTMLInputElement).checked),
    ).toEqual([true, true, true])
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent('3 MCP servers')

    // `dev` off: out of the bar, out of the auth panel — and with it the `409`
    // its unsigned-in provider was promising, because this run never asks.
    await user.click(screen.getByRole('checkbox', { name: 'dev' }))
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent('2 MCP servers')
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent(
      /Switched off for this run: dev/,
    )
    expect(screen.queryByTestId('mcp-dev')).not.toBeInTheDocument()
    expect(screen.getByTestId('mcp-keyed')).toBeInTheDocument()
    expect(screen.queryByText(/answer 409 until/)).not.toBeInTheDocument()

    // And back, because a switch that only goes one way is a trap.
    await user.click(screen.getByRole('checkbox', { name: 'dev' }))
    expect(screen.getByTestId('mcp-dev')).toBeInTheDocument()
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent('3 MCP servers')
  })

  it('names the servers on the wire only once one is off', async () => {
    const { fetchMock, sent } = recordingApi(['pong', 'pong', 'pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    await agentMode(user)
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // All on: the profile already says which, and a copy alongside it is a
    // second thing that can disagree with the file.
    expect(sent[0]).not.toHaveProperty('mcpServers')

    await user.click(screen.getByRole('checkbox', { name: 'ghost' }))
    await user.type(screen.getByRole('textbox', { name: /message/i }), 'again')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]?.mcpServers).toEqual(['dev', 'keyed'])

    // Every one off is a list of none, not a silence: saying nothing would be
    // asking for all three.
    await user.click(screen.getByRole('checkbox', { name: 'dev' }))
    await user.click(screen.getByRole('checkbox', { name: 'keyed' }))
    await user.type(screen.getByRole('textbox', { name: /message/i }), 'and again')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(3))

    expect(sent[2]?.mcpServers).toEqual([])
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

    // Two named runs rather than a mechanism to switch on, and it starts on the
    // loop, which is the mode this tool is about.
    const picker = await screen.findByLabelText('mode')
    expect(
      Array.from(picker.querySelectorAll('option')).map((option) => option.textContent),
    ).toEqual(['Agent', 'Chat'])
    expect(picker).toHaveValue('agent')
    expect(screen.getByLabelText(/max turns/)).toBeEnabled()

    // A chat is one turn, so the cap on the second one is shown as inert rather
    // than quietly ignored.
    await chatMode(user)
    expect(screen.getByLabelText(/max turns/)).toBeDisabled()

    // There is no second turn of an embedding, so there is nothing to loop.
    await user.click(screen.getByRole('button', { name: /embed/ }))
    expect(screen.queryByLabelText('mode')).not.toBeInTheDocument()
    expect(screen.queryByLabelText(/max turns/)).not.toBeInTheDocument()
  })

  it('streams on Chat and loops on Agent, and only ever offers one Send', async () => {
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
        if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
        }
        if (url.endsWith('api/agent')) {
          return Promise.resolve(sse(agentStream([answerTurn(200)])))
        }
        return Promise.resolve(sse(''))
      }),
    )

    render(<App />)

    // Nothing is chosen, nothing is typed but the draft that was already there:
    // pressing the one button runs the loop.
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(urls.some((url) => url.endsWith('api/agent'))).toBe(true))
    expect(urls.some((url) => url.endsWith('api/call/stream'))).toBe(false)

    // Picking **Chat** is what asks for the stream, and it is the only thing
    // that does: there is no second button to press.
    await chatMode(user)
    await user.type(screen.getByRole('textbox', { name: /message/i }), 'again')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(urls.some((url) => url.endsWith('api/call/stream'))).toBe(true))

    expect(screen.getAllByRole('button', { name: 'Send' })).toHaveLength(1)
    expect(screen.queryByRole('button', { name: 'Stream' })).not.toBeInTheDocument()
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
        if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
        }
        if (url.endsWith('api/agent')) {
          return Promise.resolve(sse(agentStream([answerTurn(200)])))
        }
        return Promise.resolve(Response.json(completion(200)))
      }),
    )

    render(<App />)

    // There is one send button, and for a chat profile it is the loop.
    await agentMode(user)
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
        'api/prompts': PROMPTS,
        'api/call/stream': stream,
      }),
    )

    render(<App />)
    await chatMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // The answer joins the conversation as a turn rather than staying in a
    // panel of its own: the deltas were a preview of this bubble.
    await waitFor(() => {
      expect(within(panel('Conversation')).getByText('pong')).toBeInTheDocument()
    })

    // And the numbers a streaming test is actually for, one panel down: the
    // headline on the folded card, the counters once it is opened.
    expect(within(panel('Traffic')).getByText(/first token 40 ms/)).toBeInTheDocument()
    const model = within(await openCard(user, /Call · model/))
    expect(model.getByText('3 chunks')).toBeInTheDocument()
    expect(model.getByText(/ended cleanly/)).toBeInTheDocument()
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
        'api/prompts': PROMPTS,
        'api/call/stream': stream,
      }),
    )

    render(<App />)
    await chatMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // What arrived is still shown: a truncated answer is the finding.
    await waitFor(() => {
      expect(within(panel('Conversation')).getByText('half a sen')).toBeInTheDocument()
    })
    const model = within(await openCard(user, /Call · model/))
    expect(model.getByText(/stopped without ending/i)).toBeInTheDocument()
  })

  it('shows the tools a run called in the transcript and the verdict it ended on', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // What the run did on its own sits between the question and the answer,
    // where it happened, rather than in a panel somewhere else.
    const conversation = within(panel('Conversation'))
    await waitFor(() => {
      expect(conversation.getByText('get_weather')).toBeInTheDocument()
    })
    expect(conversation.getByText(/via/)).toHaveTextContent('via weather · 42 ms')
    expect(conversation.getByText(/asked for no more tools/i)).toBeInTheDocument()
  })

  it('shows a hook in the transcript, on the side of the call it fired on', async () => {
    const user = userEvent.setup()
    const turn = turnFixture()
    // A gate in front of the call and an audit behind it — the order they fired
    // in, which is the only order they can be read in.
    turn.hooks = [
      {
        server: 'weather',
        hook: 'gate',
        phase: 'before',
        tool: 'get_weather',
        action: 'http',
        url: 'https://policy.internal/decide',
        method: 'POST',
        headers: { 'x-api-key': '***' },
        request: '{"phase":"before","tool":"get_weather"}',
        files: [],
        status: 204,
        response: '',
        latencyMs: 8,
        stoppedTheCall: false,
      },
      {
        server: 'weather',
        hook: 'audit',
        phase: 'after',
        tool: 'get_weather',
        action: 'http',
        url: 'https://audit.internal/tool-calls',
        method: 'POST',
        headers: {},
        request: '{"phase":"after","tool":"get_weather"}',
        files: [],
        status: 200,
        response: 'ok',
        latencyMs: 12,
        stoppedTheCall: false,
      },
    ]
    vi.stubGlobal('fetch', toolRunApi([turn]))

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    await waitFor(() => {
      expect(conversation.getByRole('button', { name: 'gate' })).toBeInTheDocument()
    })
    expect(conversation.getByText(/fired before/)).toBeInTheDocument()
    expect(conversation.getByText(/fired after/)).toBeInTheDocument()

    // Around the call, not in a list of their own: the gate is above the tool it
    // let through and the audit is below it.
    const gate = conversation.getByRole('button', { name: 'gate' })
    const tool = conversation.getByRole('button', { name: 'get_weather' })
    const audit = conversation.getByRole('button', { name: 'audit' })
    expect(gate.compareDocumentPosition(tool) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
    expect(tool.compareDocumentPosition(audit) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()

    // And the row is a way to its own card, like every other row here.
    await user.click(gate)
    expect(within(panel('Traffic')).getByText(/x-api-key: \*\*\*/)).toBeInTheDocument()
  })

  it('says a hook sat a call out, and what it was waiting for', async () => {
    const user = userEvent.setup()
    const turn = turnFixture()
    // `when_defined: [session]` on a run where no call has opened one yet: the
    // hook is doing what it was told, and the tool call went ahead untouched.
    turn.hooks = [
      {
        server: 'weather',
        hook: 'session-audit',
        phase: 'after',
        tool: 'get_weather',
        action: 'http',
        url: 'https://audit.internal/sessions/{{ vars.session }}/tool-calls',
        method: 'POST',
        headers: {},
        request: '',
        files: [],
        status: 0,
        response: '',
        latencyMs: 0,
        stoppedTheCall: false,
        skipped: 'session',
      },
    ]
    vi.stubGlobal('fetch', toolRunApi([turn]))

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    await waitFor(() => {
      expect(conversation.getByRole('button', { name: 'session-audit' })).toBeInTheDocument()
    })
    expect(conversation.getByText(/sat out/)).toBeInTheDocument()
    expect(conversation.getByText(/waiting for/)).toBeInTheDocument()

    // Not firing is not failing: no status in red, and the run reads as clean.
    const traffic = within(panel('Traffic'))
    expect(traffic.queryByText('never answered')).not.toBeInTheDocument()
    expect(traffic.getByRole('button', { name: 'Nothing failed' })).toBeDisabled()

    const hook = within(await openCard(user, /Turn 1 · session-audit \(after\)/))
    expect(hook.getByText('did not fire')).toBeInTheDocument()
    expect(hook.getByText(/Nothing was sent/)).toBeInTheDocument()
    // The address it would have rendered, template and all: on the folded
    // summary line, and on the request line under it.
    expect(hook.getAllByText(/{{ vars.session }}/)).toHaveLength(2)
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
        'api/prompts': PROMPTS,
        'api/agent': [
          'event: turn',
          `data: ${JSON.stringify({ event: 'turn', ...refused })}`,
          '',
          '',
        ].join('\n'),
      }),
    )

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // The card says `400` while folded; opening it opens the body with it,
    // because a red status with nothing under it is the whole problem this
    // answers.
    await waitFor(() => {
      expect(within(panel('Traffic')).getByText('400')).toBeInTheDocument()
    })
    const model = within(await openCard(user, /Turn 1 · model/))
    expect(model.getByText(/maximum context length/)).toBeInTheDocument()
  })

  it('reads the error out of a body whose status called it fine', async () => {
    const user = userEvent.setup()
    const swallowed = {
      ...turnFixture(),
      call: {
        ...completion(200),
        response: {
          ...completion(200).response,
          decoded: {
            kind: 'completion',
            content: null,
            toolCalls: [],
            finishReason: null,
            usage: null,
          },
          error: {
            message: 'no capacity upstream',
            type: 'service_unavailable',
            code: '503',
            raw: { detail: 'no capacity upstream' },
          },
        },
      },
      tools: [],
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
        'api/prompts': PROMPTS,
        'api/agent': [
          'event: turn',
          `data: ${JSON.stringify({ event: 'turn', ...swallowed })}`,
          '',
          '',
        ].join('\n'),
      }),
    )

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const traffic = within(panel('Traffic'))
    // The status said `200`, so the badge on the folded card is the only thing
    // that would ever have told you otherwise.
    await waitFor(() => {
      expect(traffic.getByText(/error in the body/)).toBeInTheDocument()
    })

    const model = within(await openCard(user, /Turn 1 · model/))
    expect(model.getByText('no capacity upstream')).toBeInTheDocument()
    expect(model.getByText('service_unavailable')).toBeInTheDocument()
    // And the profile is not the one at fault, so it is not accused of it.
    expect(model.queryByText(/No configured path resolved the content/)).not.toBeInTheDocument()
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
        // Typed, not inferred: an empty literal infers `never[]`, and a variant
        // of this fixture with a schema error in it would not be assignable.
        schemaErrors: [] as string[],
        result: '{"temp": 21}',
      },
    ],
    mcp: [mcpExchange('tools/call', '{"jsonrpc":"2.0","id":1,"result":{"temp":21}}')],
    // Typed for the same reason `schemaErrors` is: most turns fire no hook, and
    // the one variant that does has to be assignable to this.
    hooks: [] as HookRecord[],
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
function toolRunApi(turns: ReturnType<typeof turnFixture>[] = [turnFixture()]) {
  const stream = [
    ...setupEvent(),
    ...turns.flatMap((turn) => [
      'event: turn',
      `data: ${JSON.stringify({ event: 'turn', ...turn })}`,
      '',
    ]),
    'event: done',
    `data: ${JSON.stringify({ event: 'done', ...traceFixture() })}`,
    '',
    '',
  ].join('\n')

  return mockApi({
    'api/profiles': PROFILES,
    'api/auth': AUTH,
    'api/mcp': MCP,
    'api/prompts': PROMPTS,
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

    await agentMode(user)
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

  it('gives a hook its own card, and says when one stopped the call', async () => {
    const turn = turnFixture()
    turn.hooks = [
      {
        server: 'weather',
        hook: 'gate',
        phase: 'before',
        tool: 'get_weather',
        action: 'http',
        url: 'https://policy.internal/decide',
        method: 'POST',
        headers: { 'x-api-key': '***' },
        request: '{"phase":"before","tool":"get_weather"}',
        files: [],
        status: 403,
        response: 'get_weather is not allowed here',
        latencyMs: 8,
        error: 'answered 403 Forbidden: get_weather is not allowed here',
        stoppedTheCall: true,
      },
    ]
    vi.stubGlobal('fetch', toolRunApi([turn]))
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // Its own card, not a line on the tool's: the tool card says the call did
    // not happen, and only this says who decided that.
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · gate \(before\)/ })).toBeInTheDocument()
    })
    const hook = within(await openCard(user, /Turn 1 · gate \(before\)/))
    expect(hook.getByText('stopped the call')).toBeInTheDocument()
    expect(hook.getByText('403')).toBeInTheDocument()
    // Twice over: the reason it failed, and the body it answered with.
    expect(hook.getAllByText(/not allowed here/)).toHaveLength(2)
    // The header travels by name; its value does not.
    expect(hook.getByText(/x-api-key: \*\*\*/)).toBeInTheDocument()
  })

  it('says why a hook never got an answer, and what that cost the call', async () => {
    const turn = turnFixture()
    // `status: 0` with `stoppedTheCall: false` is the `on_error: continue` case
    // the panel used to describe as a hook answering nothing, under an error
    // saying the request never arrived.
    turn.hooks = [
      {
        server: 'sandbox',
        hook: 'audit',
        phase: 'after',
        tool: 'run_code',
        action: 'http',
        url: 'http://sandbox-mcp.sandbox-api:8080/v1/inputs',
        method: 'POST',
        headers: {},
        request: '{"phase":"after","tool":"run_code"}',
        files: [],
        status: 0,
        response: '',
        latencyMs: 12,
        error:
          'could not reach the endpoint: dns error: failed to lookup address information: nodename nor servname provided',
        stoppedTheCall: false,
      },
    ]
    vi.stubGlobal('fetch', toolRunApi([turn]))
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const hook = within(await openCard(user, /Turn 1 · audit \(after\)/))
    expect(hook.getByText('failed, stepped over')).toBeInTheDocument()
    // The cause, which is the whole reason the URL alone was not enough.
    expect(hook.getByText(/failed to lookup address information/)).toBeInTheDocument()
    expect(hook.getByText(/The tool call was unaffected/)).toBeInTheDocument()
    expect(hook.getByText(/never reached an answer/)).toBeInTheDocument()
    // A request nobody answered is not a hook electing to say nothing.
    expect(hook.queryByText(/entitled to answer/)).not.toBeInTheDocument()
  })

  it('names the files a hook attached, and shows none of their bytes', async () => {
    const turn = turnFixture()
    turn.hooks = [
      {
        server: 'weather',
        hook: 'audit',
        phase: 'after',
        tool: 'get_weather',
        action: 'http',
        url: 'https://audit.internal/tool-calls',
        method: 'POST',
        headers: { 'content-type': 'multipart/form-data; boundary=abc' },
        // The `payload` part alone: the bytes went out beside it, as parts.
        request: '{"phase":"after","tool":"get_weather"}',
        files: [
          { id: 'aB3dE5gH7jK9', name: 'report.pdf', size: 2048, contentType: 'application/pdf' },
        ],
        status: 200,
        response: 'ok',
        latencyMs: 12,
        stoppedTheCall: false,
      },
    ]
    vi.stubGlobal('fetch', toolRunApi([turn]))
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · audit \(after\)/ })).toBeInTheDocument()
    })
    const hook = within(await openCard(user, /Turn 1 · audit \(after\)/))
    expect(hook.getByText('report.pdf')).toBeInTheDocument()
    // Sized the way every other file chip in this UI is sized.
    expect(hook.getByText(/application\/pdf · 2\.0 kB/)).toBeInTheDocument()
  })

  it('records what was said to the MCP server before the loop began', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    // No tool was called by these, so a tool listing could never show them —
    // and a server that refuses the handshake produces nothing else at all.
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Setup · initialize/ })).toBeInTheDocument()
    })
    expect(screen.getByRole('button', { name: /Setup · tools\/list/ })).toBeInTheDocument()
    // The tool call's own JSON-RPC belongs to the turn that made it.
    expect(screen.getByRole('button', { name: /Turn 1 · tools\/call/ })).toBeInTheDocument()

    const handshake = within(await openCard(user, /Setup · initialize/))
    expect(handshake.getByText('protocolVersion')).toBeInTheDocument()
    expect(handshake.getByText('"x"')).toBeInTheDocument()
    expect(handshake.getAllByText(/mcp-method:/).length).toBeGreaterThan(0)
  })

  it('shows every body as a tree, the way the raw response always was', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })

    // The request that went out is one line of JSON. Finding a field in it is
    // the job, so it gets the same foldable tree the response always had.
    const model = within(await openCard(user, /Turn 1 · model/))
    expect(model.getByText('messages')).toBeInTheDocument()
    // A branch is a button, because folding it is the point.
    expect(model.getByRole('button', { name: /array · 0/ })).toBeInTheDocument()

    // Same for the JSON-RPC underneath a tool, in both directions.
    const protocol = within(await openCard(user, /Turn 1 · tools\/call/))
    expect(protocol.getByText('method')).toBeInTheDocument()
    expect(protocol.getByText('"tools/call"')).toBeInTheDocument()
    expect(protocol.getByText('temp')).toBeInTheDocument()
  })

  it('shows the request, the decode and the response of the model call', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })
    const model = within(await openCard(user, /Turn 1 · model/))

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

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    })
    const tool = within(await openCard(user, /Turn 1 · get_weather/))

    expect(tool.getByText('city')).toBeInTheDocument()
    expect(tool.getByText('"Paris"')).toBeInTheDocument()
    expect(tool.getByText(/arguments match the schema/i)).toBeInTheDocument()
    expect(tool.getByText('temp')).toBeInTheDocument()
    expect(tool.getByText('21')).toBeInTheDocument()
    // It really ran, so the card names the server that answered rather than
    // claiming nothing was executed.
    expect(tool.getByText('weather')).toBeInTheDocument()
    expect(tool.queryByText(/simulated/)).not.toBeInTheDocument()
    // And nothing was captured, so there is no section claiming otherwise.
    expect(tool.queryByText('Captured')).not.toBeInTheDocument()
  })

  it('says what a call captured, and what the variable is worth', async () => {
    const turn = turnFixture()
    // The same run, with a capture rule that matched: the tool answered a
    // session, and a hook later in the run will render it into an address. What
    // that address comes out as is only explicable if this value is readable.
    const capturing = {
      ...turn,
      tools: turn.tools.map((tool) => ({ ...tool, captured: { session: 'abc-123' } })),
    }
    vi.stubGlobal('fetch', toolRunApi([capturing]))
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    })

    // The transcript names the variable without being opened at all.
    expect(within(panel('Conversation')).getByText('session')).toBeInTheDocument()

    // The card is where it is worth something.
    const tool = within(await openCard(user, /Turn 1 · get_weather/))
    expect(tool.getByText('Captured')).toBeInTheDocument()
    expect(tool.getByText('session')).toBeInTheDocument()
    expect(tool.getByText('"abc-123"')).toBeInTheDocument()
  })

  it('lands folded, and unfolds every exchange and back', async () => {
    vi.stubGlobal('fetch', toolRunApi())
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument()
    })

    // A run lands as a list of what happened, not as five open bodies: the
    // summary lines are there, and nothing underneath them is until asked for.
    expect(screen.queryByText(/arguments match the schema/i)).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Expand all' }))
    expect(screen.getByText(/arguments match the schema/i)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Collapse all' }))
    expect(screen.queryByText(/arguments match the schema/i)).not.toBeInTheDocument()

    // And one card on its own still opens, which is the ordinary gesture.
    await user.click(screen.getByRole('button', { name: /Turn 1 · get_weather/ }))
    expect(screen.getByText(/arguments match the schema/i)).toBeInTheDocument()
  })

  it('keeps the traffic of every turn, so two turns can be compared', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/prompts': PROMPTS,
        'api/agent': agentStream([answerTurn(200)]),
      }),
    )
    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => {
      expect(within(panel('Traffic')).getByText('1 exchange')).toBeInTheDocument()
    })

    await user.type(screen.getByRole('textbox', { name: /message/i }), 'again')
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
    await openAuth(user)
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
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
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
    await openAuth(user)
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
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
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
    await openAuth(user)

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
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
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

    // The session lands, so the row stops asking for one and says who it got
    // instead — with the way back out, since this row is the only place that
    // identity is on screen.
    const dev = within(screen.getByTestId('mcp-dev'))
    await waitFor(
      () => expect(dev.getByRole('button', { name: 'Sign out of me' })).toBeInTheDocument(),
      { timeout: 4000 },
    )
    expect(dev.queryByRole('button', { name: /Sign in to me/ })).not.toBeInTheDocument()
    expect(dev.getByText('gleroy')).toBeInTheDocument()

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
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
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
    await openAuth(user)
    expect(screen.getByText('gleroy')).toBeInTheDocument()
    expect(screen.getByText('expires in 4 min')).toBeInTheDocument()
    expect(screen.getByText(/granted: openid profile/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Sign out' }))

    expect(await screen.findByRole('button', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.queryByText('gleroy')).not.toBeInTheDocument()
  })

  it('drops a session from the MCP row that is the only place it shows', async () => {
    const logouts: string[] = []
    let signedIn = true
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        let payload: unknown = PROFILES
        if (url.endsWith('/logout')) {
          logouts.push(url)
          signedIn = false
          payload = { signedOut: true }
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
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

    // `guarded` calls the model as `pasted`, so `me` appears nowhere but here.
    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    const dev = within(screen.getByTestId('mcp-dev'))
    expect(dev.getByText('gleroy')).toBeInTheDocument()

    await user.click(dev.getByRole('button', { name: 'Sign out of me' }))

    expect(await dev.findByRole('button', { name: /Sign in to me/ })).toBeInTheDocument()
    expect(logouts.some((url) => url.endsWith('/api/auth/me/logout'))).toBe(true)
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
    await agentMode(user)
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
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    expect(conversation.getByText('Capital of France?')).toBeInTheDocument()
    await waitFor(() => {
      expect(conversation.getByText('Paris is the capital.')).toBeInTheDocument()
    })
  })

  it('reads the answer as markdown and the question as what was typed', async () => {
    const { fetchMock, sent } = recordingApi(['Use **`serde`**, not a hand-written parser.'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'Which crate for **json**?')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    await waitFor(() => expect(conversation.getByText('serde')).toBeInTheDocument())
    expect(conversation.getByText('serde').tagName).toBe('CODE')

    // A question is text somebody typed, so its asterisks are asterisks — and
    // what goes back on the wire is that text, not the render of it.
    expect(conversation.getByText('Which crate for **json**?')).toBeInTheDocument()
    expect(sent[0]?.messages).toEqual([{ role: 'user', content: 'Which crate for **json**?' }])
  })

  it('empties the box so the next turn starts clean', async () => {
    const { fetchMock } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'hello')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => expect(screen.getByRole('textbox', { name: /message/i })).toHaveValue(''))
  })

  it('sends on Enter and starts a line on Shift+Enter', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await agentMode(user)
    const box = await screen.findByRole('textbox', { name: /message/i })
    await user.clear(box)
    await user.type(box, 'one{Shift>}{Enter}{/Shift}two')
    expect(box).toHaveValue('one\ntwo')

    await user.type(box, '{Enter}')
    await waitFor(() => expect(sent).toHaveLength(1))
    expect(sent[0]?.messages).toEqual([{ role: 'user', content: 'one\ntwo' }])
  })

  it('refuses to send an empty box, so nothing goes out unseen', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // The box empties itself on send, and what is left cannot be sent again by
    // pressing the same button: that used to resend the whole conversation with
    // nothing on screen to say it had.
    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled()

    await user.type(screen.getByRole('textbox', { name: /message/i }), '{Enter}')
    expect(sent).toHaveLength(1)
  })

  it('asks the last answer again without it', async () => {
    const { fetchMock, sent } = recordingApi(['first answer', 'second answer'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    // Asking again without the answer is the far more interesting question:
    // does it still say that if it never said it the first time?
    await user.click(await screen.findByRole('button', { name: 'Retry turn 2' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]?.messages).toEqual([{ role: 'user', content: 'one' }])
    await waitFor(() => {
      expect(within(panel('Conversation')).getByText('second answer')).toBeInTheDocument()
    })
    expect(within(panel('Conversation')).queryByText('first answer')).not.toBeInTheDocument()
  })

  it('asks a turn that already worked again, and drops what followed it', async () => {
    const { fetchMock, sent } = recordingApi(['first answer', 'second answer', 'third answer'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    await type(user, 'two')
    await user.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(2))
    expect(sent[1]?.messages).toEqual([
      { role: 'user', content: 'one' },
      { role: 'assistant', content: 'first answer' },
      { role: 'user', content: 'two' },
    ])

    // The first answer landed fine, which used to be the reason it could never
    // be asked again. It is the run worth repeating: same question, nothing of
    // what came after it on the wire.
    const again = screen.getByRole('button', { name: 'Retry turn 2' })
    // And it says what it costs before it is pressed, rather than after.
    expect(again).toHaveAttribute('title', expect.stringContaining('The 2 turns after it go.'))

    await user.click(again)
    await waitFor(() => expect(sent).toHaveLength(3))
    expect(sent[2]?.messages).toEqual([{ role: 'user', content: 'one' }])

    const conversation = within(panel('Conversation'))
    await waitFor(() => expect(conversation.getByText('third answer')).toBeInTheDocument())
    // The follow-up and its answer are gone with the branch, and the question
    // being re-asked is still there exactly once.
    expect(conversation.queryByText('two')).not.toBeInTheDocument()
    expect(conversation.queryByText('second answer')).not.toBeInTheDocument()
    expect(conversation.queryByText('first answer')).not.toBeInTheDocument()
    expect(conversation.getAllByText('one')).toHaveLength(1)
  })

  it('drops the tool calls of the answer it replaces, and keeps them on the wire', async () => {
    vi.stubGlobal('fetch', toolRunApi())

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'weather?')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    await waitFor(() => {
      expect(conversation.getByRole('button', { name: 'get_weather' })).toBeInTheDocument()
    })

    await user.click(conversation.getByRole('button', { name: 'Retry turn 2' }))

    // The run happens again, so the row is back — once. The rows belonging to
    // the answer being replaced left with it, rather than stacking above the new
    // ones and reading as a tool that was called twice.
    await waitFor(() => {
      expect(conversation.getAllByRole('button', { name: 'get_weather' })).toHaveLength(1)
    })

    // Nothing was actually lost: both runs really did go out, and the traffic
    // below is the record of what this tab has done, not of what it still shows.
    expect(screen.getAllByRole('button', { name: /Turn 1 · get_weather/ })).toHaveLength(2)
  })

  it('sends the last question again when the call it made failed', async () => {
    const sent: Array<Record<string, unknown>> = []
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input)
        if (url.endsWith('api/profiles')) return Promise.resolve(Response.json(PROFILES))
        if (url.endsWith('api/auth')) return Promise.resolve(Response.json(AUTH))
        if (url.endsWith('api/prompts')) return Promise.resolve(Response.json(PROMPTS))
        if (url.endsWith('api/mcp')) return Promise.resolve(Response.json(MCP))
        sent.push(JSON.parse(String(init?.body)) as Record<string, unknown>)
        // Fails the first time, answers the second: exactly the case Retry is
        // there for.
        return Promise.resolve(
          sent.length === 1
            ? Response.json({ code: 'upstream', message: 'the endpoint refused' }, { status: 502 })
            : sse(agentStream([answerTurn(200, 'pong')])),
        )
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(screen.getByText('the endpoint refused')).toBeInTheDocument())

    // The question stayed on screen, so the retry is on it rather than on an
    // answer nobody got.
    await user.click(screen.getByRole('button', { name: 'Retry turn 1' }))
    await waitFor(() => expect(sent).toHaveLength(2))

    expect(sent[1]?.messages).toEqual([{ role: 'user', content: 'one' }])
    const conversation = within(panel('Conversation'))
    expect(conversation.getAllByText('one')).toHaveLength(1)
    await waitFor(() => expect(conversation.getByText('pong')).toBeInTheDocument())
  })

  it('starts over when the conversation is reset', async () => {
    const { fetchMock, sent } = recordingApi(['pong', 'pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'one')
    await agentMode(user)
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

  it('keeps the label and the Retry out of a copied message', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'a question')
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    const conversation = within(panel('Conversation'))

    // jsdom has neither layout nor a clipboard, so what is asserted is the rule
    // that decides the paste rather than the paste itself: `user-select: none`
    // is what keeps a browser from putting a run of chrome in the selection.
    expect(conversation.getByText('you').parentElement?.className).toContain('select-none')
    expect(
      conversation.getByRole('button', { name: 'Retry turn 1' }).closest('div')?.className,
    ).toContain('select-none')

    // And the thing being copied stays copyable, which is the other half of it.
    expect(conversation.getByText('a question').className).not.toContain('select-none')
  })

  it('shows the highlight on a question, which is printed in the highlight colour', async () => {
    const { fetchMock, sent } = recordingApi(['pong'])
    vi.stubGlobal('fetch', fetchMock)

    const user = userEvent.setup()
    render(<App />)

    await type(user, 'a question')
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    const conversation = within(panel('Conversation'))

    // jsdom paints nothing, so what is asserted is the rule again: the bubble
    // you typed into is the one surface wearing the brand colour, and a
    // highlight in that same colour is a selection nobody can see. The pair is
    // flipped back on the bubble; the answer beside it keeps the page's.
    const asked = conversation.getByText('a question').parentElement?.className ?? ''
    expect(asked).toContain('bg-brand')
    expect(asked).toContain('selection:bg-on-brand')
    expect(asked).toContain('selection:text-brand')

    const answered = conversation.getByText('pong').closest('div')?.className ?? ''
    expect(answered).not.toContain('selection:')
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

describe('preflight', () => {
  it('says where the call is going and who it goes as, before it is made', async () => {
    render(<App />)

    await screen.findByRole('button', { name: /^chat/ })
    const bar = screen.getByLabelText('What the next call will do')

    expect(within(bar).getByText('ready')).toBeInTheDocument()
    expect(within(bar).getByText('https://models.internal/v1/chat/completions')).toBeInTheDocument()
    expect(bar).toHaveTextContent('as anonymous')
  })

  it('counts the servers a run would set up first', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent('3 MCP servers')
  })

  it('names what would refuse the call, and starts the login that fixes it', async () => {
    const popup = { location: { href: '' }, closed: false, close: vi.fn() }
    vi.stubGlobal(
      'open',
      vi.fn(() => popup),
    )

    const logins: string[] = []
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
        } else if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
        } else if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = AUTH
        }
        return Promise.resolve(Response.json(payload))
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    // `as-me` calls as a human, and nobody is signed in.
    await user.click(await screen.findByRole('button', { name: /as-me/ }))
    const bar = screen.getByLabelText('What the next call will do')
    expect(within(bar).getByText('blocked')).toBeInTheDocument()
    expect(bar).toHaveTextContent('Nobody is signed in to me')

    // The fix is on the bar rather than three panels away.
    await user.click(within(bar).getByRole('button', { name: 'Sign in to me' }))
    await waitFor(() => expect(logins).toHaveLength(1))
    expect(logins[0]).toContain('/api/auth/me/login')
  })

  it('opens the auth detail by itself when the fix is a field inside it', async () => {
    const user = userEvent.setup()
    render(<App />)

    // `guarded` names `pasted`, whose value only this tab can supply — so the
    // panel holding the box is already open rather than folded away.
    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    expect(await screen.findByPlaceholderText('paste the credential')).toBeInTheDocument()

    await user.type(screen.getByPlaceholderText('paste the credential'), 'sk-test')
    expect(screen.getByLabelText('What the next call will do')).not.toHaveTextContent(
      'has no value',
    )
  })
})

describe('stopping a run', () => {
  it('drops the request and keeps what had already arrived', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input)
        if (url.endsWith('api/agent')) {
          return Promise.resolve(stalled(init?.signal))
        }
        let payload: unknown = PROFILES
        if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
        }
        if (url.endsWith('api/mcp')) {
          payload = MCP
        } else if (url.endsWith('api/auth')) {
          payload = AUTH
        }
        return Promise.resolve(Response.json(payload))
      }),
    )

    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /^chat/ }))
    // The box arrives prefilled, so it is cleared rather than typed into twice.
    await user.clear(screen.getByLabelText('Message'))
    await user.type(screen.getByLabelText('Message'), 'are you there')
    await agentMode(user)
    await user.click(screen.getByRole('button', { name: 'Send' }))

    // Nothing to stop until there is something in flight.
    const stop = await screen.findByRole('button', { name: 'Stop' })
    await user.click(stop)

    expect(await screen.findByText(/Stopped\./)).toBeInTheDocument()
    // The question stays: it was asked, and the endpoint was told about it.
    expect(screen.getByText('are you there')).toBeInTheDocument()
    // Being called off is not a failure, and must not be reported as one.
    expect(screen.queryByText(/could not run this call/)).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument()
  })
})

describe('reading a run', () => {
  it('takes a tool in the transcript to its card in the traffic', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const conversation = within(panel('Conversation'))
    await waitFor(() => expect(conversation.getByText('get_weather')).toBeInTheDocument())

    // A run puts five folded cards below, which is how somebody reads a long
    // session, and the point of the link is that it unfolds the one card being
    // pointed at.
    expect(within(card(/Turn 1 · get_weather/)).queryByText(/"temp"/)).not.toBeInTheDocument()

    // The scroll is the half a fold cannot show: jsdom has no layout, so the
    // stub is watched rather than the viewport.
    const scrolled = vi.spyOn(Element.prototype, 'scrollIntoView')
    await user.click(conversation.getByRole('button', { name: 'get_weather' }))

    const tool = within(card(/Turn 1 · get_weather/))
    expect(tool.getByText(/arguments match the schema/)).toBeInTheDocument()
    await waitFor(() => expect(scrolled).toHaveBeenCalled())
    scrolled.mockRestore()
    // And the row's turn opens the model call that asked for it.
    await user.click(conversation.getByRole('button', { name: 'turn 1' }))
    expect(within(card(/Turn 1 · model/)).getByText('Request')).toBeInTheDocument()
  })

  it('narrows the traffic to one kind of exchange, and back', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const traffic = within(panel('Traffic'))
    await waitFor(() =>
      expect(traffic.getByRole('button', { name: /Turn 1 · model/ })).toBeInTheDocument(),
    )
    expect(traffic.getByText('5 exchanges')).toBeInTheDocument()

    await user.click(traffic.getByRole('button', { name: 'Tools' }))
    expect(traffic.getByText('1 of 5')).toBeInTheDocument()
    expect(traffic.queryByRole('button', { name: /Turn 1 · model/ })).not.toBeInTheDocument()
    expect(traffic.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()

    await user.click(traffic.getByRole('button', { name: 'All' }))
    expect(traffic.getByText('5 exchanges')).toBeInTheDocument()
  })

  it('says outright when nothing failed, rather than offering an empty filter', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const traffic = within(panel('Traffic'))
    await waitFor(() => expect(traffic.getByText('5 exchanges')).toBeInTheDocument())
    expect(traffic.getByRole('button', { name: 'Nothing failed' })).toBeDisabled()
  })

  it('picks out the exchange that failed, whichever kind it was', async () => {
    const user = userEvent.setup()
    // The same run, with the one thing wrong that no status code reports: the
    // call went out, the server answered, and the arguments were never valid.
    const turn = turnFixture()
    const broken = {
      ...turn,
      tools: turn.tools.map((tool) => ({
        ...tool,
        schemaErrors: ['city: expected string, got number'],
      })),
    }
    vi.stubGlobal('fetch', toolRunApi([broken]))

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const traffic = within(panel('Traffic'))
    await waitFor(() => expect(traffic.getByText('5 exchanges')).toBeInTheDocument())

    // The model call answered 200 and the handshake landed; only the arguments
    // were wrong, and that is what the filter is for.
    await user.click(traffic.getByRole('button', { name: /1 failed/ }))
    expect(traffic.getByText('1 of 5')).toBeInTheDocument()
    expect(traffic.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    expect(traffic.queryByRole('button', { name: /Turn 1 · model/ })).not.toBeInTheDocument()
  })

  it('drops a filter that would hide the card being pointed at', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    const traffic = within(panel('Traffic'))
    await waitFor(() => expect(traffic.getByText('5 exchanges')).toBeInTheDocument())

    // Reading the protocol, then following a tool from the transcript: the
    // click has to land rather than appear to do nothing.
    await user.click(traffic.getByRole('button', { name: 'Protocol' }))
    expect(traffic.queryByRole('button', { name: /Turn 1 · get_weather/ })).not.toBeInTheDocument()

    await user.click(within(panel('Conversation')).getByRole('button', { name: 'get_weather' }))
    expect(traffic.getByRole('button', { name: /Turn 1 · get_weather/ })).toBeInTheDocument()
    expect(traffic.getByText('5 exchanges')).toBeInTheDocument()
  })
})

describe('saved prompts', () => {
  it('drops the one you pick in the box, and sends nothing', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.clear(await screen.findByLabelText('Message'))
    await user.selectOptions(screen.getByLabelText('Saved'), 'two lines')

    expect(screen.getByLabelText('Message')).toHaveValue('one\ntwo')
    // A prompt is text for the box. Nothing has left the process for it.
    expect(screen.queryByRole('log', { name: 'Conversation' })).toHaveTextContent(
      'Nothing said yet',
    )
  })

  it('replaces what was in the box rather than growing it', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.clear(await screen.findByLabelText('Message'))
    await user.type(screen.getByLabelText('Message'), 'half a thought')
    await user.selectOptions(screen.getByLabelText('Saved'), 'smallest signal')

    expect(screen.getByLabelText('Message')).toHaveValue('ping')
  })

  it('picks the same one twice, because a select that remembers looks broken', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.selectOptions(await screen.findByLabelText('Saved'), 'smallest signal')
    await user.clear(screen.getByLabelText('Message'))
    await user.selectOptions(screen.getByLabelText('Saved'), 'smallest signal')

    expect(screen.getByLabelText('Message')).toHaveValue('ping')
  })

  it('names the entry prompts.yaml would not load, where it bites', async () => {
    render(<App />)

    expect(await screen.findByText(/1 entry of prompts.yaml did not load/)).toHaveTextContent(
      'hollow',
    )
  })

  it('offers the same library to an embedding box', async () => {
    const user = userEvent.setup()
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /embed/ }))
    await user.selectOptions(await screen.findByLabelText('Saved'), 'two lines')

    expect(screen.getByLabelText('One text per line')).toHaveValue('one\ntwo')
  })

  it('is gone entirely when the file declares nothing and complains about nothing', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/prompts': { prompts: [], issues: [] },
      }),
    )

    render(<App />)

    await screen.findByLabelText('Message')
    expect(screen.queryByLabelText('Saved')).not.toBeInTheDocument()
  })
})

describe('coming back to it', () => {
  it('reopens on the profile and the draft it was left on', async () => {
    const user = userEvent.setup()
    const first = render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    await user.clear(screen.getByLabelText('Message'))
    await user.type(screen.getByLabelText('Message'), 'half a thought')
    first.unmount()

    render(<App />)
    expect(await screen.findByLabelText('Message')).toHaveValue('half a thought')
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent(
      'http://127.0.0.1:11435/v1/messages',
    )
  })

  it('reopens in the mode it was left in', async () => {
    const user = userEvent.setup()
    const first = render(<App />)

    // Away from the default, which is the only setting a reload can get wrong.
    await chatMode(user)
    first.unmount()

    render(<App />)
    expect(await screen.findByLabelText('mode')).toHaveValue('chat')
    // And the controls a chat has no use for stay out of the way with it.
    expect(screen.getByLabelText(/max turns/)).toBeDisabled()
  })

  it('never keeps the credential, whatever else it keeps', async () => {
    const user = userEvent.setup()
    const first = render(<App />)

    await user.click(await screen.findByRole('button', { name: /guarded/ }))
    await user.type(await screen.findByPlaceholderText('paste the credential'), 'sk-secret')
    first.unmount()

    // Not under a key of ours, and not under anybody else's either.
    const stored = Object.keys(window.localStorage).map((key) => window.localStorage.getItem(key))
    expect(JSON.stringify(stored)).not.toContain('sk-secret')

    render(<App />)
    expect(await screen.findByPlaceholderText('paste the credential')).toHaveValue('')
  })

  it('falls back to a profile that exists when the remembered one is gone', async () => {
    const user = userEvent.setup()
    const first = render(<App />)
    await user.click(await screen.findByRole('button', { name: /as-me/ }))
    first.unmount()

    // The file was deleted between the two visits.
    const fewer = { ...PROFILES, profiles: PROFILES.profiles.filter((one) => one.name !== 'as-me') }
    vi.stubGlobal(
      'fetch',
      mockApi({ 'api/profiles': fewer, 'api/auth': AUTH, 'api/mcp': MCP, 'api/prompts': PROMPTS }),
    )

    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })
    expect(screen.getByLabelText('What the next call will do')).toHaveTextContent(
      'https://models.internal/v1/chat/completions',
    )
  })
})

describe('taking the run away with you', () => {
  it('writes out every exchange, with what the run was pointed at', async () => {
    const user = userEvent.setup()
    vi.stubGlobal('fetch', toolRunApi())

    const written: string[] = []
    // The two statics only, defined rather than spied on: jsdom has no object
    // URLs at all, and replacing the whole of `URL` would take the constructor
    // every other module builds its request paths with.
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: () => 'blob:mire' })
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: () => {} })
    // The blob is the file: reading it back is reading what would be saved.
    const OriginalBlob = globalThis.Blob
    vi.stubGlobal(
      'Blob',
      class extends OriginalBlob {
        constructor(parts: BlobPart[], options?: BlobPropertyBag) {
          super(parts, options)
          written.push(String(parts[0]))
        }
      },
    )
    const clicked = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})

    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() =>
      expect(within(panel('Traffic')).getByText('5 exchanges')).toBeInTheDocument(),
    )

    await user.click(within(panel('Traffic')).getByRole('button', { name: 'Export' }))

    expect(clicked).toHaveBeenCalled()
    const payload = JSON.parse(written[0] ?? '{}')
    expect(payload.tool).toBe('mire')
    expect(payload.profile).toBe('chat')
    expect(payload.endpoint).toBe('https://models.internal/v1/chat/completions')
    expect(payload.exchanges).toHaveLength(5)
    // The history as the next request would have carried it, not a rendering of it.
    expect(payload.messages[0]).toEqual({ role: 'user', content: 'ping' })

    clicked.mockRestore()
  })
})

/**
 * What `POST /api/uploads` answers: a display name and a stored name that are
 * not the same string, which is the part the UI has to get right.
 */
const UPLOADED = {
  id: 'aB3dE5gH7jK9',
  name: 'report.pdf',
  storedAs: 'aB3dE5gH7jK9-report.pdf',
  path: '/home/gleroy/uploads/aB3dE5gH7jK9-report.pdf',
  size: 14336,
  contentType: 'application/pdf',
}

/**
 * The file input **Attach** clicks.
 *
 * Found in the DOM rather than by role: it is hidden and `aria-hidden`, because
 * assistive technology should be offered the button and not two controls doing
 * the same thing. There is no picker in jsdom either way, so a test changes the
 * input the way the browser would once the dialog closed.
 */
function picker(): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>('input[type="file"]')
  if (!input) {
    throw new Error('no file input')
  }
  return input
}

function pick(files: File[]): void {
  fireEvent.change(picker(), { target: { files } })
}

/**
 * The JSON body a route was sent, parsed.
 *
 * Throws rather than optional-chaining its way to `undefined`: inside a
 * `waitFor` a throw is what "not yet" looks like, and a silent `undefined` would
 * be an assertion that passed against nothing.
 */
function bodySentTo(
  fetchMock: ReturnType<typeof mockApi>,
  suffix: string,
): Record<string, unknown> {
  const sent = fetchMock.mock.calls.find(([url]) => String(url).endsWith(suffix))
  if (!sent) {
    throw new Error(`nothing was sent to ${suffix}`)
  }
  return JSON.parse(String((sent[1] as RequestInit).body)) as Record<string, unknown>
}

describe('attaching a file', () => {
  it('sends it as multipart and shows what the server stored', async () => {
    const fetchMock = mockApi({
      'api/profiles': PROFILES,
      'api/auth': AUTH,
      'api/mcp': MCP,
      'api/prompts': PROMPTS,
      'api/uploads': UPLOADED,
    })
    vi.stubGlobal('fetch', fetchMock)
    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })

    pick([new File(['a known signal'], 'report.pdf', { type: 'application/pdf' })])

    // The name you recognise, not the prefixed one it is called on disk.
    expect(await screen.findByText('report.pdf')).toBeInTheDocument()
    expect(screen.getByText('14 kB')).toBeInTheDocument()

    const upload = fetchMock.mock.calls.find(([url]) => String(url).endsWith('api/uploads'))
    expect(upload).toBeDefined()
    const init = upload?.[1] as RequestInit
    expect(init.method).toBe('POST')
    // `FormData` sets its own content type, boundary included. A hand-written
    // one would be a boundary the server cannot find.
    expect(init.headers).toBeUndefined()
    const body = init.body as FormData
    expect((body.get('file') as File).name).toBe('report.pdf')
  })

  /**
   * The claim has to stop where the truth does: the file goes to the template,
   * and what the template does with it is the profile's business.
   */
  it('says the file goes to the template rather than to the endpoint', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/prompts': PROMPTS,
        'api/uploads': UPLOADED,
      }),
    )
    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })

    pick([new File(['a known signal'], 'report.pdf', { type: 'application/pdf' })])

    await screen.findByText('report.pdf')
    expect(panel('Conversation')).toHaveTextContent(/handed to this profile's template as uploads/)
    expect(panel('Conversation')).toHaveTextContent(/sends what it always sent/)
  })

  /**
   * The ids, and only the ids. The bytes are already on the server; sending them
   * again in the call body would be uploading the file twice.
   */
  it('names the attached files in the next call', async () => {
    const user = userEvent.setup()
    const fetchMock = mockApi({
      'api/profiles': PROFILES,
      'api/auth': AUTH,
      'api/mcp': MCP,
      'api/prompts': PROMPTS,
      'api/uploads': UPLOADED,
      'api/agent': agentStream([answerTurn(200, 'pong')]),
    })
    vi.stubGlobal('fetch', fetchMock)
    render(<App />)
    await agentMode(user)
    await screen.findByRole('button', { name: 'Send' })

    pick([new File(['a known signal'], 'report.pdf', { type: 'application/pdf' })])
    await screen.findByText('report.pdf')
    await user.click(screen.getByRole('button', { name: 'Send' }))

    await waitFor(() => {
      const sent = bodySentTo(fetchMock, 'api/agent')
      expect(sent.uploads).toEqual([UPLOADED.id])
    })
  })

  /** Nothing attached is no field at all, not an empty list. */
  it('leaves the field out entirely when nothing is attached', async () => {
    const user = userEvent.setup()
    const fetchMock = mockApi({
      'api/profiles': PROFILES,
      'api/auth': AUTH,
      'api/mcp': MCP,
      'api/prompts': PROMPTS,
      'api/agent': agentStream([answerTurn(200, 'pong')]),
    })
    vi.stubGlobal('fetch', fetchMock)
    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))

    await waitFor(() => {
      expect(bodySentTo(fetchMock, 'api/agent')).not.toHaveProperty('uploads')
    })
  })

  it('sends one request per file', async () => {
    const fetchMock = mockApi({
      'api/profiles': PROFILES,
      'api/auth': AUTH,
      'api/mcp': MCP,
      'api/prompts': PROMPTS,
      'api/uploads': UPLOADED,
    })
    vi.stubGlobal('fetch', fetchMock)
    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })

    pick([
      new File(['one'], 'a.txt', { type: 'text/plain' }),
      new File(['two'], 'b.txt', { type: 'text/plain' }),
    ])

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([url]) => String(url).endsWith('api/uploads')),
      ).toHaveLength(2),
    )
  })

  it('shows the reason a file was refused, and lists nothing', async () => {
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
        if (url.endsWith('api/mcp')) {
          return Promise.resolve(Response.json(MCP))
        }
        if (url.endsWith('api/prompts')) {
          return Promise.resolve(Response.json(PROMPTS))
        }
        return Promise.resolve(
          Response.json(
            {
              code: 'upload_too_large',
              message: 'the file is 26214401 bytes, the limit is 26214400',
              detail: { limitBytes: 26214400 },
            },
            { status: 413 },
          ),
        )
      }),
    )
    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })

    pick([new File(['x'], 'big.bin', { type: 'application/octet-stream' })])

    expect(await screen.findByText(/the limit is 26214400/)).toBeInTheDocument()
    expect(screen.queryByText('big.bin')).not.toBeInTheDocument()
  })

  /**
   * Forgetting is a list operation. `mire` does not delete files off somebody's
   * disk because a browser tab said so, so nothing goes out on this click.
   */
  it('forgets a file without asking the server to delete it', async () => {
    const user = userEvent.setup()
    const fetchMock = mockApi({
      'api/profiles': PROFILES,
      'api/auth': AUTH,
      'api/mcp': MCP,
      'api/prompts': PROMPTS,
      'api/uploads': UPLOADED,
    })
    vi.stubGlobal('fetch', fetchMock)
    render(<App />)
    await screen.findByRole('button', { name: /^chat/ })

    pick([new File(['a known signal'], 'report.pdf', { type: 'application/pdf' })])
    await screen.findByText('report.pdf')
    // Requests about the file, rather than every request the page made. The
    // whole page is not the subject here, and counting all of it would fail on
    // whatever else the tab happens to be doing — a login poll from an earlier
    // test outlives it by up to three minutes and lands wherever it lands.
    const aboutTheFile = () =>
      fetchMock.mock.calls.filter(([input]) => String(input).includes('api/uploads'))
    const before = aboutTheFile().length

    await user.click(screen.getByRole('button', { name: 'Forget report.pdf' }))

    expect(screen.queryByText('report.pdf')).not.toBeInTheDocument()
    expect(aboutTheFile()).toHaveLength(before)
  })

  /**
   * A new conversation is a clean composer, list included. The turn first,
   * because **New conversation** is inert until there is one to leave — the
   * chips have their own way off, and a second control for an empty page would
   * be a button that does nothing.
   */
  it('clears the list when the conversation is reset', async () => {
    const user = userEvent.setup()
    vi.stubGlobal(
      'fetch',
      mockApi({
        'api/profiles': PROFILES,
        'api/auth': AUTH,
        'api/mcp': MCP,
        'api/prompts': PROMPTS,
        'api/uploads': UPLOADED,
        'api/agent': agentStream([answerTurn(200, 'pong')]),
      }),
    )
    render(<App />)
    await agentMode(user)
    await user.click(await screen.findByRole('button', { name: 'Send' }))
    await waitFor(() =>
      expect(within(panel('Traffic')).getByText('1 exchange')).toBeInTheDocument(),
    )

    pick([new File(['a known signal'], 'report.pdf', { type: 'application/pdf' })])
    await screen.findByText('report.pdf')

    await user.click(screen.getByRole('button', { name: 'New conversation' }))

    expect(screen.queryByText('report.pdf')).not.toBeInTheDocument()
  })
})

describe('on a narrow screen', () => {
  it('folds the profile list away, and closes it again once one is picked', async () => {
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      writable: true,
      value: (query: string) => ({
        matches: false,
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    })

    const user = userEvent.setup()
    render(<App />)

    // One line saying where you are, rather than a screenful to scroll past.
    await waitFor(() => expect(within(panel('Profiles')).getByText('chat')).toBeInTheDocument())
    expect(screen.queryByRole('button', { name: /guarded/ })).not.toBeInTheDocument()

    await user.click(within(panel('Profiles')).getByRole('button', { name: 'Change' }))
    await user.click(screen.getByRole('button', { name: /guarded/ }))

    expect(screen.queryByRole('button', { name: /guarded/ })).not.toBeInTheDocument()
    expect(within(panel('Profiles')).getByText('guarded')).toBeInTheDocument()
  })
})
