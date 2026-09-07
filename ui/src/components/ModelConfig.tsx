import { type ReactNode, useState } from 'react'
import type { ModelConfig as Config, ErrorBody } from '../api'
import { JsonTree } from './JsonTree'
import { Badge, Code, CopyButton, Panel, Spinner } from './primitives'

/** Which reading of the model is on screen. */
type Lens = 'blocks' | 'yaml'

/**
 * `mire`'s own agent defaults, for a model that declares no `agent:` block.
 *
 * Kept in step by hand with `default_spec` in `src/agent.rs` — the API answers
 * `agent: null` there, because the file really does declare nothing, and the
 * loop's own defaults are not a property of the model.
 */
const DEFAULT_MAX_TURNS = 10

/**
 * What the endpoint under test actually is, in the three blocks that decide a
 * call: what goes out, how the answer is read, when the loop stops.
 *
 * There is no summary table on purpose. Where the call goes and who it goes as
 * are on the preflight bar this block opens from, one line above; repeating them
 * here would spend the first screenful on what has just been read.
 *
 * Everything shown is **resolved rather than copied**: `${ stage.… }` is already
 * substituted, and the named shapes of `decode.from` are already flattened into
 * the cascades below. That is the whole reason to open it — the file says what
 * was typed, this says what the endpoint is.
 */
export function ModelConfig({
  id,
  config,
  document,
  error,
}: {
  /** The model asked for, `name` or `name@stage`. */
  id: string
  /** `null` until the fetch lands. */
  config: Config | null
  /** The same model as a `models/` file, `null` until it lands. */
  document: string | null
  error: ErrorBody | null
}) {
  const [lens, setLens] = useState<Lens>('blocks')

  return (
    <Panel
      title={`Configuration · ${id}`}
      actions={
        <span className="flex items-center gap-1">
          <div role="tablist" className="flex gap-1">
            {(
              [
                ['blocks', 'Blocks'],
                ['yaml', 'YAML'],
              ] as const
            ).map(([key, label]) => (
              <button
                key={key}
                type="button"
                role="tab"
                aria-selected={lens === key}
                onClick={() => setLens(key)}
                className={`rounded px-2 py-0.5 text-[11px] transition-colors ${
                  lens === key
                    ? 'border border-line bg-well text-ink'
                    : 'border border-transparent text-faint hover:text-ink'
                }`}
              >
                {label}
              </button>
            ))}
          </div>
          {document === null ? null : <CopyButton text={document} label="Copy YAML" />}
        </span>
      }
    >
      {error ? (
        <p className="text-bad text-sm">{error.message}</p>
      ) : config === null || document === null ? (
        <Spinner label="Reading the configuration…" />
      ) : lens === 'yaml' ? (
        <Document text={document} />
      ) : (
        <div className="space-y-1">
          <Request request={config.request} />
          <Decode decode={config.decode} />
          <Agent agent={config.agent ?? null} />
        </div>
      )}
    </Panel>
  )
}

/**
 * One foldable section, open to begin with.
 *
 * Open including the blocks a model declares nothing for: "no `agent:`" is not
 * the answer somebody came for, "so it stops on the first turn with no tool
 * call, and never runs past ten" is. `meta` is what the header keeps saying once
 * a block is folded, so closing one still leaves an answer rather than a title.
 */
function Block({
  title,
  meta,
  muted = false,
  children,
}: {
  title: string
  meta?: ReactNode
  /** A block the model does not declare: still listed, because absent is an answer. */
  muted?: boolean
  children: ReactNode
}) {
  const [open, setOpen] = useState(true)

  return (
    <section className={`border-line border-t pt-2 first:border-t-0 ${muted ? 'opacity-60' : ''}`}>
      <h3>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((was) => !was)}
          className="flex w-full items-center gap-2 text-left"
        >
          <span aria-hidden="true" className="text-[10px] text-faint">
            {open ? '▾' : '▸'}
          </span>
          <span className="font-medium font-mono text-xs">{title}</span>
          {meta === undefined ? null : <span className="text-faint text-xs">{meta}</span>}
        </button>
      </h3>
      {open ? <div className="mt-2">{children}</div> : null}
    </section>
  )
}

