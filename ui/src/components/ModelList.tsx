import type { LoadIssue, ModelSummary } from '../api'
import { Badge } from './primitives'

export function ModelList({
  models,
  issues,
  selected,
  onSelect,
}: {
  models: ModelSummary[]
  issues: LoadIssue[]
  /** The id of the selected model — `name`, or `name@stage`. */
  selected: string | null
  onSelect: (id: string) => void
}) {
  return (
    <div className="space-y-3">
      <ul className="space-y-1">
        {models.map((model) => (
          <li key={model.id}>
            <button
              type="button"
              aria-current={model.id === selected ? 'true' : undefined}
              onClick={() => onSelect(model.id)}
              className={`w-full rounded border px-2 py-1.5 text-left transition-colors ${
                model.id === selected
                  ? 'border-line-strong bg-well'
                  : 'border-transparent hover:bg-well'
              }`}
            >
              <span className="flex items-center gap-2">
                <span className="truncate font-medium text-sm">{model.name}</span>
                {/*
                  One row per stage rather than a name with a picker beside it:
                  two stages of one file are two endpoints, with their own URL,
                  their own credential and their own answer, and the list is
                  where you pick which one you are asking. They sort together
                  under the name they share.
                */}
                {model.stage === undefined ? null : (
                  <span className="font-mono text-muted text-xs">@{model.stage}</span>
                )}
                <Badge tone={model.kind === 'embedding' ? 'warn' : 'neutral'}>{model.kind}</Badge>
                {model.hasDecode ? null : <Badge tone="warn">no decode</Badge>}
              </span>
              <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">
                {model.url}
              </span>
            </button>
          </li>
        ))}
      </ul>

      {models.length === 0 ? (
        <p className="text-muted text-xs">
          No model loaded. Drop a YAML file in the models directory — it is picked up without a
          restart.
        </p>
      ) : null}

      {issues.length > 0 ? (
        <div className="space-y-1">
          <h3 className="font-semibold text-muted text-xs">Files that did not load</h3>
          {issues.map((issue) => (
            <p key={`${issue.file}:${issue.message}`} className="text-xs">
              <span className="break-all font-mono text-bad">
                {issue.file}
                {issue.line === null ? '' : `:${issue.line}:${issue.column ?? 0}`}
              </span>
              <span className="block text-muted">{issue.message}</span>
            </p>
          ))}
        </div>
      ) : null}
    </div>
  )
}
