import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { z } from 'zod'
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
  fetchModels,
  fetchPrompts,
  logout,
  type McpResponse,
  type Message,
  type ModelsResponse,
  type PromptsResponse,
  runAgent,
  startLogin,
  type UploadedFile,
  uploadFile,
} from './api'
import { ChatPanel } from './components/ChatPanel'
import { EmbeddingPanel } from './components/EmbeddingPanel'
import { EmbeddingRequest } from './components/EmbeddingRequest'
import { Failure } from './components/Failure'
import { Mark } from './components/Mark'
import { McpPanel } from './components/McpPanel'
import { ModelList } from './components/ModelList'
import { Preflight } from './components/Preflight'
import { Badge, Button, Panel, Spinner } from './components/primitives'
import { TrafficPanel } from './components/TrafficPanel'
import {
  activityItem,
  type ChatItem,
  callExchange,
  type Exchange,
  type Live,
  messageItem,
  setupExchanges,
  turnExchanges,
  verdictItem,
  wireMessages,
} from './conversation'
import { download, exportFilename, runExport } from './export'
import { logger } from './logger'
import { activeServers } from './mcp'
import { useMediaQuery } from './media'
import { preflight } from './preflight'
import { usePersisted } from './storage'

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

/**
 * Whether this is the run being called off, rather than a run that went wrong.
 *
 * `fetch` rejects with an `AbortError` when its signal fires, and surfacing that
 * as a failure would be reporting the button somebody just pressed as a fault.
 *
 * The name is read off the object rather than the constructor: what arrives is a
 * `DOMException`, which is only *sometimes* an `instanceof Error` — not in jsdom,
 * and not across a realm boundary. A rejection that calls itself `AbortError` is
 * one, whatever it was built from.
 */
function abandoned(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    'name' in error &&
    (error as { name: unknown }).name === 'AbortError'
  )
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
    const entry = latest.providers.find((candidate) => candidate.id === provider)
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
      const settled = final.providers.find((candidate) => candidate.id === provider)
      if (settled?.session) {
        return final
      }
      throw new Error(settled?.lastError ?? 'the sign-in tab closed before a session appeared')
    }
  }

  throw new Error('gave up waiting for the sign-in to complete')
}

