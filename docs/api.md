# API

`/docs` serves a Scalar reference against `/openapi.json`.

| Route | What it does |
| --- | --- |
| `GET /api/models` | Every model, plus the files that failed to load and why |
| `GET /api/models/{name}` | One model, as declared |
| `GET /api/prompts` | Prompts declared in `prompts/`, plus the entries that did not load |
| `GET /api/auth` | Auth providers, with session status |
| `GET /api/mcp` | MCP servers declared in `mcp/` — what each one authenticates with, the hooks around its calls, and what it captures — plus the entries that did not load |
| `GET /api/mcp/{name}/tools` | Ask a server what it offers, right now, and on which revision |
| `POST /api/auth/{name}/login` | Start a browser login; returns where to send it |
| `POST /api/auth/{name}/logout` | Forget the session `mire` holds |
| `POST /api/call` | Render, authenticate, send, decode |
| `POST /api/call/stream` | The same, read chunk by chunk, with time to first token |
| `POST /api/agent` | The same, in a loop, served as server-sent events — one turn at a time, and with `"stream": true` one chunk at a time as well |
| `POST /api/uploads` | Store one attached file; returns the id a call names it by |
| `GET /auth/callback` | Where the identity provider sends the browser back |
| `GET /healthz` | Liveness |

`/auth/callback` and `/healthz` are the two routes outside the OpenAPI document:
one is a page for a human, the other is ops plumbing. Neither is API surface.

A `4xx` or `5xx` **from the endpoint under test** is a successful call: read
`response.http.status`. The API only returns an error when `mire` itself could
not do its job.


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
