# API

`/docs` serves a Scalar reference against `/openapi.json`.

| Route | What it does |
| --- | --- |
| `GET /api/events` | Server-sent events: one `config` event per configuration reload, so a client can re-read the listings instead of showing what was there when it connected |
| `GET /api/config` | Every directory `mire` reads, how many entries each one provides and every file that did not load — the whole configuration in one request, and the only place `decodes/` issues appear |
| `GET /api/models` | Every model, plus the files that failed to load and why |
| `GET /api/models/{id}` | One model, as declared. With `Accept: application/yaml`, the same model as a `models/` file — stages substituted, named decodes flattened, defaults filled in — so that saving it there loads the same endpoint back |
| `GET /api/prompts` | Prompts declared in `prompts/`, plus the entries that did not load |
| `GET /api/auth` | Auth providers, with session status |
| `GET /api/mcp` | MCP servers declared in `mcp/` — what each one authenticates with, the hooks around its calls, and what it captures — plus the entries that did not load |
| `GET /api/mcp/{id}/tools` | Ask a server what it offers, right now, and on which revision |
| `POST /api/auth/{id}/login` | Start a browser login; returns where to send it |
| `POST /api/auth/{id}/logout` | Forget the session `mire` holds |
| `POST /api/call` | Render, authenticate, send, decode |
| `POST /api/call/stream` | The same, read chunk by chunk, with time to first token |
| `POST /api/agent` | The same, in a loop, served as server-sent events — every wire announced as it happens, and with `"stream": true` one chunk at a time as well |
| `POST /api/uploads` | Store one attached file; returns the id a call names it by |
| `GET /auth/callback` | Where the identity provider sends the browser back |
| `GET /healthz` | Liveness |

`/auth/callback` and `/healthz` are the two routes outside the OpenAPI document:
one is a page for a human, the other is ops plumbing. Neither is API surface.

Both streaming routes say what went out before saying what came back. `POST
/api/call/stream` opens with a `sent` event carrying the rendered request and its
`curl`, then `open`, then the deltas. `POST /api/agent` emits `setup` for the MCP
traffic the tool listing cost, then per turn a `sent`, a `delta` per chunk when
the run streams, and a `protocol`, `hook` or `tool` event each time one of those
lands — and closes the turn with a `turn` event repeating all of it. A client
that reads `turn` and `done` alone therefore loses nothing but the wait.

A `4xx` or `5xx` **from the endpoint under test** is a successful call: read
`response.http.status`. The API only returns an error when `mire` itself could
not do its job.

An `{id}` is an entry's `name`, or `name@stage` for a file declaring
[stages](configuration.md#stages) — the same string `"model"`, `"auth"` and
`"mcpServers"` take on a call. A bare name is the entry's default stage, so a
request written before stages existed still means what it meant. Listings carry
all three: `id` to send back, `name` to show, and `stage` when there is one.

```sh
curl -sS localhost:8787/api/call -H 'content-type: application/json' -d '{
  "model": "qwen3@prod",
  "auth": "keycloak-workload@preprod",
  "mcpServers": ["dev@local"],
  "prompt": "ping"
}'
```


## See exactly what was sent

Every call hands back the request it made, with its `curl` equivalent. This is
the thing you paste into a ticket:

```sh
curl -s localhost:8787/api/call \
  -H 'content-type: application/json' \
  -d '{"model": "qwen3", "prompt": "ping"}' | jq -r .curl
```

Credentials are masked in the `curl` export, in the request view, and in every
trace and log line.
