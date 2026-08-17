/**
 * Which of the profile's MCP servers this run is allowed to reach.
 *
 * The profile decides which servers exist for it — that is `mcp:`, opt-in and
 * never implied, because a tool call here really runs somewhere. This only ever
 * takes one *out* of the run in front of you, which is the question the file is
 * bad at answering: "does the model still get there without the search tool?",
 * "is this server the thing that has been failing for ten minutes?". Both used to
 * mean editing a profile and putting it back.
 *
 * Switching one off is not the same as it being idle. The run never discovers
 * it, never lists it, never signs in to it, and its tools are not offered to the
 * model — so what comes back is what the endpoint does without them.
 */
export function McpServers({
  names,
  off,
  disabled,
  onToggle,
}: {
  /** Every server the profile names, in the order it names them. */
  names: string[]
  /** The ones switched off. Names from other profiles are ignored, not shown. */
  off: string[]
  disabled: boolean
  onToggle: (name: string, on: boolean) => void
}) {
  const on = names.filter((name) => !off.includes(name))

  return (
    // A `fieldset`, because a set of related boxes is what one is for — but its
    // `legend` is laid out by the browser rather than by this row, and it lands
    // on a line of its own. So the legend names the group for a screen reader
    // and the row carries the label everybody else reads, in the flow, on the
    // same line as the boxes it introduces.
    <fieldset>
      <legend className="sr-only">Servers</legend>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="font-medium text-muted text-xs" aria-hidden="true">
          Servers
        </span>

        {names.map((name) => (
          <label key={name} className="flex items-center gap-1.5 font-mono text-ink text-xs">
            <input
              type="checkbox"
              checked={!off.includes(name)}
              disabled={disabled}
              onChange={(event) => onToggle(name, event.target.checked)}
              className="disabled:opacity-50"
            />
            {name}
          </label>
        ))}

        <span className="text-[11px] text-faint">
          {on.length === names.length
            ? 'All of them, as the profile declares. Untick one to leave it out of this run.'
            : on.length === 0
              ? 'None: nothing is set up, and the model is offered no live tool.'
              : `Off for this run: ${names.filter((name) => off.includes(name)).join(', ')}.`}
        </span>
      </div>
    </fieldset>
  )
}
