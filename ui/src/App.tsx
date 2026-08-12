import { useCallback, useEffect, useState } from 'react'
import {
  type AgentRequest,
  ApiError,
  type AuthResponse,
  type CallOutcome,
  type CallRequest,
  call,
  callbackUri,
  fetchAuth,
  fetchProfiles,
  logout,
  type ProfilesResponse,
  runAgent,
  startLogin,
  streamCall,
  type Trace,
  type Turn,
} from './api'
import { AgentPanel } from './components/AgentPanel'
import { AuthSelector } from './components/AuthSelector'
import { ProfileList } from './components/ProfileList'
import { Badge, Panel, Spinner } from './components/primitives'
import { RenderedRequest, RequestPanel } from './components/RequestPanel'
import { ResponsePanel } from './components/ResponsePanel'
import { logger } from './logger'

const ANONYMOUS = 'anonymous'

/** How long to wait for someone to get through their identity provider. */
const LOGIN_TIMEOUT_MS = 180_000
const LOGIN_POLL_MS = 1_000

/**
 * Waits for the login to land, by polling.
 *
 * The callback happens in another tab, on a page served by `mire` — so the
 * simplest thing that works everywhere is to ask the server whether it has a
 * session yet. No `postMessage`, no shared origin assumptions, nothing that a
 * notebook proxy could get in the way of.
 */
async function waitForSession(provider: string, popup: Window | null): Promise<AuthResponse> {
  const deadline = Date.now() + LOGIN_TIMEOUT_MS

  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, LOGIN_POLL_MS))

    const latest = await fetchAuth()
    const entry = latest.providers.find((candidate) => candidate.name === provider)
    if (entry?.session) {
      return latest
    }
    // The callback recorded why it failed. That beats anything this side could
    // infer, so it wins over the tab-closed guess below.
    if (entry?.lastError) {
      throw new Error(entry.lastError)
    }

    // The callback page closes itself once it has landed, so a closed popup
    // races with the session appearing. Look once more before giving up.
    if (popup?.closed) {
      const final = await fetchAuth()
      const settled = final.providers.find((candidate) => candidate.name === provider)
      if (settled?.session) {
        return final
      }
      throw new Error(settled?.lastError ?? 'the sign-in tab closed before a session appeared')
    }
  }

  throw new Error('gave up waiting for the sign-in to complete')
}

