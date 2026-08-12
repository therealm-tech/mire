import type { LoadIssue, ProfileSummary } from '../api'
import { Badge } from './primitives'

export function ProfileList({
  profiles,
  issues,
  selected,
  onSelect,
}: {
  profiles: ProfileSummary[]
  issues: LoadIssue[]
  selected: string | null
  onSelect: (name: string) => void
}) {
  return (
    <div className="space-y-3">
      <ul className="space-y-1">
        {profiles.map((profile) => (
          <li key={profile.name}>
            <button
              type="button"
              aria-current={profile.name === selected ? 'true' : undefined}
              onClick={() => onSelect(profile.name)}
              className={`w-full rounded border px-2 py-1.5 text-left ${
                profile.name === selected
                  ? 'border-stone-400 bg-stone-100 dark:border-stone-600 dark:bg-stone-800'
                  : 'border-transparent hover:bg-stone-100 dark:hover:bg-stone-800'
              }`}
            >
              <span className="flex items-center gap-2">
                <span className="truncate font-medium text-sm">{profile.name}</span>
                <Badge tone={profile.kind === 'embedding' ? 'warn' : 'neutral'}>
                  {profile.kind}
                </Badge>
                {profile.hasDecode ? null : <Badge tone="warn">no decode</Badge>}
              </span>
              <span className="mt-0.5 block truncate font-mono text-[11px] text-stone-500 dark:text-stone-400">
                {profile.url}
              </span>
            </button>
          </li>
        ))}
      </ul>

      {profiles.length === 0 ? (
        <p className="text-stone-500 text-xs dark:text-stone-400">
          No profile loaded. Drop a YAML file in the profiles directory — it is picked up without a
          restart.
        </p>
      ) : null}

      {issues.length > 0 ? (
        <div className="space-y-1">
          <h3 className="font-semibold text-stone-600 text-xs dark:text-stone-400">
            Files that did not load
          </h3>
          {issues.map((issue) => (
            <p key={`${issue.file}:${issue.message}`} className="text-xs">
              <span className="break-all font-mono text-rose-700 dark:text-rose-300">
                {issue.file}
                {issue.line === null ? '' : `:${issue.line}:${issue.column ?? 0}`}
              </span>
              <span className="block text-stone-600 dark:text-stone-400">{issue.message}</span>
            </p>
          ))}
        </div>
      ) : null}
    </div>
  )
}
