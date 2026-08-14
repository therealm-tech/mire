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

  it('counts a missing decode block as a note rather than a refusal', () => {
    const state = run({ profile: { hasDecode: false } })
    expect(state.blockers).toEqual([])
    expect(state.notes).toHaveLength(1)
  })
})
