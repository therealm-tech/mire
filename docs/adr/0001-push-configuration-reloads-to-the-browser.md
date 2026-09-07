# 0001. Push configuration reloads to the browser over server-sent events

- **Status**: accepted
- **Date**: 2026-09-06

## Context

The process watches its configuration directories and swaps a whole new snapshot
in when a file changes. That is what makes the tool usable: you edit a model
file, and the next call uses it without a restart.

The browser was left out of it. The UI reads `GET /api/models`, `/api/prompts`,
`/api/auth` and `/api/mcp` once, when the tab is opened, and nothing after that —
so a page open while somebody edits `models/` shows the directory as it was at
open. In a tool whose whole use is editing those files all afternoon, that is the
state the page spends most of its life in.

The process is the only party that knows when a reload landed; the browser has no
way to find out short of asking. Everything the API serves today is a request the
browser made, so there is no existing path for the process to say anything on its
own. `POST /api/call/stream` and `POST /api/agent` already emit server-sent
events, and `api::sse` already wraps them so `aide` can document a stream — so
the framing, the wrapper and the client-side parser exist, all of them attached
to a POST.

## Decision

The process pushes. `ConfigStore` counts its reloads, and every reload that lands
is broadcast to whoever is listening; `GET /api/events` is a server-sent event
stream that emits one `config` event per reload, carrying the new generation. The
UI subscribes to it with `EventSource` and re-reads the four listings on each
event.

The event carries no configuration. It says that the directories were re-read and
nothing else — the listings stay the only place the contents come from.

The counter is per process, and the browser reconnects on its own, which is what
`EventSource` does with a GET.

## Consequences

The page follows the files. A file saved in the editor moves the model list, the
credential list and the load errors in the tab, which makes a broken YAML file
something you see rather than something you refresh for.

The UI now settles its remembered selections against the listings on every read
rather than only at startup: a model file deleted under an open tab has to be
handled exactly like one that was never there. That reconciliation was already
written for the mount path; it is now on a path that runs at arbitrary moments,
including mid-run.

Each open tab holds a connection for the life of the tab. It costs a task and a
subscriber on the server, and it needs a keep-alive to survive the notebook
proxies `--base-path` exists for — a stream that says nothing for minutes at a
time is exactly what an idle-timeout kills.

Because the event carries nothing, a tab acts on it with four requests, and two
saves in quick succession can leave two sets of them in flight at once. The
client keeps the newest and drops the rest; a read that straddles two generations
is corrected by the next announcement, which is guaranteed because every
generation is announced.

The counter resets when the process restarts, so a client that reconnects to a
lower number than it held is looking at a different process rather than at a
contradiction. A reconnection is therefore a reason to re-read, and a client that
compared generations to decide would get that case wrong.

`OpenAPI` describes the endpoint loosely — it can say a stream comes back and
what the frames mean in prose, not much more — which is the same limitation the
two streaming POST routes already carry.

## Alternatives considered

**Poll a generation endpoint.** A `GET` returning the current counter, called
every couple of seconds. Cheaper to write and it needs no new machinery, but it
trades latency and a permanent timer for about fifteen lines, when the process
knows the exact instant and the SSE wrapper is already in the tree.

**Put the configuration in the event.** It would save the four requests, at the
price of a second way to learn the configuration — and therefore a second way to
be wrong about it — plus diffing two snapshots on the server to decide what to
send. The four requests are made over loopback.

**A WebSocket.** Bidirectional, which nothing here needs: the browser has nothing
to say back that a request would not carry better. It would also give up the
automatic reconnection that comes free with `EventSource`, which is what lets a
tab left open across a restart of the process catch up by itself.

**Leave it manual.** A reload button, or F5. It is honest, and it is what the
page did — but it makes the person the transport for something the process
already knows.