/** What the call sends: one of a template, a script, or a form. */
function Request({ request }: { request: Config['request'] }) {
  if (request.template !== undefined) {
    return (
      <Block title="request.template" meta="MiniJinja">
        <Code>{request.template}</Code>
      </Block>
    )
  }
  if (request.script !== undefined) {
    return (
      <Block title="request.script" meta="Rhai">
        <Code>{request.script}</Code>
      </Block>
    )
  }
  if (request.multipart !== undefined) {
    return (
      <Block title="request.multipart" meta="one entry per form field">
        <div className="font-mono text-xs">
          <JsonTree value={request.multipart} />
        </div>
      </Block>
    )
  }
  return (
    <Block title="request" muted meta={<Badge tone="warn">nothing declared</Badge>}>
      <p className="text-muted text-xs">
        This model declares no template, script or form, so there is no body to send.
      </p>
    </Block>
  )
}

/** Every cascade of the decode, in the order each one is tried. */
function Decode({ decode }: { decode: Config['decode'] }) {
  const cascades =
    decode === undefined
      ? []
      : (
          [
            ['content', decode.content],
            ['delta', decode.delta],
            ['tool_calls', decode.tool_calls],
            ['finish_reason', decode.finish_reason],
            ['usage', decode.usage],
            ['vectors', decode.vectors],
            ['error', decode.error],
          ] as const
        ).filter(([, paths]) => paths.length > 0)

  if (decode?.script !== undefined) {
    return (
      <Block title="decode.script" meta="Rhai — it replaces the cascades entirely">
        <Code>{decode.script}</Code>
      </Block>
    )
  }

  if (decode === undefined || cascades.length === 0) {
    return (
      <Block title="decode" muted meta={<Badge tone="warn">nothing configured</Badge>}>
        <p className="text-muted text-xs">
          Nothing is read out of the response yet, so a call comes back as a raw body and a decode
          trace saying every path missed.
        </p>
      </Block>
    )
  }

  return (
    <Block
      title="decode"
      meta={decode.from.length === 0 ? undefined : `from: ${decode.from.join(', ')}`}
    >
      <dl className="space-y-1.5">
        {cascades.map(([field, paths]) => (
          <div key={field} className="grid gap-x-3 sm:grid-cols-[8rem_1fr]">
            <dt className="font-medium text-muted text-xs">{field}</dt>
            <dd className="min-w-0">
              {paths.map((path, index) => (
                <span
                  key={path}
                  className={`block truncate font-mono text-xs ${
                    index === 0 ? 'text-ink' : 'text-faint'
                  }`}
                >
                  {path}
                </span>
              ))}
            </dd>
          </div>
        ))}
      </dl>
      {decode.from.length === 0 ? null : (
        <p className="mt-2 text-faint text-xs">
          Flattened when the model loaded: the paths a named shape brings are these, and the trace
          of a call names whichever of them won.
        </p>
      )}
    </Block>
  )
}

/**
 * When the loop stops.
 *
 * The conditions are an *or* — the run ends on the first one that trips — so
 * they are laid out side by side rather than nested under `stop_when`, which is
 * a shape to recompose in your head before it means anything.
 */
function Agent({ agent }: { agent: NonNullable<Config['agent']> | null }) {
  const stop = agent?.stop_when
  const conditions: string[] = []

  if (stop === undefined || stop.no_tool_calls) {
    conditions.push('a turn asks for no tool')
  }
  if (stop !== undefined && stop.finish_reason_in.length > 0) {
    conditions.push(`finish_reason is ${stop.finish_reason_in.join(' or ')}`)
  }
  if (stop?.repeated_call) {
    conditions.push('the same tool is called twice with the same arguments')
  }
  conditions.push(`${agent?.default_max_turns ?? DEFAULT_MAX_TURNS} turns`)
  if (agent?.max_duration_ms != null) {
    conditions.push(`${agent.max_duration_ms / 1000}s of wall clock`)
  }

  return (
    <Block
      title="agent"
      muted={agent === null}
      meta={agent === null ? 'not declared — the loop runs on the defaults' : 'the loop stops on'}
    >
      <ul className="flex flex-wrap gap-1.5">
        {conditions.map((condition) => (
          <li key={condition} className="rounded border border-line px-2 py-0.5 text-xs">
            {condition}
          </li>
        ))}
      </ul>
      <p className="mt-2 text-faint text-xs">
        Whichever comes first. The turn count is a ceiling as much as a default — the composer can
        ask for fewer turns, never for more.
      </p>
    </Block>
  )
}

/** The model as the file it would be. */
function Document({ text }: { text: string }) {
  return (
    <>
      <Code>{text}</Code>
      <p className="mt-2 text-faint text-xs">
        Saved under <span className="font-mono">models/</span> this loads the same endpoint back:
        the stage is already resolved, so it is also how a stage becomes a model of its own.
      </p>
    </>
  )
}
