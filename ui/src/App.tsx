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
  fetchMcp,
  fetchProfiles,
  logout,
  type McpResponse,
  type Message,
  type ProfilesResponse,
  runAgent,
  startLogin,
  streamCall,
  type Trace,
  type Turn,
} from './api'
import { AgentPanel } from './components/AgentPanel'
import { ConversationPanel } from './components/ConversationPanel'
import { McpAuth } from './components/McpAuth'
import { ModelAuth } from './components/ModelAuth'
import { ProfileList } from './components/ProfileList'
import { Badge, Panel, Spinner } from './components/primitives'
import { RenderedRequest, RequestPanel } from './components/RequestPanel'
import { ResponsePanel } from './components/ResponsePanel'
import { logger } from './logger'

const ANONYMOUS = 'anonymous'

/**
 * The turn about to be sent, appended to what came before.
 *
 * A blank box is not an empty turn: it means "send the conversation as it
 * stands", which is how you retry a turn after removing one, or ask the same
 * history of a different profile.
 */
function withPrompt(history: Message[], prompt: string): Message[] {
  const text = prompt.trim()
  return text.length === 0 ? history : [...history, { role: 'user', content: text }]
}

/**
 * The model's turn, as the decoder saw it.
 *
 * `null` when there is nothing to record — a call that failed, or answered
 * neither text nor a tool call. Appending an empty assistant turn would put a
 * message on the next request that the endpoint never actually produced.
 */
