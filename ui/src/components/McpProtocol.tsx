/**
 * Which revision this run speaks to its MCP servers.
 *
 * `mire` settles a revision per server, once, by asking — and that is the right
 * default, which is why **Auto** is one. But the revision is also a thing worth
 * testing on purpose: "does this endpoint still work on `2025-03-26`?" used to
 * mean editing `mcp.yaml`, restarting, running, and putting it back. Here it is a
 * dropdown, and the choice covers exactly one run.
 *
 * It applies to every server the profile names, because a run is the unit being
 * observed: mixing revisions across servers within one trace would produce a
 * result nobody could attribute. `mcp.yaml` is still where a per-server pin
 * belongs, and **Auto** is what leaves it in charge.
 */
export function McpProtocol({
  revisions,
  selected,
  disabled,
  onSelect,
}: {
  revisions: string[]
  /** `null` is auto — negotiate, or honour whatever `mcp.yaml` pinned. */
  selected: string | null
  disabled: boolean
  onSelect: (revision: string | null) => void
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <label className="font-medium text-muted text-xs" htmlFor="mcp-protocol">
        Protocol
      </label>
      <select
        id="mcp-protocol"
        value={selected ?? ''}
        disabled={disabled}
        onChange={(event) => onSelect(event.target.value === '' ? null : event.target.value)}
        className="rounded border border-line-strong bg-panel px-2 py-1 font-mono text-ink text-xs disabled:opacity-50"
      >
        <option value="">auto</option>
        {revisions.map((revision) => (
          <option key={revision} value={revision}>
            {revision}
          </option>
        ))}
      </select>
      <span className="text-[11px] text-faint">
        {selected === null
          ? 'Negotiated per server, unless mcp.yaml pins one.'
          : 'Stated outright: no probe, and a server that refuses it says so.'}
      </span>
    </div>
  )
}
