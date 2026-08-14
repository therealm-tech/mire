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
              className={`w-full rounded border px-2 py-1.5 text-left transition-colors ${
                profile.name === selected
                  ? 'border-line-strong bg-well'
                  : 'border-transparent hover:bg-well'
              }`}
            >
              <span className="flex items-center gap-2">
                <span className="truncate font-medium text-sm">{profile.name}</span>
                <Badge tone={profile.kind === 'embedding' ? 'warn' : 'neutral'}>
                  {profile.kind}
                </Badge>
                {profile.hasDecode ? null : <Badge tone="warn">no decode</Badge>}
              </span>
              <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">
                {profile.url}
              </span>
            </button>
          </li>
        ))}
      </ul>

      {profiles.length === 0 ? (
        <p className="text-muted text-xs">
          No profile loaded. Drop a YAML file in the profiles directory — it is picked up without a
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
