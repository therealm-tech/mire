import { describe, expect, it } from 'vitest'
import type { AuthDescriptor, McpDescriptor, ModelSummary } from './api'
import { activeServers } from './mcp'
import { type Preflight, preflight, reaches } from './preflight'

/** The rows that refuse the call — what used to be the whole of `blockers`. */
function blocking(state: Preflight) {
  return state.rows.filter((row) => row.blocks)
}

/** The one row about who the model call goes out as. */
function identity(state: Preflight) {
  return state.rows.find((row) => row.key === 'identity')
}

const MODEL: ModelSummary = {
  id: 'chat',
  name: 'chat',
  isDefault: true,
  kind: 'chat',
  url: 'https://models.internal/v1/chat/completions',
  auth: 'token',
  source: '/tmp/chat.yaml',
  hasPrompt: true,
  hasDecode: true,
  requiresUpload: false,
}

const PROVIDER: AuthDescriptor = {
  id: 'token',
  name: 'token',
  kind: 'token',
  needsValue: false,
  needsLogin: false,
  allowedHosts: [],
}

function run(overrides: {
  model?: Partial<ModelSummary>
  provider?: Partial<AuthDescriptor> | undefined
  providers?: AuthDescriptor[]
  servers?: McpDescriptor[]
  token?: string
  /** A chat model with servers unless a test says otherwise. */
  usesMcp?: boolean
  /** The servers switched on. Nothing is, unless a test says so. */
  mcpOn?: string[]
}) {
  const provider =
    'provider' in overrides && overrides.provider === undefined
      ? undefined
      : { ...PROVIDER, ...overrides.provider }

  // The two MCP lists the way `App` works them out, from the same helpers: the
  // bar reports the run the panel is drawing, and a second reading here would be
  // a second thing to keep in step.
  const servers = overrides.servers ?? []
  const on = overrides.mcpOn ?? []
  const usesMcp = overrides.usesMcp ?? true

  return preflight({
    model: { ...MODEL, ...overrides.model },
    provider,
    providers: overrides.providers ?? (provider ? [provider] : []),
    servers,
    token: overrides.token ?? '',
    mcpActive: usesMcp ? activeServers(servers, on, {}) : [],
  })
}

/** A browser identity nobody has signed in to. */
const HUMAN: AuthDescriptor = {
  id: 'me',
  name: 'me',
  kind: 'oidc_browser',
  needsValue: false,
  needsLogin: true,
  allowedHosts: [],
}

/** Two declared servers, both wanting `me`, in the order the registry lists them. */
const TWO_SERVERS: McpDescriptor[] = [
  {
    id: 'files',
    name: 'files',
    isDefault: true,
    url: 'https://a',
    auth: 'me',
    tools: [],
    headers: [],
    usesAuth: [],
  },
  {
    id: 'search',
    name: 'search',
    isDefault: true,
    url: 'https://b',
    auth: 'me',
    tools: [],
    headers: [],
    usesAuth: [],
  },
]

describe('reaches', () => {
  it('lets a credential with no allowed_hosts go anywhere', () => {
    expect(reaches(PROVIDER, MODEL)).toBe(true)
  })

  it('refuses a host the credential was not pinned to', () => {
    expect(reaches({ ...PROVIDER, allowedHosts: ['127.0.0.1'] }, MODEL)).toBe(false)
  })
})

