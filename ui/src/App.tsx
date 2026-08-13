import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  type AgentRequest,
  ApiError,
  type AuthResponse,
  type CallOutcome,
  type CallRequest,
  call,
  callbackUri,
  type Embedding,
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
} from './api'
import { ChatPanel } from './components/ChatPanel'
import { EmbeddingPanel } from './components/EmbeddingPanel'
import { EmbeddingRequest } from './components/EmbeddingRequest'
import { McpAuth } from './components/McpAuth'
import { McpProtocol } from './components/McpProtocol'
import { ModelAuth } from './components/ModelAuth'
import { ProfileList } from './components/ProfileList'
import { Panel, Spinner } from './components/primitives'
import { TrafficPanel } from './components/TrafficPanel'
import {
  activityItem,
  type ChatItem,
  callExchange,
  type Exchange,
  messageItem,
  setupExchanges,
  turnExchanges,
  verdictItem,
  wireMessages,
} from './conversation'
import { logger } from './logger'

const ANONYMOUS = 'anonymous'

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
  const [timeline, setTimeline] = useState<ChatItem[]>([])
  const [input, setInput] = useState('one\ntwo')
  const [repeat, setRepeat] = useState(1)
  const [includeVectors, setIncludeVectors] = useState(false)

  const [maxIterations, setMaxIterations] = useState(6)
  // `null` is auto: every server settles its own revision the way it always did.
  const [mcpProtocol, setMcpProtocol] = useState<string | null>(null)

  const [signingIn, setSigningIn] = useState<string | null>(null)
  // Carries the provider, because two places can start a login now and an error
  // shown against the wrong one is worse than no error at all.
  const [loginError, setLoginError] = useState<{ provider: string; message: string } | null>(null)

  const [busy, setBusy] = useState(false)
  const [live, setLive] = useState<string | null>(null)
  const [embedding, setEmbedding] = useState<Embedding | null>(null)
  const [exchanges, setExchanges] = useState<Exchange[]>([])
  const [callError, setCallError] = useState<ApiError | null>(null)

  // The history is the timeline, not a copy of it: the two cannot drift, so what
  // the transcript shows is exactly what the next request will carry.
  const messages = useMemo(() => wireMessages(timeline), [timeline])

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
    setEmbedding(null)

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
        setExchanges((current) => [...current, callExchange(result)])
        const decoded = result.response.decoded
        setEmbedding(decoded?.kind === 'embedding' ? decoded : null)
        logger.info('call.done', {
          profile: result.profile,
          auth: result.auth,
          status: result.response.http.status,
        })
      })
      .catch((error: unknown) => {
        if (error instanceof ApiError) {
          setCallError(error)
        } else {
          logger.error('call.failed', { message: String(error) })
        }
      })
      .finally(() => setBusy(false))
  }, [profile, token, input, repeat, includeVectors])

  /**
   * The turn about to be sent, appended to what came before.
   *
   * The question joins the transcript straight away rather than when the answer
   * lands — waiting for the endpoint to say something before showing what was
   * asked is how a chat window feels broken. Sending the history as it stands is
   * **Retry**'s job, not an empty box's.
   */
  const ask = useCallback((): Message[] => {
    const text = prompt.trim()
    if (text.length === 0) {
      return messages
    }

    const asked: Message = { role: 'user', content: text }
    setTimeline((current) => [...current, messageItem(asked)])
    setPrompt('')
    return [...messages, asked]
  }, [messages, prompt])

  const stream = useCallback(() => {
    if (!profile) {
      return
    }
    setBusy(true)
    setCallError(null)
    setEmbedding(null)
    // Empty rather than null: the bubble appears immediately, so a stream that
    // never produces a token is visibly a stream that never produced a token.
    setLive('')

    const sent = ask()
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
          setExchanges((current) => [...current, callExchange(event)])
          // The `done` event carries the same decoded answer the non-streaming
          // endpoint returns, so the conversation is built from that rather than
          // from the deltas: one source of truth, and it survives a stream whose
          // last chunk arrived in a shape the delta reader skipped.
          const answer = assistantTurn(event)
          if (answer) {
            setTimeline((current) => [...current, messageItem(answer)])
            setLive(null)
          }
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
  }, [profile, token, ask])

  /**
   * A chat profile, run in a loop over the history it is handed. This is what
   * **Send** and **Retry** both do, whether or not the profile declares a single
   * tool: a profile with nothing to call stops on turn one, which is the same one
   * turn a plain call would have made.
   *
   * It takes the history rather than reading it, because **Retry** shortens the
   * timeline and then runs it in the same breath — and `setTimeline` has not
   * landed by then.
   */
  const runChat = useCallback(
    (sent: Message[]) => {
      if (!profile) {
        return
      }
      setBusy(true)
      setCallError(null)
      setLive(null)
      setEmbedding(null)

      const body: AgentRequest = {
        profile: profile.name,
        messages: sent,
        maxIterations,
      }
      if (token.length > 0) {
        body.token = token
      }
      // Left out entirely on auto: the field's absence is what tells the server
      // to settle the revision itself, and sending a value it worked out anyway
      // would be a second opinion nobody asked for.
      if (mcpProtocol !== null) {
        body.mcpProtocol = mcpProtocol
      }

      runAgent(body, (event) => {
        switch (event.event) {
          case 'setup':
            // Before the first turn, because that is when it happened: a run
            // that never got past `initialize` has no turn to hang the reason
            // off.
            setExchanges((current) => [...current, ...setupExchanges(event.mcp)])
            break
          case 'turn':
            // Everything the turn put on a wire, in the order it left: the model
            // call, then each tool that answered it.
            setExchanges((current) => [...current, ...turnExchanges(event)])
            if (event.tools.length > 0) {
              setTimeline((current) => [...current, activityItem(event)])
            }
            break
          case 'done': {
            // Only the answer it finished on rejoins the history. The turns in
            // between are tool calls and their results; they are in the traffic
            // below, and replaying them without the results would break the next
            // call.
            const last = event.turns.at(-1)
            const answer = last ? assistantTurn(last.call) : null
            setTimeline((current) => [
              ...current,
              ...(answer ? [messageItem(answer)] : []),
              verdictItem(event),
            ])
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
    },
    [profile, token, maxIterations, mcpProtocol],
  )

  const send = useCallback(() => runChat(ask()), [runChat, ask])

  /**
   * That turn again, and nothing after it.
   *
   * A question is asked again as it stands — which is the whole point when the
   * call failed and left it sitting there unanswered. An answer is dropped
   * first, since asking again with it still in the history would only be asking
   * the model to agree with itself. Either way whatever followed it goes too:
   * the verdict of the run being replaced is no longer about anything.
   */
  const retry = useCallback(
    (id: string) => {
      const index = timeline.findIndex((item) => item.id === id)
      const item = timeline[index]
      if (item?.kind !== 'message') {
        return
      }
      const kept = timeline.slice(0, item.message.role === 'user' ? index + 1 : index)
      setTimeline(kept)
      runChat(wireMessages(kept))
    },
    [timeline, runChat],
  )

  const reset = useCallback(() => {
    setTimeline([])
    setLive(null)
    setCallError(null)
    setPrompt('')
  }, [])

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
  const chatting = profile !== undefined && profile.kind !== 'embedding'

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
              {/*
                Above the servers rather than on each one: the revision is a
                property of the run, and one trace speaking two of them is a
                result nobody could attribute.
              */}
              <McpProtocol
                revisions={mcp.revisions}
                selected={mcpProtocol}
                disabled={busy}
                onSelect={setMcpProtocol}
              />
              <McpAuth
                names={profile.mcp}
                servers={mcp.servers}
                providers={auth.providers}
                signingIn={signingIn}
                loginError={loginError}
                onLogin={signIn}
                onLogout={signOut}
              />
            </section>
          ) : null}
        </div>
      </Panel>

      {/*
        `min-w-0` on both columns: a grid child is `min-width: auto`, so a wide
        request body would widen the column and put the scrollbar on the page
        rather than on the block that is too wide.
      */}
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <div className="min-w-0 space-y-4">
          <Panel title="Profiles">
            <ProfileList
              profiles={profiles.profiles}
              issues={profiles.issues}
              selected={selectedProfile}
              onSelect={setSelectedProfile}
            />
          </Panel>
        </div>

        <div className="min-w-0 space-y-4">
          {profile === undefined ? (
            <Panel title="Request">
              <p className="text-stone-500 text-sm dark:text-stone-400">
                Select a profile to get started.
              </p>
            </Panel>
          ) : null}

          {chatting ? (
            <ChatPanel
              items={timeline}
              live={live}
              busy={busy}
              prompt={prompt}
              maxIterations={maxIterations}
              error={callError ? callError.body : null}
              onPrompt={setPrompt}
              onMaxIterations={setMaxIterations}
              onSend={send}
              onStream={stream}
              onRetry={retry}
              onReset={reset}
            />
          ) : null}

          {profile !== undefined && !chatting ? (
            <>
              <EmbeddingRequest
                input={input}
                repeat={repeat}
                includeVectors={includeVectors}
                busy={busy}
                onInput={setInput}
                onRepeat={setRepeat}
                onIncludeVectors={setIncludeVectors}
                onSend={embed}
              />

              {busy ? <Spinner label="Calling…" /> : null}

              {callError ? (
                <Panel title="mire could not run this call">
                  <p className="text-sm">{callError.body.message}</p>
                </Panel>
              ) : null}

              {embedding ? (
                <Panel title="Embedding">
                  <EmbeddingPanel embedding={embedding} />
                </Panel>
              ) : null}
            </>
          ) : null}

          <TrafficPanel
            exchanges={exchanges}
            expectUnauthorized={expectUnauthorized}
            onClear={() => setExchanges([])}
          />
        </div>
      </div>
    </div>
  )
}
