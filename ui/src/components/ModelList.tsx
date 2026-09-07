import type { ModelSummary } from '../api'
import { Badge, Button } from './primitives'

/**
 * The models of one file, in the stages it declares.
 *
 * `fallback` is where a row points before anybody picks: the entry a bare
 * `name` means, which is also the one another file's `auth:` or a hand-written
 * call reaches.
 */
type Group = {
  name: string
  fallback: ModelSummary
  entries: ModelSummary[]
}

/**
 * One row per file, not per stage.
 *
 * Two stages of a file are two endpoints — their own URL, their own credential,
 * their own answer — but they are two answers to the *same* question, and a
 * list that spelled them `qwen3@dev` and `qwen3@prod` on two rows made picking
 * an endpoint and picking where it runs look like one choice. So the name is the
 * row and the stages are buttons under it: what is on the row — the URL, the
 * badges — is the stage that is picked, because that is what **Send** would ask.
 */
function group(models: ModelSummary[]): Group[] {
  const groups = new Map<string, Group>()
  for (const model of models) {
    const existing = groups.get(model.name)
    if (existing === undefined) {
      groups.set(model.name, { name: model.name, fallback: model, entries: [model] })
      continue
    }
    existing.entries.push(model)
    if (model.isDefault) {
      existing.fallback = model
    }
  }
  return [...groups.values()]
}

export function ModelList({
  models,
  selected,
  stages,
  onSelect,
}: {
  models: ModelSummary[]
  /** The id of the selected model — `name`, or `name@stage`. */
  selected: string | null
  /** The stage last picked, per model name. See `stages` in `App`. */
  stages: Record<string, string>
  onSelect: (model: ModelSummary) => void
}) {
  const groups = group(models)

  return (
    <div className="space-y-3">
      <ul className="space-y-1">
        {groups.map((entry) => {
          // What the row stands for, in the order the answers are worth having:
          // the stage that is selected, else the one last picked here, else the
          // one a bare name means. A remembered stage the file no longer
          // declares matches nothing and falls through to the default.
          const active =
            entry.entries.find((model) => model.id === selected) ??
            entry.entries.find((model) => model.stage === stages[entry.name]) ??
            entry.fallback
          const on = active.id === selected

          return (
            <li key={entry.name}>
              {/*
                The card is a `div` rather than the button it used to be, because
                the stage buttons are buttons too and one cannot be nested in
                another. The row itself keeps the whole card as its hit area:
                `after:absolute after:inset-0` stretches it over the card's every
                pixel, and the stages — positioned, and later in the document —
                stay on top of that. So the card is clicked exactly as an
                unstaged one is, and it holds the same three things.
              */}
              <div
                className={`relative rounded border px-2 py-1.5 transition-colors ${
                  on ? 'border-line-strong bg-well' : 'border-transparent hover:bg-well'
                }`}
              >
                <button
                  type="button"
                  aria-current={on ? 'true' : undefined}
                  onClick={() => onSelect(active)}
                  className="block w-full text-left after:absolute after:inset-0"
                >
                  {/*
                    The spaces are for the accessible name, which is these three
                    spans run together: `flex` drops a whitespace-only child, and
                    without them the row announces itself as "qwen3chat".
                  */}
                  <span className="flex items-center gap-2">
                    <span className="truncate font-medium text-sm">{entry.name}</span>{' '}
                    <Badge tone={active.kind === 'embedding' ? 'warn' : 'neutral'}>
                      {active.kind}
                    </Badge>{' '}
                    {active.hasDecode ? null : <Badge tone="warn">no decode</Badge>}
                  </span>
                  <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">
                    {active.url}
                  </span>
                </button>

                {active.stage === undefined ? null : (
                  <fieldset className="relative mt-1">
                    <legend className="sr-only">Stages of {entry.name}</legend>
                    <div className="flex flex-wrap gap-1">
                      {entry.entries.map((model) => (
                        <Button
                          key={model.id}
                          variant={model.id === selected ? 'primary' : 'ghost'}
                          aria-pressed={model.id === selected}
                          onClick={() => onSelect(model)}
                          className="font-mono"
                        >
                          {model.isDefault ? (
                            <span aria-hidden="true" className="mr-1 opacity-60">
                              •
                            </span>
                          ) : null}
                          {model.stage}
                          {model.isDefault ? (
                            <>
                              {' '}
                              <span className="sr-only">(default)</span>
                            </>
                          ) : null}
                        </Button>
                      ))}
                    </div>
                  </fieldset>
                )}
              </div>
            </li>
          )
        })}
      </ul>

      {models.length === 0 ? (
        <p className="text-muted text-xs">
          No model loaded. Drop a YAML file in the models directory — it is picked up without a
          restart.
        </p>
      ) : null}
    </div>
  )
}