function assistantTurn(outcome: CallOutcome): Message | null {
  const decoded = outcome.response.decoded
  if (decoded?.kind !== 'completion') {
    return null
  }

  const toolCalls = decoded.toolCalls
  if (!decoded.content && toolCalls.length === 0) {
    return null
  }

  return {
    role: 'assistant',
    ...(decoded.content ? { content: decoded.content } : {}),
    ...(toolCalls.length > 0 ? { toolCalls } : {}),
  }
}

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
  const [mcp, setMcp] = useState<McpResponse | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)

  const [selectedProfile, setSelectedProfile] = useState<string | null>(null)
  const [token, setToken] = useState('')

  const [prompt, setPrompt] = useState('ping')
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('one\ntwo')
  const [repeat, setRepeat] = useState(1)
  const [includeVectors, setIncludeVectors] = useState(false)

  const [maxIterations, setMaxIterations] = useState(6)

  const [signingIn, setSigningIn] = useState<string | null>(null)
  // Carries the provider, because two places can start a login now and an error
  // shown against the wrong one is worse than no error at all.
  const [loginError, setLoginError] = useState<{ provider: string; message: string } | null>(null)

  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<CallOutcome | null>(null)
  const [live, setLive] = useState<string | null>(null)
  const [turns, setTurns] = useState<Turn[]>([])
  const [trace, setTrace] = useState<Trace | null>(null)
  const [callError, setCallError] = useState<ApiError | null>(null)

  useEffect(() => {
    Promise.all([fetchProfiles(), fetchAuth(), fetchMcp()])
      .then(([loadedProfiles, loadedAuth, loadedMcp]) => {
        setProfiles(loadedProfiles)
        setAuth(loadedAuth)
        setMcp(loadedMcp)
        setSelectedProfile((current) => current ?? loadedProfiles.profiles[0]?.name ?? null)
        logger.info('config.loaded', {
          profiles: loadedProfiles.profiles.length,
          providers: loadedAuth.providers.length,
          servers: loadedMcp.servers.length,
        })
      })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error)
        logger.error('config.load_failed', { message })
        setLoadError(message)
      })
  }, [])

  const profile = profiles?.profiles.find((candidate) => candidate.name === selectedProfile)

  /**
   * The identity this profile calls with. `auth:` when it names one, otherwise
   * the anonymous provider that always exists — the same resolution the server
   * does, so what is shown is what goes out.
   *
   * The request bodies below leave `auth` out entirely rather than sending this:
   * the server would only work it out again, and a UI that sends it is a UI that
   * can disagree with the file.
   */
  const provider = auth?.providers.find((entry) => entry.name === (profile?.auth ?? ANONYMOUS))

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
        logger.info('login.done', { provider })
      })
      .catch((error: unknown) => {
        const message = error instanceof ApiError ? error.body.message : String(error)
        logger.error('login.failed', { provider, message })
        setLoginError({ provider, message })
        popup?.close()
      })
      .finally(() => setSigningIn(null))
  }, [])

  const signOut = useCallback((provider: string) => {
    setLoginError(null)
    logout(provider)
      .then(() => fetchAuth())
      .then(setAuth)
      .catch((error: unknown) => setLoginError({ provider, message: String(error) }))
  }, [])

  /**
   * One embedding call. There is no second turn of an embedding, so this is the
   * only mode that does not go through the loop.
   */
  const embed = useCallback(() => {
    if (!profile) {
      return
    }
    setBusy(true)
    setCallError(null)
    setLive(null)
    setTurns([])
    setTrace(null)

    const body: CallRequest = {
      profile: profile.name,
      input: input.split('\n').filter((line) => line.trim().length > 0),
      repeat,
      includeVectors,
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
          status: result.response.http.status,
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
  }, [profile, token, input, repeat, includeVectors])

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

    const sent = withPrompt(messages, prompt)
    const body: CallRequest = { profile: profile.name, messages: sent }
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
        case 'done': {
          setOutcome(event)
          // The `done` event carries the same decoded answer the non-streaming
          // endpoint returns, so the conversation is built from that rather than
          // from the deltas: one source of truth, and it survives a stream whose
          // last chunk arrived in a shape the delta reader skipped.
          const answer = assistantTurn(event)
          setMessages(answer ? [...sent, answer] : sent)
          setPrompt('')
          logger.info('stream.done', {
            status: event.response?.http.status ?? null,
            ttftMs: event.response?.http.ttftMs ?? null,
            chunks: event.response?.stream?.chunks ?? null,
          })
          break
        }
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
  }, [profile, token, prompt, messages])

  /**
   * A chat profile, run in a loop. This is what **Send** does, whether or not the
   * profile declares a single tool: a profile with nothing to call stops on turn
   * one, which is the same one turn a plain call would have made.
   */
  const send = useCallback(() => {
    if (!profile) {
      return
    }
    setBusy(true)
    setCallError(null)
    setOutcome(null)
    setLive(null)
    setTurns([])
    setTrace(null)

    const sent = withPrompt(messages, prompt)
    const body: AgentRequest = {
      profile: profile.name,
      messages: sent,
      maxIterations,
    }
    if (token.length > 0) {
      body.token = token
    }

    runAgent(body, (event) => {
      switch (event.event) {
        case 'turn':
          setTurns((current) => [...current, event])
          // The rendered request, the decode trace and the raw body are the point
          // of this tool, and they belong to a turn rather than to a run — so the
          // panels below follow the latest one as it lands.
          setOutcome(event.call)
          break
        case 'done': {
          setTrace(event)
          // Only the answer it finished on rejoins the conversation. The turns
          // in between are tool calls and their results; they are in the trace,
          // and replaying them without the results would break the next call.
          const last = event.turns.at(-1)
          const answer = last ? assistantTurn(last.call) : null
          setMessages(answer ? [...sent, answer] : sent)
          setPrompt('')
          logger.info('agent.done', { turns: event.turns.length, stop: event.stop.outcome })
          break
        }
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
  }, [profile, token, prompt, messages, maxIterations])

  if (loadError) {
    return (
      <main className="mx-auto max-w-2xl p-6">
        <Panel title="mire is not answering">
          <p className="text-sm">{loadError}</p>
        </Panel>
      </main>
    )
  }

  if (!profiles || !auth || !mcp) {
    return (
      <main className="p-6">
        <Spinner label="Loading configuration…" />
      </main>
    )
  }

  // Sending nothing and being refused is the route proving it is protected.
  const expectUnauthorized = provider?.kind === 'anonymous'

  return (
    <div className="mx-auto max-w-6xl space-y-4 p-3 sm:p-6">
      <header className="flex flex-wrap items-baseline justify-between gap-2">
        <h1 className="font-semibold text-xl">mire</h1>
        <p className="text-stone-500 text-xs dark:text-stone-400">
          A known signal in, a look at what comes out.
        </p>
      </header>

      <Panel title="Auth">
        <div className="space-y-3">
          <section className="space-y-2">
            <h3 className="font-semibold text-stone-600 text-xs dark:text-stone-400">
              Model endpoint
            </h3>
            <ModelAuth
              provider={provider}
              profile={profile}
              issues={auth.issues}
              token={token}
              signingIn={signingIn}
              loginError={loginError}
              onToken={setToken}
              onLogin={signIn}
              onLogout={signOut}
            />
          </section>

          {/*
            Its own section because it is its own question: the model's identity
            comes from the profile, a server's from `mcp.yaml`, and one says
            nothing about the other.
          */}
          {profile && profile.mcp.length > 0 ? (
            <section className="space-y-2 border-stone-200 border-t pt-3 dark:border-stone-800">
              <h3 className="font-semibold text-stone-600 text-xs dark:text-stone-400">
                MCP servers
              </h3>
              <McpAuth
                names={profile.mcp}
                servers={mcp.servers}
                providers={auth.providers}
                signingIn={signingIn}
                loginError={loginError}
                onLogin={signIn}
              />
            </section>
          ) : null}
        </div>
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
          {profile && profile.kind !== 'embedding' ? (
            <ConversationPanel
              messages={messages}
              busy={busy}
              onRemove={(index) =>
                setMessages((current) => current.filter((_, position) => position !== index))
              }
              onReset={() => setMessages([])}
            />
          ) : null}

          {profile ? (
            <RequestPanel
              profile={profile}
              prompt={prompt}
              turns={messages.length}
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
              onSend={profile.kind === 'embedding' ? embed : send}
              onStream={stream}
            />
          ) : (
            <Panel title="Request">
              <p className="text-stone-500 text-sm dark:text-stone-400">
                Select a profile to get started.
              </p>
            </Panel>
          )}

          {busy && live === null && turns.length === 0 ? <Spinner label="Calling…" /> : null}

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
