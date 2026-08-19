import { describe, expect, it } from 'vitest'
import type { AuthDescriptor, McpDescriptor, ProfileSummary } from './api'
import { preflight, reaches } from './preflight'

const PROFILE: ProfileSummary = {
  name: 'chat',
  kind: 'chat',
  url: 'https://models.internal/v1/chat/completions',
  auth: 'token',
  source: '/tmp/chat.yaml',
  hasPrompt: true,
  hasDecode: true,
  requiresUpload: false,
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
  /** A chat profile with servers unless a test says otherwise. */
  usesMcp?: boolean
  uploads?: number
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
    uploads: overrides.uploads ?? 0,
    mcpOff: overrides.mcpOff ?? [],
  })
}

/** A browser identity nobody has signed in to. */
const HUMAN: AuthDescriptor = {
  name: 'me',
  kind: 'oidc_browser',
  needsValue: false,
  needsLogin: true,
  allowedHosts: [],
}

/** Two declared servers, both wanting `me`, in the order the registry lists them. */
const TWO_SERVERS: McpDescriptor[] = [
  { name: 'files', url: 'https://a', auth: 'me', tools: [], headers: [], usesAuth: [] },
  { name: 'search', url: 'https://b', auth: 'me', tools: [], headers: [], usesAuth: [] },
]

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

  it('blocks a requires_upload profile until a file is attached', () => {
    const empty = run({ profile: { name: 'whisper', requiresUpload: true } })
    expect(empty.blockers).toHaveLength(1)
    expect(empty.blockers[0]?.message).toContain('whisper')
    // The fix is a button on the composer, not one on the bar.
    expect(empty.blockers[0]?.needsUpload).toBe(true)
    expect(empty.blockers[0]?.signIn).toBeUndefined()

    // Any file clears it: the profile asked for one, not for a particular one.
    expect(run({ profile: { requiresUpload: true }, uploads: 1 }).blockers).toEqual([])
  })

  it('says nothing about attachments a profile never asked for', () => {
    expect(run({ uploads: 0 }).blockers).toEqual([])
    expect(run({ uploads: 3 }).blockers).toEqual([])
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

    const state = run({ providers: [PROVIDER, human], servers })
    expect(state.blockers).toHaveLength(2)
    expect(state.blockers.every((blocker) => blocker.signIn === 'me')).toBe(true)

    // A session on that provider is all it took.
    const signedIn = run({
      providers: [
        PROVIDER,
        { ...human, session: { expiresInS: 600, canRefresh: true, subject: 'gleroy' } },
      ],
      servers,
    })
    expect(signedIn.blockers).toEqual([])
  })

  it('offers every declared server to a profile that says nothing about them', () => {
    // There is no per-profile opt-in left to read: `mcp.yaml` declares a server
    // and every chat profile is offered it.
    const state = run({
      servers: [
        { name: 'files', url: 'https://a', tools: [], headers: [], usesAuth: [] },
        { name: 'search', url: 'https://b', tools: [], headers: [], usesAuth: [] },
      ],
    })
    expect(state.servers).toEqual(['files', 'search'])
  })

  it('leaves the servers out entirely on a run that will not speak to them', () => {
    const overrides = { providers: [PROVIDER, HUMAN], servers: TWO_SERVERS }

    // With the servers in the run both are its business, and both want a session.
    expect(run(overrides).blockers).toHaveLength(2)

    // On an embedding profile neither is. There is no loop, so a credential it
    // never uses cannot refuse it — and the bar stays green, correctly.
    const loopless = run({ ...overrides, usesMcp: false })
    expect(loopless.blockers).toEqual([])
    expect(loopless.servers).toEqual([])
  })

  it('drops a server switched off, and says so rather than quietly shrinking', () => {
    const overrides = { providers: [PROVIDER, HUMAN], servers: TWO_SERVERS }

    // Both want a session nobody has: two refusals, both of them about servers
    // this run would set up.
    expect(run(overrides).blockers).toHaveLength(2)

    // Switched off, and with it the refusal it was causing: an unticked server is
    // one this run never discovers, lists or signs in to.
    const narrowed = run({ ...overrides, mcpOff: ['search'] })
    expect(narrowed.servers).toEqual(['files'])
    expect(narrowed.blockers).toHaveLength(1)
    expect(narrowed.blockers[0]?.signIn).toBe('me')
    expect(narrowed.notes[0]).toContain('search')

    // All of them off — what the composer's **None** button asks for. Nothing to
    // set up, nothing to refuse it, and the note is what keeps that from looking
    // like an installation with no servers at all.
    const none = run({ ...overrides, mcpOff: ['search', 'files'] })
    expect(none.servers).toEqual([])
    expect(none.blockers).toEqual([])
    expect(none.notes[0]).toContain('files')
  })

  it('ignores a switched-off name that nothing declares any more', () => {
    // `mcpOff` outlives a reload, and so outlives the entry it was about. A name
    // deleted from `mcp.yaml` is simply not in the picture — not a server this
    // run reports having left out.
    const state = run({ servers: TWO_SERVERS, mcpOff: ['deleted'] })
    expect(state.servers).toEqual(['files', 'search'])
    expect(state.notes).toEqual([])
  })

  it('says nothing about servers switched off on a run that reaches none anyway', () => {
    // A run with no loop already leaves every server out, so a note listing the ones
    // somebody unticked would be reporting a distinction this run does not have.
    const loopless = run({ servers: TWO_SERVERS, mcpOff: ['files'], usesMcp: false })
    expect(loopless.servers).toEqual([])
    expect(loopless.notes).toEqual([])
  })

  it('counts a missing decode block as a note rather than a refusal', () => {
    const state = run({ profile: { hasDecode: false } })
    expect(state.blockers).toEqual([])
    expect(state.notes).toHaveLength(1)
  })
})