export function App() {
  const [profiles, setProfiles] = useState<ProfilesResponse | null>(null)
  const [auth, setAuth] = useState<AuthResponse | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)

  const [selectedProfile, setSelectedProfile] = useState<string | null>(null)
  const [selectedAuth, setSelectedAuth] = useState(ANONYMOUS)
  const [token, setToken] = useState('')

  const [prompt, setPrompt] = useState('ping')
  const [input, setInput] = useState('one\ntwo')
  const [repeat, setRepeat] = useState(1)
  const [includeVectors, setIncludeVectors] = useState(false)

  const [maxIterations, setMaxIterations] = useState(6)

  const [signingIn, setSigningIn] = useState<string | null>(null)
  const [loginError, setLoginError] = useState<string | null>(null)

  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<CallOutcome | null>(null)
  const [live, setLive] = useState<string | null>(null)
  const [turns, setTurns] = useState<Turn[]>([])
  const [trace, setTrace] = useState<Trace | null>(null)
  const [callError, setCallError] = useState<ApiError | null>(null)

  useEffect(() => {
    Promise.all([fetchProfiles(), fetchAuth()])
      .then(([loadedProfiles, loadedAuth]) => {
        setProfiles(loadedProfiles)
        setAuth(loadedAuth)
        setSelectedProfile((current) => current ?? loadedProfiles.profiles[0]?.name ?? null)
        logger.info('config.loaded', {
          profiles: loadedProfiles.profiles.length,
          providers: loadedAuth.providers.length,
        })
      })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error)
        logger.error('config.load_failed', { message })
        setLoadError(message)
      })
  }, [])

  const profile = profiles?.profiles.find((candidate) => candidate.name === selectedProfile)

  const signIn = useCallback((provider: string, prompt?: string) => {
    // Opened *before* awaiting anything: a popup opened after an await has lost
    // its user gesture, and browsers block it. It gets its URL a moment later.
    const popup = window.open('', 'mire-login', 'width=520,height=680')

    setSigningIn(provider)
    setLoginError(null)

    startLogin(provider, callbackUri(), prompt)
      .then((login) => {
        logger.info('login.started', { provider, redirectUri: login.redirectUri })
        if (popup) {
          popup.location.href = login.authorizationUrl
        } else {
          // Blocked. Going top-level costs the page state, but beats a dead end.
          window.location.href = login.authorizationUrl
        }
        return waitForSession(provider, popup)
      })
      .then((latest) => {
        setAuth(latest)
        setSelectedAuth(provider)
        logger.info('login.done', { provider })
      })
      .catch((error: unknown) => {
        const message = error instanceof ApiError ? error.body.message : String(error)
        logger.error('login.failed', { provider, message })
        setLoginError(message)
        popup?.close()
      })
      .finally(() => setSigningIn(null))
  }, [])

  const signOut = useCallback((provider: string) => {
    setLoginError(null)
    logout(provider)
      .then(() => fetchAuth())
      .then(setAuth)
      .catch((error: unknown) => setLoginError(String(error)))
  }, [])

  const run = useCallback(
    (dryRun: boolean) => {
      if (!profile) {
        return
      }
      setBusy(true)
      setCallError(null)
      setLive(null)
      setTurns([])
      setTrace(null)

      const body: CallRequest = { profile: profile.name, auth: selectedAuth, dryRun }
      if (profile.kind === 'embedding') {
        body.input = input.split('\n').filter((line) => line.trim().length > 0)
        body.repeat = repeat
        body.includeVectors = includeVectors
      } else {
        body.prompt = prompt
      }
      if (token.length > 0) {
        body.token = token
      }

      call(body)
        .then((result) => {
          setOutcome(result)
          logger.info('call.done', {
            profile: result.profile,
            auth: result.auth,
            status: result.response?.http.status ?? null,
          })
        })
        .catch((error: unknown) => {
          if (error instanceof ApiError) {
            setCallError(error)
            setOutcome(null)
          } else {
            logger.error('call.failed', { message: String(error) })
          }
        })
        .finally(() => setBusy(false))
    },
    [profile, selectedAuth, token, prompt, input, repeat, includeVectors],
  )

  const stream = useCallback(() => {
    if (!profile) {
      return
    }
    setBusy(true)
    setCallError(null)
    setOutcome(null)
    setTurns([])
    setTrace(null)
    // Empty rather than null: the panel appears immediately, so a stream that
    // never produces a token is visibly a stream that never produced a token.
    setLive('')

    const body: CallRequest = { profile: profile.name, auth: selectedAuth, prompt }
    if (token.length > 0) {
      body.token = token
    }

    streamCall(body, (event) => {
      switch (event.event) {
        case 'open':
          logger.debug('stream.open', { status: event.status })
          break
        case 'delta':
          setLive((current) => (current ?? '') + event.text)
          break
        case 'done':
          setOutcome(event)
          logger.info('stream.done', {
            status: event.response?.http.status ?? null,
            ttftMs: event.response?.http.ttftMs ?? null,
            chunks: event.response?.stream?.chunks ?? null,
          })
          break
        case 'failed':
          setCallError(new ApiError(500, { code: event.code, message: event.message }))
          break
      }
    })
      .catch((error: unknown) => {
        if (error instanceof ApiError) {
          setCallError(error)
        } else {
          logger.error('stream.failed', { message: String(error) })
        }
      })
      .finally(() => setBusy(false))
  }, [profile, selectedAuth, token, prompt])

  const loop = useCallback(() => {
    if (!profile) {
      return
    }
    setBusy(true)
    setCallError(null)
    setOutcome(null)
    setLive(null)
    setTurns([])
    setTrace(null)

    const body: AgentRequest = {
      profile: profile.name,
      auth: selectedAuth,
      prompt,
      maxIterations,
    }
    if (token.length > 0) {
      body.token = token
    }

    runAgent(body, (event) => {
      switch (event.event) {
        case 'turn':
          setTurns((current) => [...current, event])
          break
        case 'done':
          setTrace(event)
          logger.info('agent.done', { turns: event.turns.length, stop: event.stop.outcome })
          break
        case 'failed':
          setCallError(new ApiError(500, { code: event.code, message: event.message }))
          break
      }
    })
      .catch((error: unknown) => {
        if (error instanceof ApiError) {
          setCallError(error)
        } else {
          logger.error('agent.failed', { message: String(error) })
        }
      })
      .finally(() => setBusy(false))
  }, [profile, selectedAuth, token, prompt, maxIterations])

  if (loadError) {
    return (
      <main className="mx-auto max-w-2xl p-6">
        <Panel title="mire is not answering">
          <p className="text-sm">{loadError}</p>
        </Panel>
      </main>
    )
  }

  if (!profiles || !auth) {
    return (
      <main className="p-6">
        <Spinner label="Loading configuration…" />
      </main>
    )
  }

  const expectUnauthorized =
    auth.providers.find((provider) => provider.name === selectedAuth)?.kind === 'anonymous'

  return (
    <div className="mx-auto max-w-6xl space-y-4 p-3 sm:p-6">
      <header className="flex flex-wrap items-baseline justify-between gap-2">
        <h1 className="font-semibold text-xl">mire</h1>
        <p className="text-stone-500 text-xs dark:text-stone-400">
          A known signal in, a look at what comes out.
        </p>
      </header>

      <Panel title="Auth">
        <AuthSelector
          providers={auth.providers}
          issues={auth.issues}
          selected={selectedAuth}
          token={token}
          signingIn={signingIn}
          loginError={loginError}
          onSelect={setSelectedAuth}
          onToken={setToken}
          onLogin={signIn}
          onLogout={signOut}
        />
      </Panel>

      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <div className="space-y-4">
          <Panel title="Profiles">
            <ProfileList
              profiles={profiles.profiles}
              issues={profiles.issues}
              selected={selectedProfile}
              onSelect={setSelectedProfile}
            />
          </Panel>
        </div>

        <div className="space-y-4">
          {profile ? (
            <RequestPanel
              profile={profile}
              prompt={prompt}
              input={input}
              repeat={repeat}
              includeVectors={includeVectors}
              busy={busy}
              onPrompt={setPrompt}
              onInput={setInput}
              onRepeat={setRepeat}
              onIncludeVectors={setIncludeVectors}
              maxIterations={maxIterations}
              onMaxIterations={setMaxIterations}
              onDryRun={() => run(true)}
              onSend={() => run(false)}
              onStream={stream}
              onLoop={loop}
            />
          ) : (
            <Panel title="Request">
              <p className="text-stone-500 text-sm dark:text-stone-400">
                Select a profile to get started.
              </p>
            </Panel>
          )}

          {busy && live === null ? <Spinner label="Calling…" /> : null}

          {live === null ? null : (
            <Panel
              title="Streaming"
              actions={
                busy ? (
                  <Badge tone="neutral">receiving…</Badge>
                ) : (
                  <Badge tone="good">complete</Badge>
                )
              }
            >
              {live.length === 0 ? (
                <p className="text-stone-500 text-sm dark:text-stone-400">
                  Connected. Nothing has arrived yet.
                </p>
              ) : (
                <p className="whitespace-pre-wrap text-sm">{live}</p>
              )}
            </Panel>
          )}

          {callError ? (
            <Panel
              title="mire could not run this call"
              actions={<Badge tone="bad">{callError.body.code}</Badge>}
            >
              <p className="text-sm">{callError.body.message}</p>
              {callError.body.detail ? (
                <pre className="mt-2 overflow-x-auto rounded bg-stone-100 p-2 font-mono text-xs dark:bg-stone-950">
                  {JSON.stringify(callError.body.detail, null, 2)}
                </pre>
              ) : null}
            </Panel>
          ) : null}

          <AgentPanel turns={turns} trace={trace} running={busy && turns.length > 0} />

          {outcome ? <RenderedRequest outcome={outcome} /> : null}
          {outcome ? (
            <ResponsePanel outcome={outcome} expectUnauthorized={expectUnauthorized} />
          ) : null}
        </div>
      </div>
    </div>
  )
}
