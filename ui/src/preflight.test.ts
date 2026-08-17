import { describe, expect, it } from 'vitest'
import type { AuthDescriptor, McpDescriptor, ProfileSummary } from './api'
import { preflight, reaches } from './preflight'

const PROFILE: ProfileSummary = {
  name: 'chat',
  kind: 'chat',
  url: 'https://models.internal/v1/chat/completions',
  auth: 'token',
  mcp: [],
  source: '/tmp/chat.yaml',
  hasDecode: true,
}

const PROVIDER: AuthDescriptor = {
  name: 'token',
  kind: 'token',
  needsValue: false,
  needsLogin: false,
  allowedHosts: [],
}

function run(overrides: {
  profile?: Partial<ProfileSummary>
  provider?: Partial<AuthDescriptor> | undefined
  providers?: AuthDescriptor[]
  servers?: McpDescriptor[]
  token?: string
  /** Agent mode unless a test says otherwise: a chat sets no server up. */
  usesMcp?: boolean
  mcpOff?: string[]
}) {
  const provider =
    'provider' in overrides && overrides.provider === undefined
      ? undefined
      : { ...PROVIDER, ...overrides.provider }

  return preflight({
    profile: { ...PROFILE, ...overrides.profile },
    provider,
    providers: overrides.providers ?? (provider ? [provider] : []),
    servers: overrides.servers ?? [],
    token: overrides.token ?? '',
    usesMcp: overrides.usesMcp ?? true,
    mcpOff: overrides.mcpOff ?? [],
  })
}

describe('reaches', () => {
  it('lets a credential with no allowed_hosts go anywhere', () => {
    expect(reaches(PROVIDER, PROFILE)).toBe(true)
  })

  it('refuses a host the credential was not pinned to', () => {
    expect(reaches({ ...PROVIDER, allowedHosts: ['127.0.0.1'] }, PROFILE)).toBe(false)
  })
})

describe('preflight', () => {
  it('clears a profile whose identity is resolved and unconstrained', () => {
    const state = run({})
    expect(state.blockers).toEqual([])
    expect(state.identity).toBe('token')
    expect(state.url).toBe('https://models.internal/v1/chat/completions')
  })

  it('blocks on an identity no auth.yaml entry declares', () => {
    const state = run({ provider: undefined, profile: { auth: 'ghost' } })
    expect(state.blockers).toHaveLength(1)
    expect(state.blockers[0]?.message).toContain('ghost')
    // Nothing to press: the fix is in a file, not in this tab.
    expect(state.blockers[0]?.signIn).toBeUndefined()
  })

  it('blocks a credential pinned away from where the profile points', () => {
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

  it('blocks on an MCP server no mcp.yaml entry declares', () => {
    const state = run({ profile: { mcp: ['ghost'] } })
    expect(state.blockers[0]?.message).toContain('ghost')
    expect(state.servers).toEqual(['ghost'])
  })

  it('blocks on a server whose identity nobody is signed in to, named or templated', () => {
    const human: AuthDescriptor = {
      name: 'me',
      kind: 'oidc_browser',
      needsValue: false,
      needsLogin: true,
      allowedHosts: [],
    }
    const servers: McpDescriptor[] = [
      { name: 'named', url: 'https://a', auth: 'me', tools: [], headers: [], usesAuth: [] },
      { name: 'templated', url: 'https://b', tools: [], headers: [], usesAuth: ['me'] },
    ]

    const state = run({
      profile: { mcp: ['named', 'templated'] },
      providers: [PROVIDER, human],
      servers,
    })
    expect(state.blockers).toHaveLength(2)
    expect(state.blockers.every((blocker) => blocker.signIn === 'me')).toBe(true)

    // A session on that provider is all it took.
    const signedIn = run({
      profile: { mcp: ['named', 'templated'] },
      providers: [
        PROVIDER,
        { ...human, session: { expiresInS: 600, canRefresh: true, subject: 'gleroy' } },
      ],
      servers,
    })
    expect(signedIn.blockers).toEqual([])
  })

  it('leaves the servers out entirely on a run that will not speak to them', () => {
    const human: AuthDescriptor = {
      name: 'me',
      kind: 'oidc_browser',
      needsValue: false,
      needsLogin: true,
      allowedHosts: [],
    }
    const overrides = {
      profile: { mcp: ['named', 'ghost'] },
      providers: [PROVIDER, human],
      servers: [
        { name: 'named', url: 'https://a', auth: 'me', tools: [], headers: [], usesAuth: [] },
      ] as McpDescriptor[],
    }

    // In agent mode both are the run's business: one is undeclared, the other
    // needs a session.
    expect(run(overrides).blockers).toHaveLength(2)

    // In chat mode neither is. A single turn calls no tool, so a credential it
    // never uses cannot refuse it — and the bar stays green, correctly.
    const chat = run({ ...overrides, usesMcp: false })
    expect(chat.blockers).toEqual([])
    expect(chat.servers).toEqual([])
  })

  it('drops a server switched off, and says so rather than quietly shrinking', () => {
    const human: AuthDescriptor = {
      name: 'me',
      kind: 'oidc_browser',
      needsValue: false,
      needsLogin: true,
      allowedHosts: [],
    }
    const overrides = {
      profile: { mcp: ['named', 'ghost'] },
      providers: [PROVIDER, human],
      servers: [
        { name: 'named', url: 'https://a', auth: 'me', tools: [], headers: [], usesAuth: [] },
      ] as McpDescriptor[],
    }

    // `ghost` is undeclared and `named` wants a session: two refusals, both of
    // them about servers this run would set up.
    expect(run(overrides).blockers).toHaveLength(2)

    // Switched off, and with it the refusal it was causing — the same way a chat
    // drops them, because this run reaches it exactly as little.
    const narrowed = run({ ...overrides, mcpOff: ['ghost'] })
    expect(narrowed.servers).toEqual(['named'])
    expect(narrowed.blockers).toHaveLength(1)
    expect(narrowed.blockers[0]?.signIn).toBe('me')
    expect(narrowed.notes[0]).toContain('ghost')

    // All of them off: nothing to set up, nothing to refuse it, and the note is
    // what keeps that from looking like a profile with no servers at all.
    const none = run({ ...overrides, mcpOff: ['ghost', 'named'] })
    expect(none.servers).toEqual([])
    expect(none.blockers).toEqual([])
    expect(none.notes[0]).toContain('named')
  })

  it('says nothing about servers switched off on a run that reaches none anyway', () => {
    // Chat mode already leaves every server out, so a note listing the ones
    // somebody unticked would be reporting a distinction this run does not have.
    const chat = run({ profile: { mcp: ['named'] }, mcpOff: ['named'], usesMcp: false })
    expect(chat.servers).toEqual([])
    expect(chat.notes).toEqual([])
  })

  it('counts a missing decode block as a note rather than a refusal', () => {
    const state = run({ profile: { hasDecode: false } })
    expect(state.blockers).toEqual([])
    expect(state.notes).toHaveLength(1)
  })
})
