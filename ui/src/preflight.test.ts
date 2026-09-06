import { describe, expect, it } from 'vitest'
import type { AuthDescriptor, McpDescriptor, ModelSummary } from './api'
import { activeServers, serverNames } from './mcp'
import { preflight, reaches } from './preflight'

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
  uploads?: number
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
    uploads: overrides.uploads ?? 0,
    mcpActive: usesMcp ? activeServers(servers, on, {}) : [],
    mcpDeclared: usesMcp ? serverNames(servers) : [],
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
    expect(state.blockers).toEqual([])
    expect(state.identity).toBe('token')
    expect(state.url).toBe('https://models.internal/v1/chat/completions')
  })

  it('blocks on an identity no provider declares', () => {
    const state = run({ provider: undefined, model: { auth: 'ghost' } })
    expect(state.blockers).toHaveLength(1)
    expect(state.blockers[0]?.message).toContain('ghost')
    // Nothing to press: the fix is in a file, not in this tab.
    expect(state.blockers[0]?.signIn).toBeUndefined()
  })

  it('blocks a requires_upload model until a file is attached', () => {
    const empty = run({ model: { name: 'whisper', requiresUpload: true } })
    expect(empty.blockers).toHaveLength(1)
    expect(empty.blockers[0]?.message).toContain('whisper')
    // The fix is a button on the composer, not one on the bar.
    expect(empty.blockers[0]?.needsUpload).toBe(true)
    expect(empty.blockers[0]?.signIn).toBeUndefined()

    // Any file clears it: the model asked for one, not for a particular one.
    expect(run({ model: { requiresUpload: true }, uploads: 1 }).blockers).toEqual([])
  })

  it('says nothing about attachments a model never asked for', () => {
    expect(run({ uploads: 0 }).blockers).toEqual([])
    expect(run({ uploads: 3 }).blockers).toEqual([])
  })

  it('blocks a credential pinned away from where the model points', () => {
    const state = run({ provider: { allowedHosts: ['127.0.0.1'] } })
    expect(state.blockers[0]?.message).toContain('models.internal')
  })

  it('blocks on a credential this tab was never given, and clears once it is', () => {
    expect(run({ provider: { needsValue: true } }).blockers).toHaveLength(1)
    // Whitespace is not a credential.
    expect(run({ provider: { needsValue: true }, token: '   ' }).blockers).toHaveLength(1)
    expect(run({ provider: { needsValue: true }, token: 'sk-x' }).blockers).toEqual([])
  })

  it('sends you to the sign-in for a browser identity with no session', () => {
    const state = run({ provider: { needsLogin: true } })
    expect(state.blockers[0]?.signIn).toBe('token')
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

    const state = run({ providers: [PROVIDER, human], servers, mcpOn: ['named', 'templated'] })
    expect(state.blockers).toHaveLength(2)
    expect(state.blockers.every((blocker) => blocker.signIn === 'me')).toBe(true)

    // A session on that provider is all it took.
    const signedIn = run({
      providers: [
        PROVIDER,
        { ...human, session: { expiresInS: 600, canRefresh: true, subject: 'gleroy' } },
      ],
      servers,
      mcpOn: ['named', 'templated'],
    })
    expect(signedIn.blockers).toEqual([])
  })

  it('reaches no server until one is switched on, and says as much', () => {
    // Declaring a server in `mcp/` makes it available, not live: a tool call
    // really runs somewhere, so the run reaches one because somebody said so.
    const idle = run({ servers: TWO_SERVERS })
    expect(idle.servers).toEqual([])
    expect(idle.blockers).toEqual([])
    expect(idle.notes[0]).toContain('2 declared in mcp/')

    const state = run({ servers: TWO_SERVERS, mcpOn: ['files', 'search'] })
    expect(state.servers).toEqual(['files', 'search'])
    // Nothing left out, so nothing to report.
    expect(state.notes).toEqual([])
  })

  it('leaves the servers out entirely on a run that will not speak to them', () => {
    const overrides = {
      providers: [PROVIDER, HUMAN],
      servers: TWO_SERVERS,
      mcpOn: ['files', 'search'],
    }

    // With the servers in the run both are its business, and both want a session.
    expect(run(overrides).blockers).toHaveLength(2)

    // On an embedding model neither is. There is no loop, so a credential it
    // never uses cannot refuse it — and the bar stays green, correctly. Nothing
    // is noted either: a run with no loop has not left a server out, it has no
    // business with one.
    const loopless = run({ ...overrides, usesMcp: false })
    expect(loopless.blockers).toEqual([])
    expect(loopless.servers).toEqual([])
    expect(loopless.notes).toEqual([])
  })

  it('carries only the blockers of the servers this run actually reaches', () => {
    const overrides = { providers: [PROVIDER, HUMAN], servers: TWO_SERVERS }

    // Both want a session nobody has, and only the one switched on can refuse
    // this call: a server left off is never discovered, listed or signed in to.
    const narrowed = run({ ...overrides, mcpOn: ['files'] })
    expect(narrowed.servers).toEqual(['files'])
    expect(narrowed.blockers).toHaveLength(1)
    expect(narrowed.blockers[0]?.signIn).toBe('me')
    // Something is on, so the note about reaching nothing has nothing to say.
    expect(narrowed.notes).toEqual([])

    expect(run({ ...overrides, mcpOn: ['files', 'search'] }).blockers).toHaveLength(2)
  })

  it('ignores a switched-on name that nothing declares any more', () => {
    // `mcpOn` outlives a reload, and so outlives the entry it was about. A name
    // deleted from `mcp/` is simply not in the picture.
    const state = run({ servers: TWO_SERVERS, mcpOn: ['deleted', 'files'] })
    expect(state.servers).toEqual(['files'])
  })

  it('counts a missing decode block as a note rather than a refusal', () => {
    const state = run({ model: { hasDecode: false } })
    expect(state.blockers).toEqual([])
    expect(state.notes).toHaveLength(1)
  })
})