export function App() {
  const [models, setModels] = useState<ModelsResponse | null>(null)
  const [auth, setAuth] = useState<AuthResponse | null>(null)
  const [mcp, setMcp] = useState<McpResponse | null>(null)
  const [prompts, setPrompts] = useState<PromptsResponse | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)

  // Remembered across a reload, all of it small and none of it secret — see
  // `storage.ts` for what is deliberately left out, starting with the token.
  const [selectedModel, setSelectedModel] = usePersisted<string | null>(
    'model',
    z.string().nullable(),
    null,
  )
  const [token, setToken] = useState('')

  const [prompt, setPrompt] = usePersisted('prompt', z.string(), 'ping')
  const [timeline, setTimeline] = useState<ChatItem[]>([])
  const [input, setInput] = usePersisted('input', z.string(), 'one\ntwo')
  const [repeat, setRepeat] = usePersisted('repeat', z.number(), 1)
  const [includeVectors, setIncludeVectors] = usePersisted('vectors', z.boolean(), false)

  // The loop's budget, and the only thing that says how many turns a send is:
  // `1` is a single turn of the same mechanism, 6 is a loop with room to finish.
  // There is nothing beside it, because there is nothing else to ask.
  const [maxIterations, setMaxIterations] = usePersisted('maxTurns', z.number(), 6)
  // Off by default. Streaming is a second thing to get right — the framing, the
  // deltas, the endpoint actually chunking at all — and a first run should fail
  // for one reason at a time. It is also where tool calls stop reassembling, so a
  // loop that silently streamed would be a loop that silently stopped calling
  // tools.
  const [streaming, setStreaming] = usePersisted('stream', z.boolean(), false)
  // The servers switched on, and nothing is on to begin with. A tool call is the
  // one thing `mire` does that has effects outside this process — it really runs,
  // on somebody's real server — so a run reaches one because somebody said so in
  // this tab, never because a file was sitting in `mcp/`. Names are `mcp/`'s, so
  // this is global to the tab; a file's name rather than a stage's id, because
  // switching a server on is a statement about the server and it survives
  // changing which stage of it you were asking.
  const [mcpOn, setMcpOn] = usePersisted<string[]>('mcpOn', z.array(z.string()), [])
  // The stage last picked, per server name — the same memory as `stages` below,
  // for the same reason: a staged server is two endpoints, and coming back to
  // one is coming back to the stage the question was about.
  const [mcpStages, setMcpStages] = usePersisted<Record<string, string>>(
    'mcpStages',
    z.record(z.string(), z.string()),
    {},
  )
  // The stage last picked, per model name. Coming back to a model is coming back
  // to the endpoint you were asking, which is rarely its default one: the whole
  // reason to have stages is that `prod` is where the question was. A name whose
  // file no longer declares that stage falls back to the default rather than to
  // nothing, and the entry is simply never read again.
  const [stages, setStages] = usePersisted<Record<string, string>>(
    'stages',
    z.record(z.string(), z.string()),
    {},
  )

  const [signingIn, setSigningIn] = useState<string | null>(null)
  // Carries the provider, because two places can start a login now and an error
  // shown against the wrong one is worse than no error at all.
  const [loginError, setLoginError] = useState<{ provider: string; message: string } | null>(null)

  const [busy, setBusy] = useState(false)
  // The auth detail, which is a thing you read once. Shut by default so the box
  // you actually came to type in starts near the top of the page.
  // The servers, the same way and for the same reason: read when it is the
  // question, folded away when it is not.
  const [mcpOpen, setMcpOpen] = useState(false)
  const [stopped, setStopped] = useState(false)
  // Laptop or phone. The list is a column on one and a disclosure on the other,
  // which is two different sets of controls rather than two stylesheets.
  const wide = useMediaQuery('(min-width: 64rem)')
  const [picking, setPicking] = useState(false)
  // The exchange the transcript is pointing at, held only until the traffic has
  // jumped to it: it is an instruction, not a selection.
  const [revealed, setRevealed] = useState<string | null>(null)
  // Stable, because the traffic reads it from an effect: a fresh arrow every
  // render would be a fresh dependency every render.
  const clearReveal = useCallback(() => setRevealed(null), [])
  // Text and status together rather than side by side: they are one answer, and
  // two states would let the badge say `complete` over the previous call's
  // status for as long as it took the second to land.
  const [live, setLive] = useState<Live | null>(null)
  const [embedding, setEmbedding] = useState<Embedding | null>(null)
  const [exchanges, setExchanges] = useState<Exchange[]>([])
  const [callError, setCallError] = useState<ApiError | null>(null)

  // Files written to `mire`'s upload directory. Not persisted and not part of
  // the conversation: they are on a disk somewhere, and the list is a receipt
  // for that rather than a queue waiting to be sent.
  const [attachments, setAttachments] = useState<UploadedFile[]>([])
  const [attaching, setAttaching] = useState(false)
  const [attachError, setAttachError] = useState<ApiError | null>(null)

  // The history is the timeline, not a copy of it: the two cannot drift, so what
  // the transcript shows is exactly what the next request will carry.
  const messages = useMemo(() => wireMessages(timeline), [timeline])

  /**
   * The run in flight, so that it can be called off.
   *
   * A ref rather than state: nothing renders differently for holding it, and a
   * re-render between starting a call and aborting it would be a re-render that
   * loses the handle.
   */
  const running = useRef<AbortController | null>(null)

  /** Opens a run, replacing whatever the last one left behind. */
  const begin = useCallback((): AbortSignal => {
    running.current?.abort()
    const controller = new AbortController()
    running.current = controller
    setBusy(true)
    setStopped(false)
    setCallError(null)
    return controller.signal
  }, [])

  /** Closes it, however it ended. */
  const settle = useCallback(() => {
    running.current = null
    setBusy(false)
  }, [])

  /**
   * Stop waiting.
   *
   * The request is dropped where it stands and whatever arrived stays on the
   * page: a stream cut off after four tokens produced four tokens, and that is a
   * finding rather than a mess to clear up. Nothing is sent to call off the work
   * upstream — an endpoint that has been asked a question is going to answer it —
   * so this is about this tab, and says only that.
   */
  const stop = useCallback(() => {
    running.current?.abort()
    running.current = null
    setStopped(true)
    setBusy(false)
    logger.info('run.stopped', {})
  }, [])

  useEffect(() => {
    Promise.all([fetchModels(), fetchAuth(), fetchMcp(), fetchPrompts()])
      .then(([loadedModels, loadedAuth, loadedMcp, loadedPrompts]) => {
        setModels(loadedModels)
        setAuth(loadedAuth)
        setMcp(loadedMcp)
        setPrompts(loadedPrompts)
        // A remembered name is only good while the file behind it still is:
        // models are a directory somebody edits, and coming back to a
        // selection that no longer exists would be an empty page with no
        // explanation for it.
        setSelectedModel((current) => {
          const kept = loadedModels.models.some((entry) => entry.id === current)
          // The first model at its default stage, rather than the first entry:
          // the list is ordered by id, so falling back to `models[0]` would open
          // on whichever stage sorts first rather than on the one a bare name
          // means.
          const first = loadedModels.models.find((entry) => entry.isDefault)
          return kept ? current : (first?.id ?? loadedModels.models[0]?.id ?? null)
        })
        logger.info('config.loaded', {
          models: loadedModels.models.length,
          providers: loadedAuth.providers.length,
          servers: loadedMcp.servers.length,
          prompts: loadedPrompts.prompts.length,
        })
      })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error)
        logger.error('config.load_failed', { message })
        setLoadError(message)
      })
  }, [setSelectedModel])

  const model = models?.models.find((candidate) => candidate.id === selectedModel)

  /**
   * The identity this model calls with. `auth:` when it names one, otherwise
   * the anonymous provider that always exists — the same resolution the server
   * does, so what is shown is what goes out.
   *
   * The request bodies below leave `auth` out entirely rather than sending this:
   * the server would only work it out again, and a UI that sends it is a UI that
   * can disagree with the file.
   */
  const provider = auth?.providers.find((entry) => entry.id === (model?.auth ?? ANONYMOUS))

  /**
   * Whether this model takes a message somebody types.
   *
   * The model's own answer, so the composer follows the file rather than a
   * setting in this tab: `has_prompt: false` is a transcriber or a classifier
   * saying its input is the attachment, not a sentence. True while no model is
   * selected, which is the state where there is no composer to hide anything in.
   */
  const hasPrompt = model?.hasPrompt !== false

  /**
   * Whether this run has MCP servers to decide about at all.
   *
   * Not whether it reaches any — that is each switch's answer, and they start
   * off. Every send goes through the loop, so a server that is on is in the
   * picture whatever the turn budget: one turn against a real server is a fair
   * question — does the model ask for the tool `tools/list` showed it? — and
   * `max turns` is not the place to answer it.
   *
   * Only on a chat model: `kind: embedding` has no loop to be in, and the server
   * refuses one outright. False also with an empty `mcp/`, where there is
   * nothing to open a block on.
   */
  const usesMcp = model?.kind === 'chat' && (mcp?.servers.length ?? 0) > 0

  /**
   * The servers this run will actually set up, by id.
   *
   * One id per file — the stage that is picked, and only that one — minus
   * whatever is switched off, and empty when the run speaks to none of them at
   * all. Everything that describes the run reads this rather than the registry:
   * the bar above the box, and the list that goes out with the request.
   */
  const activeMcp = useMemo(
    () => (usesMcp && mcp ? activeServers(mcp.servers, mcpOn, mcpStages) : []),
    [usesMcp, mcp, mcpOn, mcpStages],
  )

  /** What the next call would do, and what would stop it. */
  const ready = useMemo(
    () =>
      model && auth && mcp
        ? preflight({
            model,
            provider,
            providers: auth.providers,
            servers: mcp.servers,
            token,
            mcpActive: activeMcp,
          })
        : null,
    [model, provider, auth, mcp, token, activeMcp],
  )

  /** Puts one server in or out of the next run. */
  const toggleMcp = useCallback(
    (name: string, on: boolean) => {
      setMcpOn((current) => {
        if (!on) {
          return current.filter((entry) => entry !== name)
        }
        return current.includes(name) ? current : [...current, name]
      })
    },
    [setMcpOn],
  )

  /** Which stage of one server the next run uses. */
  const pickMcpStage = useCallback(
    (name: string, stage: string) => {
      setMcpStages((current) => ({ ...current, [name]: stage }))
    },
    [setMcpStages],
  )

  // The one refusal the composer acts on rather than reports. Every other one is
  // a credential the server will refuse, and being refused is how you find out
  // that it does; this one has no call in it at all — a `requires_upload:` model
  // with nothing attached renders a request around a file that is not there — so
  // **Send** is shut until **Attach** has been pressed. Worked out here rather
  // than read off the bar: it is about what the request carries, which is the
  // composer's subject and not the bar's.
  const needsUpload = model?.requiresUpload === true && attachments.length === 0

  // The other refusal **Send** obeys rather than only reports. A run whose
  // identity has no session does not reach the endpoint at all — `mire` answers
  // `409 not_signed_in` itself and puts nothing on the wire — so pressing Send
  // buys the same sentence the bar is already showing, next to the button that
  // fetches the session. Every other red line is the endpoint's verdict to give,
  // and being refused by it is the answer you came for.
  const missingSignIn = ready?.rows.some((row) => row.fix?.kind === 'sign-in') ?? false

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
   * one send that does not go through the loop — and the reason `POST /api/call`
   * is still asked for from here at all.
   */
  const embed = useCallback(() => {
    if (!model) {
      return
    }
    const signal = begin()
    setLive(null)
    setEmbedding(null)

    const body: CallRequest = {
      model: model.id,
      input: input.split('\n').filter((line) => line.trim().length > 0),
      repeat,
      includeVectors,
    }
    if (token.length > 0) {
      body.token = token
    }

    call(body, signal)
      .then((result) => {
        setExchanges((current) => [...current, callExchange(result)])
        const decoded = result.response.decoded
        setEmbedding(decoded?.kind === 'embedding' ? decoded : null)
        logger.info('call.done', {
          model: result.model,
          auth: result.auth,
          status: result.response.http.status,
        })
      })
      .catch((error: unknown) => {
        if (abandoned(error)) {
          return
        }
        if (error instanceof ApiError) {
          setCallError(error)
        } else {
          logger.error('call.failed', { message: String(error) })
        }
      })
      .finally(settle)
  }, [model, token, input, repeat, includeVectors, begin, settle])

  /**
   * The turn about to be sent, appended to what came before.
   *
   * The question joins the transcript straight away rather than when the answer
   * lands — waiting for the endpoint to say something before showing what was
   * asked is how a chat window feels broken. Sending the history as it stands is
   * **Retry**'s job, not an empty box's.
   *
   * A model that takes no prompt sends the history untouched, whatever this
   * tab happens to be remembering: the box is hidden, and a sentence typed
   * against another model must not ride along invisibly on this one.
   */
  const ask = useCallback((): Message[] => {
    const text = prompt.trim()
    if (!hasPrompt || text.length === 0) {
      return messages
    }

    const asked: Message = { role: 'user', content: text }
    setTimeline((current) => [...current, messageItem(asked)])
    setPrompt('')
    return [...messages, asked]
  }, [hasPrompt, messages, prompt, setPrompt])

  /**
   * Every send: a chat model, run in a loop over the history it is handed.
   *
   * This is what **Send** and **Retry** do, whether or not the model declares a
   * single tool and whatever `max turns` says. A model with nothing to call
   * stops on turn one; a budget of one turn stops there too, and either way it is
   * the same model rendered into the same body — the count is the only thing
   * the composer changes.
   *
   * It takes the history rather than reading it, because **Retry** shortens the
   * timeline and then runs it in the same breath — and `setTimeline` has not
   * landed by then.
   */
  const runLoop = useCallback(
    (sent: Message[]) => {
      if (!model) {
        return
      }
      const signal = begin()
      // Empty rather than null when the turns are streamed, so the bubble is
      // there before the first token is: a turn that produces nothing is then
      // visibly a turn that produced nothing, rather than a **Thinking…** that
      // never resolves.
      setLive(streaming ? { text: '', status: null } : null)
      setEmbedding(null)

      const body: AgentRequest = {
        model: model.id,
        messages: sent,
        maxIterations,
      }
      // Sent either way rather than only when on: `POST /api/agent` reads a
      // whole answer by default, and the template is told what the run asked
      // for, not what this tab last remembered.
      body.stream = streaming
      if (token.length > 0) {
        body.token = token
      }
      if (attachments.length > 0) {
        body.uploads = attachments.map((file) => file.id)
      }
      // Always said, `[]` included. Leaving the field out asks for every server
      // `mcp/` declares, and this tab never means that: what a run reaches is
      // what somebody switched on, so the request carries it rather than
      // inheriting a default that would quietly set up the lot.
      if (usesMcp) {
        body.mcpServers = activeMcp
      }

      runAgent(
        body,
        (event) => {
          switch (event.event) {
            case 'setup':
              // Before the first turn, because that is when it happened: a run
              // that never got past `initialize` has no turn to hang the reason
              // off.
              setExchanges((current) => [...current, ...setupExchanges(event.mcp)])
              break
            case 'delta':
              // Deltas of the turn in flight, and only of that one: a turn that
              // has landed is on the transcript already, so the live bubble is
              // reset below rather than grown across the whole run.
              setLive((current) => ({ text: (current?.text ?? '') + event.text, status: null }))
              break
            case 'turn': {
              // Everything the turn put on a wire, in the order it left: the
              // model call, then each tool that answered it.
              const wires = turnExchanges(event)
              setExchanges((current) => [...current, ...wires])
              // The summary rows are built from those same exchanges rather
              // than from the event again, which is what lets a row name the
              // card it is a summary of.
              if (event.tools.length > 0) {
                setTimeline((current) => [...current, activityItem(event.index, wires)])
              }
              // This turn is written; the next one starts from an empty bubble
              // rather than appending to it. Nothing is lost — the text is in
              // the turn's own exchange below, and the answer the loop finished
              // on rejoins the transcript on `done`.
              //
              // The status goes with it. A turn that answered `400` is a turn
              // the trace below reports; carrying that number onto the bubble
              // the *next* turn is being written into would be labelling one
              // turn with another's answer.
              setLive((current) => (current === null ? null : { text: '', status: null }))
              break
            }
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
              if (answer) {
                // The answer is on the transcript now, so the half-written copy
                // of it goes: two of the same paragraph is one too many.
                setLive(null)
              } else {
                // Nothing decoded out of the last turn, so nothing was promoted
                // and the live bubble is the only thing left saying how the run
                // went — same as a streamed chat that came back with a refusal.
                setLive((current) =>
                  current === null
                    ? null
                    : { text: current.text, status: last?.call.response.http.status ?? null },
                )
              }
              logger.info('agent.done', { turns: event.turns.length, stop: event.stop.outcome })
              break
            }
            case 'failed':
              setCallError(new ApiError(500, { code: event.code, message: event.message }))
              break
          }
        },
        signal,
      )
        .catch((error: unknown) => {
          if (abandoned(error)) {
            return
          }
          if (error instanceof ApiError) {
            setCallError(error)
          } else {
            logger.error('agent.failed', { message: String(error) })
          }
        })
        .finally(settle)
    },
    [model, token, attachments, maxIterations, streaming, usesMcp, activeMcp, begin, settle],
  )

  const send = useCallback(() => runLoop(ask()), [runLoop, ask])

  /**
   * Writes the picked files to `mire`'s upload directory, one request each.
   *
   * Sequential rather than parallel, and it stops on the first refusal: these
   * all land in the same directory on somebody's machine, and firing ten at a
   * time to find out that the directory is not writable is ten answers to one
   * question. What went up before the failure stays on the list — it is up.
   *
   * Deliberately not tied to `begin()`: attaching is not a run, and a file going
   * up should not abort the call you are waiting on.
   */
  const attach = useCallback((files: File[]) => {
    setAttachError(null)
    setAttaching(true)
    void (async () => {
      try {
        for (const file of files) {
          const stored = await uploadFile(file)
          logger.info('upload.stored', { name: stored.name, size: stored.size })
          setAttachments((current) => [...current, stored])
        }
      } catch (error) {
        if (error instanceof ApiError) {
          setAttachError(error)
        } else {
          logger.error('upload.failed', { message: String(error) })
        }
      } finally {
        setAttaching(false)
      }
    })()
  }, [])

  /**
   * Takes a file off the list.
   *
   * Off the list, and nowhere else: the file stays where it was written. `mire`
   * does not delete things on somebody's disk on the strength of a click in a
   * browser tab, and a button that quietly did would be a worse surprise than
   * one that leaves a file behind.
   */
  const detach = useCallback((id: string) => {
    setAttachments((current) => current.filter((file) => file.id !== id))
  }, [])

  /**
   * That turn again, and nothing after it.
   *
   * A question is asked again as it stands — which is the whole point when the
   * call failed and left it sitting there unanswered. An answer is dropped
   * first, since asking again with it still in the history would only be asking
   * the model to agree with itself. Either way whatever followed it goes too:
   * the verdict of the run being replaced is no longer about anything.
   *
   * Any turn, not only the last: a run that went fine is the one worth asking
   * twice, and the history it needs is the history that came before it. What
   * came after is a different conversation, and it does not survive the branch.
   *
   * The tool calls the replaced answer made on the way out go with it. They are
   * rows about a run that is being re-run, and leaving them above the new one
   * would read as a run that called the same tool twice. They stay in the
   * traffic below, which is never truncated and is the actual record of what
   * this tab has done.
   */
  const retry = useCallback(
    (id: string) => {
      const index = timeline.findIndex((item) => item.id === id)
      const item = timeline[index]
      if (item?.kind !== 'message') {
        return
      }
      let end = item.message.role === 'user' ? index + 1 : index
      while (timeline[end - 1]?.kind === 'activity') {
        end -= 1
      }
      const kept = timeline.slice(0, end)
      setTimeline(kept)
      runLoop(wireMessages(kept))
    },
    [timeline, runLoop],
  )

  /**
   * The run, as a file.
   *
   * Built here rather than asked of the server, because the server was never
   * told: it answers one call at a time and keeps none of them, so this page is
   * the only place the run exists as a whole.
   */
  const exportRun = useCallback(() => {
    const at = new Date()
    const payload = runExport({
      model: model?.name ?? null,
      endpoint: model?.url ?? null,
      identity: provider?.name ?? null,
      messages,
      exchanges,
      at,
    })
    download(exportFilename(model?.name ?? null, at), JSON.stringify(payload, null, 2))
    logger.info('run.exported', { exchanges: exchanges.length, messages: messages.length })
  }, [model, provider, messages, exchanges])

  const reset = useCallback(() => {
    setTimeline([])
    setLive(null)
    setCallError(null)
    setPrompt('')
    // The list goes, the files stay. A new conversation is a clean composer, not
    // a licence to delete what was written to disk.
    setAttachments([])
    setAttachError(null)
  }, [setPrompt])

  if (loadError) {
    return (
      <main className="mx-auto max-w-2xl p-6">
        <Panel title="mire is not answering">
          <p className="text-sm">{loadError}</p>
        </Panel>
      </main>
    )
  }

  if (!models || !auth || !mcp || !prompts) {
    return (
      <main className="p-6">
        <Spinner label="Loading configuration…" />
      </main>
    )
  }

  // Sending nothing and being refused is the route proving it is protected.
  const expectUnauthorized = provider?.kind === 'anonymous'
  const chatting = model !== undefined && model.kind !== 'embedding'

  return (
    <div className="mx-auto max-w-6xl space-y-4 p-3 sm:p-6">
      <header className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2.5">
          <Mark />
          <h1 className="font-semibold text-xl tracking-tight">mire</h1>
        </div>
        <p className="text-faint text-xs">A known signal in, a look at what comes out.</p>
      </header>

      {/*
        `min-w-0` on both columns: a grid child is `min-width: auto`, so a wide
        request body would widen the column and put the scrollbar on the page
        rather than on the block that is too wide.
      */}
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <div className="min-w-0 space-y-4">
          {/*
            A column where there is room for one, and a fold-away where there is
            not: on a phone this list was a screenful to scroll past before
            reaching the thing it configures, every single time.
          */}
          <Panel
            title="Models"
            actions={
              wide ? undefined : (
                <Button aria-expanded={picking} onClick={() => setPicking((open) => !open)}>
                  {picking ? 'Hide' : 'Change'}
                </Button>
              )
            }
          >
            {wide || picking ? (
              <ModelList
                models={models.models}
                issues={models.issues}
                selected={selectedModel}
                stages={stages}
                onSelect={(picked) => {
                  setSelectedModel(picked.id)
                  const stage = picked.stage
                  if (stage !== undefined) {
                    setStages((current) => ({ ...current, [picked.name]: stage }))
                  }
                  setPicking(false)
                }}
              />
            ) : (
              // Folded away, the name and its stage rather than the `name@stage`
              // the call carries: the panel below is not the wire, and the two
              // are two things to read on the row that opens it.
              <p className="flex items-center gap-2">
                <span className="truncate font-medium text-sm">
                  {model?.name ?? selectedModel ?? 'None selected'}
                </span>
                {model?.stage === undefined ? null : <Badge>{model.stage}</Badge>}
              </p>
            )}
          </Panel>
        </div>

        <div className="min-w-0 space-y-4">
          {model === undefined ? (
            <Panel title="Request">
              <p className="text-muted text-sm">Select a model to get started.</p>
            </Panel>
          ) : null}

          {/*
            Above the box, because it is about the call that box is going to
            make. The identity is one of its lines rather than a panel hanging
            off it: there was nothing behind that button a line could not say,
            and the two things no file can hold — a credential typed into this
            tab, a browser session somebody has to go and fetch — are asked for
            where the refusal is reported.
          */}
          {ready ? (
            <Preflight
              state={ready}
              mcpOpen={mcpOpen}
              showMcp={usesMcp}
              token={token}
              signingIn={signingIn}
              onToken={setToken}
              onSignIn={signIn}
              onSignOut={signOut}
              onOpenMcp={() => setMcpOpen((open) => !open)}
            />
          ) : null}

          {/*
            Its own block, opened from its own button on the bar: which servers a
            run reaches, at which stage, and as whom is one question about the
            call that is about to happen, and it is not the one **Auth** answers.
            Shut by default like the other, because a run that reaches no server
            has nothing here to read.
          */}
          {usesMcp && mcpOpen ? (
            <McpPanel
              servers={mcp.servers}
              providers={auth.providers}
              on={mcpOn}
              stages={mcpStages}
              disabled={busy}
              signingIn={signingIn}
              loginError={loginError}
              onToggle={toggleMcp}
              onStage={pickMcpStage}
              onLogin={signIn}
              onLogout={signOut}
            />
          ) : null}

          {chatting ? (
            <ChatPanel
              items={timeline}
              live={live}
              busy={busy}
              expectUnauthorized={expectUnauthorized}
              stopped={stopped}
              prompt={prompt}
              prompts={prompts}
              hasPrompt={hasPrompt}
              maxIterations={maxIterations}
              streaming={streaming}
              error={callError ? callError.body : null}
              attachments={attachments}
              attaching={attaching}
              needsUpload={needsUpload}
              missingSignIn={missingSignIn}
              attachError={attachError ? attachError.body : null}
              onPrompt={setPrompt}
              onMaxIterations={setMaxIterations}
              onStreaming={setStreaming}
              onAttach={attach}
              onDetach={detach}
              onSend={send}
              onStop={stop}
              onRetry={retry}
              onReveal={setRevealed}
              onReset={reset}
            />
          ) : null}

          {model !== undefined && !chatting ? (
            <>
              <EmbeddingRequest
                input={input}
                prompts={prompts}
                repeat={repeat}
                includeVectors={includeVectors}
                busy={busy}
                missingSignIn={missingSignIn}
                onInput={setInput}
                onRepeat={setRepeat}
                onIncludeVectors={setIncludeVectors}
                onSend={embed}
                onStop={stop}
              />

              {busy ? <Spinner label="Calling…" /> : null}
              {stopped && !busy ? <Spinner label="Stopped." /> : null}

              {callError ? <Failure error={callError.body} /> : null}

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
            reveal={revealed}
            onRevealed={clearReveal}
            onExport={exportRun}
            onClear={() => setExchanges([])}
          />
        </div>
      </div>
    </div>
  )
}