describe('preflight', () => {
  it('clears a model whose identity is resolved and unconstrained', () => {
    const state = run({})
    expect(blocking(state)).toEqual([])
    expect(identity(state)?.subject).toBe('token')
    expect(state.url).toBe('https://models.internal/v1/chat/completions')
  })

  it('always says who the call goes out as, however dull the answer', () => {
    // The line is read rather than looked for, so it is there when there is
    // nothing wrong — which is most of the time.
    const state = run({})
    expect(state.rows).toHaveLength(1)
    expect(identity(state)?.label).toBe('token')
    expect(identity(state)?.tone).toBe('neutral')
  })

  it('names the source the server reads a token from, and nothing else', () => {
    // The point of the line: an empty MODEL_TOKEN and the wrong variable name
    // are the same 401 until one of them is said out loud. The name is the whole
    // of what the badge cannot hold, so the name is the whole of the detail.
    const env = run({ provider: { valueSource: { from: 'env', name: 'MODEL_TOKEN' } } })
    expect(identity(env)?.detail).toBe('env: MODEL_TOKEN')

    const file = run({ provider: { valueSource: { from: 'file', name: '/run/token' } } })
    expect(identity(file)?.detail).toBe('file: /run/token')
  })

  it('leaves anonymous to its badge', () => {
    const state = run({
      model: { auth: 'anonymous' },
      provider: { id: 'anonymous', name: 'anonymous', kind: 'anonymous' },
    })
    expect(identity(state)?.label).toBe('anonymous')
    expect(identity(state)?.blocks).toBe(false)
    expect(identity(state)?.detail).toBe('')
  })

  it('tells an identity nobody chose apart from one somebody did', () => {
    // Both send nothing. One is a decision in the file and the other is a field
    // that was never filled in, and only the second is worth a word.
    const state = run({
      model: { auth: null },
      provider: { id: 'anonymous', name: 'anonymous', kind: 'anonymous' },
    })
    expect(identity(state)?.detail).toBe('no auth: in this model')
  })

  it('says what a workload identity asserts', () => {
    const state = run({
      provider: { kind: 'oidc', valueSource: { from: 'file', name: '/var/run/sa/token' } },
    })
    expect(identity(state)?.label).toBe('workload')
    expect(identity(state)?.detail).toBe('file: /var/run/sa/token')
  })

  it('blocks on an identity no provider declares', () => {
    const state = run({ provider: undefined, model: { auth: 'ghost' } })
    expect(blocking(state)).toHaveLength(1)
    expect(identity(state)?.label).toBe('undeclared')
    expect(identity(state)?.subject).toBe('ghost')
    // Nothing to press: the fix is in a file, not in this tab.
    expect(identity(state)?.fix).toBeUndefined()
  })

  it('says nothing about what the request carries', () => {
    // A `requires_upload:` model with nothing attached is a real refusal, and
    // it is the composer's to report: the fix is the button next to **Send**,
    // and the bar is about where the call goes and who it goes as.
    const state = run({ model: { name: 'whisper', requiresUpload: true } })
    expect(blocking(state)).toEqual([])
    expect(state.rows).toHaveLength(1)
    expect(state.rows[0]?.key).toBe('identity')
  })

  it('blocks a credential pinned away from where the model points', () => {
    const state = run({ provider: { allowedHosts: ['127.0.0.1'] } })
    expect(identity(state)?.label).toBe('out of allowed_hosts')
    // Where it may go. Where it would have gone is the URL on the line above.
    expect(identity(state)?.detail).toBe('allowed_hosts: 127.0.0.1')
  })

  it('blocks on a credential this tab was never given, and clears once it is', () => {
    const empty = run({ provider: { needsValue: true } })
    expect(blocking(empty)).toHaveLength(1)
    expect(identity(empty)?.label).toBe('no value')
    // Whitespace is not a credential.
    expect(blocking(run({ provider: { needsValue: true }, token: '   ' }))).toHaveLength(1)

    const typed = run({ provider: { needsValue: true }, token: 'sk-x' })
    expect(blocking(typed)).toEqual([])
    expect(identity(typed)?.label).toBe('in this tab')
  })

  it('keeps the credential field through the first keystroke', () => {
    // The row carries the input whether or not it is satisfied: one that
    // appeared and vanished around a value would take the caret with it.
    expect(identity(run({ provider: { needsValue: true } }))?.prompts).toBe(true)
    expect(identity(run({ provider: { needsValue: true }, token: 'sk-x' }))?.prompts).toBe(true)
  })

  it('sends you to the sign-in for a browser identity with no session', () => {
    const state = run({ provider: { needsLogin: true } })
    expect(identity(state)?.label).toBe('not signed in')
    // Nothing after the badge: it has already said the whole of it.
    expect(identity(state)?.detail).toBe('')
    expect(identity(state)?.fix).toEqual({ kind: 'sign-in', provider: 'token', retry: false })
  })

  it('offers to sign out of a session rather than only reporting it', () => {
    const state = run({
      provider: {
        needsLogin: true,
        session: { expiresInS: 600, canRefresh: true, subject: 'gleroy', scope: 'openid' },
      },
    })
    expect(identity(state)?.label).toBe('signed in')
    expect(identity(state)?.blocks).toBe(false)
    expect(identity(state)?.detail).toContain('gleroy')
    expect(identity(state)?.detail).toContain('expires in 10 min')
    expect(identity(state)?.fix).toEqual({ kind: 'sign-out', provider: 'token' })
  })

  it('carries the reason the last sign-in failed, and offers a second way in', () => {
    // Pressing the same button again replays the identity provider's own
    // session, and with it the same failure; `retry` is what says to offer the
    // one that forces it to ask.
    const state = run({ provider: { needsLogin: true, lastError: 'access_denied' } })
    expect(identity(state)?.label).toBe('sign-in failed')
    expect(identity(state)?.detail).toBe('access_denied')
    expect(identity(state)?.fix).toEqual({ kind: 'sign-in', provider: 'token', retry: true })
  })

  it('blocks on a server whose identity nobody is signed in to, named or templated', () => {
    const human: AuthDescriptor = {
      id: 'me',
      name: 'me',
      kind: 'oidc_browser',
      needsValue: false,
      needsLogin: true,
      allowedHosts: [],
    }
    const servers: McpDescriptor[] = [
      {
        id: 'named',
        name: 'named',
        isDefault: true,
        url: 'https://a',
        auth: 'me',
        tools: [],
        headers: [],
        usesAuth: [],
      },
      {
        id: 'templated',
        name: 'templated',
        isDefault: true,
        url: 'https://b',
        tools: [],
        headers: [],
        usesAuth: ['me'],
      },
    ]

    // One line, not two: `me` is what is missing, and one sign-in is what fixes
    // both servers. Which of them wanted it is not said — it is the same
    // identity either way, and each card says its own business.
    const state = run({ providers: [PROVIDER, human], servers, mcpOn: ['named', 'templated'] })
    expect(blocking(state)).toHaveLength(1)
    expect(blocking(state)[0]?.subject).toBe('me')
    expect(blocking(state)[0]?.detail).toBe('')
    expect(blocking(state)[0]?.fix).toEqual({ kind: 'sign-in', provider: 'me', retry: false })

    // A session on that provider is all it took — and the line stays, now
    // saying who was fetched rather than that nobody was.
    const signedIn = run({
      providers: [
        PROVIDER,
        { ...human, session: { expiresInS: 600, canRefresh: true, subject: 'gleroy' } },
      ],
      servers,
      mcpOn: ['named', 'templated'],
    })
    expect(blocking(signedIn)).toEqual([])
    const row = signedIn.rows.find((entry) => entry.key === 'mcp:me')
    expect(row?.label).toBe('signed in')
    expect(row?.detail).toContain('gleroy')
    expect(row?.fix).toEqual({ kind: 'sign-out', provider: 'me' })
  })

  it('reaches no server until one is switched on, and says as much', () => {
    // Declaring a server in `mcp/` makes it available, not live: a tool call
    // really runs somewhere, so the run reaches one because somebody said so.
    const idle = run({ servers: TWO_SERVERS })
    expect(blocking(idle)).toEqual([])
    // Nothing is set up, and the block that holds the switches says so: the bar
    // has nothing to add about a run that reaches no server.
    expect(idle.notes).toEqual([])

    const state = run({ servers: TWO_SERVERS, mcpOn: ['files', 'search'] })
    expect(state.notes).toEqual([])
  })

  it('leaves the servers out entirely on a run that will not speak to them', () => {
    const overrides = {
      providers: [PROVIDER, HUMAN],
      servers: TWO_SERVERS,
      mcpOn: ['files', 'search'],
    }

    // With the servers in the run both are its business, and both want the one
    // session nobody has fetched.
    expect(blocking(run(overrides))).toHaveLength(1)

    // On an embedding model neither is. There is no loop, so a credential it
    // never uses cannot refuse it — and the bar stays green, correctly. Nothing
    // is noted either: a run with no loop has not left a server out, it has no
    // business with one.
    const loopless = run({ ...overrides, usesMcp: false })
    expect(blocking(loopless)).toEqual([])
  })

  it('carries only the blockers of the servers this run actually reaches', () => {
    const overrides = { providers: [PROVIDER, HUMAN], servers: TWO_SERVERS }

    // Both want a session nobody has, and only the one switched on can refuse
    // this call: a server left off is never discovered, listed or signed in to.
    const narrowed = run({ ...overrides, mcpOn: ['files'] })
    expect(blocking(narrowed)).toHaveLength(1)
    expect(blocking(narrowed)[0]?.fix).toEqual({ kind: 'sign-in', provider: 'me', retry: false })

    // Still one line: two servers wanting one identity is one thing to fetch.
    expect(blocking(run({ ...overrides, mcpOn: ['files', 'search'] }))).toHaveLength(1)
  })

  it('says an identity the model and a server share once, and once only', () => {
    // `as-me` calls the model as `me`, and `files` wants `me` too. One sign-in
    // fixes both, so one line says both — with the servers named on it, since
    // "who is missing" and "what is waiting on them" are one answer.
    const state = run({
      model: { auth: 'me' },
      provider: HUMAN,
      providers: [HUMAN],
      servers: TWO_SERVERS,
      mcpOn: ['files'],
    })

    expect(state.rows).toHaveLength(1)
    expect(state.rows[0]?.key).toBe('identity')
    expect(state.rows[0]?.subject).toBe('me')
    expect(state.rows[0]?.detail).toBe('')
  })

  it('keeps an identity only a server wants on its own line', () => {
    // The model calls as `token`, which is fine; `files` wants `me`, which is
    // not. Two identities, two answers, two lines.
    const state = run({ providers: [PROVIDER, HUMAN], servers: TWO_SERVERS, mcpOn: ['files'] })
    expect(state.rows.map((row) => row.subject)).toEqual(['token', 'me'])
    expect(state.rows[1]?.key).toBe('mcp:me')
  })

  it('counts a missing decode block as a note rather than a refusal', () => {
    const state = run({ model: { hasDecode: false } })
    expect(blocking(state)).toEqual([])
    expect(state.notes).toHaveLength(1)
  })
})
