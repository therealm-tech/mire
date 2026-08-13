# mire

A test pattern for model endpoints. You put a known signal in, and you look at
what comes out.

You deployed a model, or you changed a route, and you want to know four things:
does the endpoint answer, is the auth actually enforced, is the response shaped
the way you expect, does tool calling work. Today that is a copy-pasted `curl`.
This is the same thing, reproducible, with the credential handling that a browser
tab cannot give you.

`mire` is a single binary you run yourself, next to your work. No chart, no
cluster, nothing deployed for anybody else — there is a container image, but it
is the same binary with the same lifetime, for when a notebook is easier to hand
an image than a binary. It listens on localhost, serves a small web UI and an
HTTP API, and makes the outbound calls on your behalf — which is why there is no
CORS to fight and why workload identities stay testable.

## Install and run

```sh
(cd ui && npm install && npm run build)
cargo build --release
./target/release/mire --profiles ./profiles
```

The UI is built into the binary, so that is the whole deployment. Building
without the front end works too — you get a placeholder page and a fully
functional API.

It listens on `127.0.0.1:8787` and prints its URL on startup. Every option is
also an environment variable:

| Flag | Variable | Default | What it does |
| --- | --- | --- | --- |
| `--profiles` | `PROFILES_DIR` | `./profiles` | Directory of profile YAML files |
| `--host` | `HOST` | `127.0.0.1` | Listen address; widening it is deliberate |
| `--port` | `PORT` | `8787` | Listen port |
| `--base-path` | `BASE_PATH` | *(none)* | Path prefix, when a proxy forwards one — see [below](#from-a-notebook-behind-a-path-proxy) |
| `--public-url` | `PUBLIC_URL` | *(none)* | Origin the browser sees, for the OIDC callback |
| `--ca-bundle` | `CA_BUNDLE` | *(none)* | PEM bundle of extra trusted CAs |
| `--log-filter` | `LOG_FILTER` | `info` | `tracing` filter, e.g. `mire=debug` |

### In a container

```sh
docker build -t mire:0.1.0 .
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/profiles:/etc/mire/profiles:ro" mire:0.1.0
```

One static binary on `distroless/static` — a certificate bundle, timezone data,
`/etc/passwd`, and nothing else. No shell, no package manager, and nothing to
patch. It runs as UID 65532 and never writes, so `--read-only` costs nothing.

The profiles are **mounted, not baked in**. They are the input to the tool, not
part of it: an image carrying them would ship endpoints pointing at somebody
else's laptop, and testing a new endpoint would mean building a new image.
`/etc/mire/profiles` exists in the image so a run without a mount starts cleanly
with nothing to offer.

`HOST` defaults to `0.0.0.0` here, and only here. Inside a container, loopback is
a network nobody else can reach, so a published port would answer nothing. The
isolation is the container's to provide — publish to `127.0.0.1:8787` and the
exposure is the same as running the binary directly.

For an internal CA, mount the bundle and point `--ca-bundle` at it:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/profiles:/etc/mire/profiles:ro" \
  -v /etc/ssl/certs/internal.pem:/etc/mire/ca.pem:ro \
  -e CA_BUNDLE=/etc/mire/ca.pem mire:0.1.0
```

There is no `HEALTHCHECK`: the image holds no HTTP client to run one with, and
adding one would mean adding a shell back. Point your orchestrator at `/healthz`
instead — that is the same check, run by something that already has a client.

### From a notebook behind a path proxy

Notebook proxies serve you at something like
`/notebook/<namespace>/<name>/proxy/8787/`, and they come in two kinds. **Which
one you have decides whether you want `--base-path`, and getting it wrong is the
one configuration mistake that produces a genuinely cryptic error** — so find out
first:

```sh
mire --profiles ./profiles --log-filter 'mire=debug,tower_http=debug'
```

Load the page through the proxy and read the `uri=` field of the request log.

**If the prefix is forwarded** (`uri=/notebook/my-namespace/my-notebook/proxy/8787/`),
tell `mire` about it and every route moves under it:

```sh
mire --profiles ./profiles --base-path /notebook/my-namespace/my-notebook/proxy/8787
```

Everything moves together — API, `/docs`, `/healthz` and the UI. The server
injects a matching `<base href>` into `index.html`, so the bundle's relative
asset URLs and the UI's own `fetch` calls resolve under the prefix. Both
`…/8787` and `…/8787/` serve the page, because proxies disagree about the
trailing slash. Hitting the root without the prefix redirects you to it rather
than 404-ing.

**If the prefix is stripped** (`uri=/` — Kubeflow's notebook proxy does this),
`mire` really is mounted at the root: use **no** `--base-path` at all. Setting
one then hides every route behind a prefix the proxy has already removed, and
you get a redirect to a URL the proxy strips again. With no prefix configured,
no `<base href>` is injected either, and that is what makes it work: the
bundle's URLs are relative, so they resolve against the document's own URL —
the prefixed one your browser is on — and come back through the proxy. This
needs the page URL to end in a slash (`…/proxy/8787/`, not `…/proxy/8787`);
such proxies redirect to add it, but if yours does not, use the slash.

The symptom of getting this backwards is
`Failed to load module script: … MIME type of "text/html"`. It means the browser
resolved `assets/index-<hash>.js` to somewhere that is not `mire` — usually the
cluster root, where the ingress answers with its own HTML page. The `uri=` log
tells you in one line: no request for the asset at all means it never reached
`mire`.

If the proxy needs to reach `mire` on something other than loopback, widen the
listen address explicitly with `--host 0.0.0.0`. That is a choice, not a default.

## The UI

Deliberately small. It does not edit anything — the profiles are yours and your
editor's — and it holds no logic of its own: it shows what the API returns.

- **Auth selector, above everything else.** Replaying the same request across
  every mode is the central move, so switching is one click and changes nothing
  else. A `401` asked anonymously shows up green, with a note saying the route is
  protected, because that is a pass. A provider that signs in through a browser
  carries a dot for its session state, and a **Sign in** button that says who you
  are once you are back.
- **Request.** A prompt for a chat profile, one text per line for an embedding
  one. **Dry run** renders without sending; **Send** goes out.
- **Rendered request**, with a *Copy as curl* button and credentials masked.
- **Response.** The decoded content, tool calls, finish reason and usage, then
  the decode trace — which path matched, which missed, what was tried — and the
  raw JSON as a collapsible tree.
- **Embedding.** Count, width, encoding, the five checks, and per vector its
  norm, a sample of the first values and a distribution histogram. Never a wall
  of floats.

A credential typed into the UI lives in that tab and nowhere else: it is sent
with the call and never stored, never logged, never echoed back. A credential
`mire` fetched for you never reaches the tab at all — the browser sees a
username, the granted scopes and a countdown.

## A stack to point it at

`docker-compose.yaml` brings up everything needed to exercise `mire` for real:
a model runtime, an identity provider, and a door in front of the model so that
"is this route actually protected?" has an answer.

```sh
docker compose up -d          # first run pulls ~800 MB of models
export OIDC_CLIENT_SECRET=mire-dev-secret
export MODEL_TOKEN=anything   # the gateway only checks that a credential exists
mire
```

The profiles in `./profiles` — the default directory — point at that stack, so
there is nothing to pass.

| Service | Port | What it is |
| --- | --- | --- |
| `ollama` | 11434 | The models. Unauthenticated, on purpose |
| `gateway` | 11435 | nginx in front of Ollama, rejecting requests with no credential |
| `mcp` | 11436 | A minimal MCP server on `2026-07-28`, so agent mode has something real to call |
| `mcp-legacy` | 11437 | The same server on `2025-06-18`, so the revision negotiation has something to negotiate with |
| `keycloak` | 8080 | Realm `mire`: `mire-workload` (service account) and `mire-ui` (browser login, user `mire` / `mire`) |

Models, as of August 2026 — these rankings move monthly, so revisit the choice:

- **`qwen3:0.6b-q4_K_M`** (523 MB, 40K context). The smallest tag that still does
  tool calling, which agent mode will need. `gemma3:270m` is lighter (~292 MB)
  but cannot call tools.
- **`nomic-embed-text:v1.5`** (274 MB, 768 dimensions, MTEB 62.4). The most
  pulled embedding model in the Ollama library, and it runs on a CPU.
  `qwen3-embedding:0.6b` scores higher (70.7 on MTEB-eng-v2) at 639 MB.

Six profiles come with it, each showing one thing:

- **`qwen3-chat`** — Ollama's OpenAI-compatible endpoint. The obvious one.
- **`qwen3-native`** — Ollama's *own* chat API. Content at `$.message.content`,
  stop reason called `done_reason`, token counters at the top level instead of
  under `usage`. Its `decode:` block is byte-for-byte identical to
  `qwen3-chat`'s: the cascades absorb the difference, and the trace tells you
  which path won. This is the "endpoint that does not answer like OpenAI" you
  can actually run.
- **`qwen3-guarded`** — the same model behind the gateway. Replay it across the
  four auth modes and you get the matrix this tool exists for:

  ```
  anonymous          -> 401   the route is protected — a pass
  static-token       -> 200
  keycloak-workload  -> 200   a token fetched for a service account
  keycloak-user      -> 200   a token fetched for you, after signing in
  ```

- **`nomic-embed`** — embeddings, with `$.data[*].embedding` deliberately kept
  first in the cascade so you can watch it miss and `$.embeddings` take over.
- **`qwen3-scripted`** — the same model decoded by a Rhai script instead of
  cascades: it strips a reasoning block and turns Ollama's nanosecond counters
  into tokens per second, neither of which a path can do.
- **`qwen3-mcp`** — agent mode against the MCP server, with tools that are
  really called rather than simulated.

Ollama runs on the CPU inside the container, which on a laptop means well under
one token per second. Keep `max_tokens` small, and leave qwen3's thinking off
(`think: false`, or `reasoning_effort: "none"` on the OpenAI-compatible
endpoint) unless you are prepared to wait minutes.

Everything in that stack is a laptop toy: the credentials are constants, the
gateway checks that a credential is *present* rather than valid, and Keycloak
runs in dev mode against an in-memory database. Do not mistake it for a
deployment.

## Write a profile from a curl you already have

One YAML file per endpoint, in the profiles directory. Take the `curl` you are
pasting around today and split it into three parts: the URL, the body, the
credential.

Say this is what you run now:

```sh
curl -X POST https://models.internal/mistral-small/v1/chat/completions \
  -H "Authorization: Bearer $MODEL_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "mistral-small", "messages": [{"role": "user", "content": "ping"}]}'
```

The profile is:

```yaml
---
name: mistral-small
kind: chat
url: https://models.internal/mistral-small/v1/chat/completions
auth: gateway-token
request:
  template: |
    {
      "model": "mistral-small",
      "messages": {{ messages | tojson }}
    }
decode:
  content:
    - $.choices[0].message.content
  finish_reason:
    - $.choices[0].finish_reason
  usage:
    - $.usage
```

The `Authorization` header became `auth: gateway-token`, a reference to an entry
in `auth.yaml` next to your profiles:

```yaml
---
providers:
  - name: gateway-token
    kind: token
    value:
      env: MODEL_TOKEN
```

**The token itself never goes in a profile.** It comes from an environment
variable, a file re-read on every call (so a rotated service account token just
works), or the UI. `auth.yaml` is safe to commit; it only says where to look.

`anonymous` always exists without being declared. That is what lets you ask "is
this route actually protected?" — and a `401` from it is a *passing* result, not
a failure.

### Testing with a workload identity

The third mode is the one that reproduces what a pod actually does. `mire`
performs the `client_credentials` exchange itself, caches the access token and
renews it 60 seconds before expiry:

```yaml
---
providers:
  - name: oidc-workload
    kind: oidc
    issuer: https://idp.internal/realms/models
    client_id: mire
    client_assertion:
      file: /var/run/secrets/kubernetes.io/serviceaccount/token
    audience: https://models.internal
```

`client_assertion` presents a **projected service account token** as an RFC 7523
assertion. It is re-read from disk on every exchange, so a rotated token is a
non-event rather than a mysterious `401` an hour after deploying. Use
`client_secret` (from `env` or `file`) instead for a plain confidential client.

Discovery reads `{issuer}/.well-known/openid-configuration`; set
`token_endpoint` explicitly to skip it when an IdP's well-known document is wrong
or unreachable. The exchange goes through the same HTTP client as everything
else, so `--ca-bundle` applies to your IdP too.

### Testing as yourself

A workload identity answers "what does the pod get?". Sometimes the question is
"what do *I* get?" — a gateway that accepts service accounts and user tokens
under different rules gives different answers, and only one of them is the one
your users will hit.

`kind: oidc_browser` runs the authorization code flow with PKCE: click **Sign
in**, a tab opens at your identity provider, and the token that comes back is
yours.

```yaml
---
providers:
  - name: me
    kind: oidc_browser
    issuer: https://idp.internal/realms/models
    client_id: mire-ui
    scope:
      - openid
      - profile
```

No `client_secret` — `mire` runs from a directory of YAML files and has no secret
to keep, which is exactly the case PKCE exists for. One is still accepted, for an
IdP configured with a confidential client.

**The callback follows the browser, not the socket.** This is the part that
matters in a Kubeflow notebook: the process binds `127.0.0.1:8787`, while your
browser is at `https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`.
Nothing inside the process can derive the second from the first, so the UI
computes the callback from `document.baseURI` and sends it with the login
request. Register `<that origin><base-path>/auth/callback` with your identity
provider and the flow works unchanged behind the proxy.

Set `--public-url` (or `PUBLIC_URL`) when a proxy rewrites paths and the
browser's own answer is wrong; it overrides everything else. Note that a redirect
URI the identity provider has not registered is refused *by the identity
provider* — that check is the one that counts, and it is not ours to weaken.

Tokens stay on the server: the UI is told a username, the granted scopes and a
countdown, never a token. When the access token expires, the refresh token is
used silently; when there is no refresh token, or the IdP has dropped the
session, the session is cleared and the button comes back rather than every call
failing the same way forever.

Signing out drops what `mire` holds. It does **not** sign you out of the identity
provider — the next login may complete without a prompt, which is worth knowing
when you are trying to come back as somebody else.

On a `401`, a token that had been *reused* is dropped and the request is replayed
exactly once with a fresh one. A token minted for that very call is left alone —
the `401` is then about something else (a missing scope, an audience mismatch),
and replaying would only hide it.

Since the same model can be pointed at all three modes without touching its
profile, the matrix is two `POST /api/call` bodies apart:

```sh
for auth in anonymous static-token keycloak-workload; do
  curl -s localhost:8787/api/call -H 'content-type: application/json' \
    -d "{\"profile\": \"qwen3-guarded\", \"auth\": \"$auth\", \"prompt\": \"ping\"}" |
    jq -r '"\(.auth): \(.response.http.status)"'
done
```

Editing anything in that directory — a profile *or* `auth.yaml` — reloads it: the
watcher picks the change up without a restart. Both swap together, so a call
never sees a new profile against an old auth registry.

A broken file never stops `mire` from starting, and never takes the good ones
down with it. One malformed profile, or one bad entry in `auth.yaml`, is skipped
and reported: `GET /api/profiles` and `GET /api/auth` each return an `issues`
list with the file, the message and the position. You reach for this tool when
something is already wrong — it should come up and show you what, not refuse to
run until its own config is perfect.

The token values themselves are read on **every** call, not cached: a rotated
service account token file is picked up on the next request.

### Check what you are about to send

`dryRun` renders the request and hands it back with its `curl` equivalent,
without sending anything. This is the thing you paste into a ticket:

```sh
curl -s localhost:8787/api/call \
  -H 'content-type: application/json' \
  -d '{"profile": "qwen3-chat", "prompt": "ping", "dryRun": true}' | jq -r .curl
```

Credentials are masked in the `curl` export, in the request view, and in every
trace and log line.

## Teach it a non-standard endpoint

Not every endpoint answers like OpenAI. Each `decode:` field is a **cascade**:
paths are tried in order and the first one that resolves wins, so one profile can
cover several shapes — including an endpoint that changes between versions.

```yaml
decode:
  content:
    - $.choices[0].message.content   # OpenAI
    - $.content[*].text              # content blocks, concatenated
    - $.output.text                  # something else entirely
  finish_reason:
    - $.choices[0].finish_reason
    - $.stop_reason
```

Decoding never fails the call. If nothing matches you still get the raw JSON, the
status, the latency — plus a trace saying exactly which paths were tried and what
went wrong:

```json
{
  "decode": {
    "matched": {"finishReason": "$.stop_reason"},
    "missed": {"content": ["$.choices[0].message.content", "$.output.text"]},
    "issues": []
  }
}
```

That is the fast way to fix a profile: look at the raw tree, pick the right path,
edit the file. See [`profiles/qwen3-native.yaml`](profiles/qwen3-native.yaml) for
a worked example — one `decode:` block covering two unrelated response shapes.

A profile with no `decode:` block at all is valid — that is the normal state of
an endpoint you have not figured out yet.

### When a cascade is not enough

Some things a path cannot do: strip a `<think>` block out of the content, join
segments conditionally, compute anything. For those, and only for those, a
profile can carry a Rhai script instead — on the request side, the response side,
or both:

```yaml
request:
  script: |
    #{ model: "exotic-1", turns: messages.map(|m| `${m.role}: ${m.content}`) }
decode:
  script: |
    let content = raw.message.content;
    let close = content.index_of("</think>");
    if close >= 0 {
      content = content.sub_string(close + 8);
      content.trim();      # trim() mutates in place, so it cannot be chained
    }
    #{ content: content, finish_reason: raw.done_reason }
```

A request script sees `messages`, `input`, `tools`, `model` and `params`, and
returns the body — a string used verbatim, or a map or array that gets serialised
for you. A decode script sees `raw`, `status` and `headers`, and returns a map:
`content` / `tool_calls` / `finish_reason` / `usage` for a chat profile,
`vectors` / `usage` for an embedding one.

**Reach for a cascade first, every time.** A script is code in a config file: it
is harder to read, harder to review, and it survives worse. It earns its place
when the alternative is not supporting the endpoint at all. `template` and
`script`, and `decode` paths and `decode.script`, are mutually exclusive — a
profile declaring both fails to load, so there is no precedence rule to remember.

Scripts are compiled when the profile loads, so a syntax error names the file at
startup. At call time they are bounded: 500k operations, a one-second deadline,
caps on string, array and map sizes, and `eval` disabled. Rhai has no file,
network or process access to begin with — there is nothing to take away, and a
test asserts it stays that way. A decode script that fails is *not* fatal: its
message lands in the decode trace next to the raw response, exactly like a path
that missed.

See [`profiles/qwen3-scripted.yaml`](profiles/qwen3-scripted.yaml) for a worked
example against the local stack.

## Really calling tools

Simulated tools prove the model *emits* well-formed calls and knows what to do
with a result. They are deterministic, depend on nothing, and execute nothing —
which is most of what you want, most of the time.

The other half of "does tool calling work" needs a real server. Declare one in
`mcp.yaml`, next to your profiles:

```yaml
---
servers:
  - name: files
    url: https://mcp.internal/mcp
    auth: keycloak-workload
```

and opt a profile into it:

```yaml
mcp:
  - files
```

At the start of a run, `mire` asks the server what it offers, declares those
tools to the model, and when the model calls one, **calls it for real**. The
trace says which: every tool card carries `simulated` or `mcp`, the server name
and the round trip.

Everything goes through the same HTTP client as your model endpoints, so
`--ca-bundle` applies, and so does the whole auth registry:
`GET /api/mcp/{name}/tools` against `anonymous`, a token and a workload identity
in turn answers "is this MCP endpoint up, and does my credential get me in?"
without running a model at all.

Three behaviours worth knowing, because each is a decision rather than an
accident:

- **A tool that fails is a result, not an error.** `isError: true` is fed back to
  the model, the loop continues, and the trace says the tool reported a problem.
  Reacting to that is exactly what is being tested. `mire` only reports an error
  of its own when it could not get an answer at all.
- **A simulated tool shadows a live one of the same name.** That is how you stub
  exactly one tool of an otherwise real server.
- **A server that asks for interactive input stops the tool.** `resultType:
  "input_required"` means it wants an elicitation, and a harness has nobody to
  ask; you get a message naming what it wanted rather than an empty result.

### Which revision you are actually speaking

`mire` speaks three revisions of the Streamable HTTP transport, and settles on
one per server, once, on first use:

| Revision | Shape |
| --- | --- |
| `2026-07-28` | No handshake, no session. Selected body fields mirrored into `Mcp-Method` / `Mcp-Name` / `Mcp-Param-*` |
| `2025-06-18` | `initialize` handshake, `Mcp-Session-Id` on every later request |
| `2025-03-26` | The same, minus the `MCP-Protocol-Version` header it predates |

It settles them by asking, newest first. `server/discover` answers with every
version a server speaks — but it is itself a method of `2026-07-28`, so it cannot
be the only question. When it comes back empty handed, `initialize` is the older
revisions' own negotiation: `mire` proposes, the server replies with what it will
actually use, and an older answer is the mechanism working rather than a
downgrade to be suspicious of. If neither answers, the newest revision is assumed
and the request goes out as it always did — `server/discover` is a method a
perfectly good server may not implement, and failing there would break endpoints
that work in order to report a problem they do not have.

**It tells you which, every time.** `GET /api/mcp/{name}/tools` carries the
answer next to the tools it produced:

```json
{
  "server": "files",
  "protocol": { "revision": "2025-06-18", "settled": "handshake" },
  "tools": []
}
```

`settled` is `discovered`, `handshake`, `pinned` or `assumed`. That last one
matters: a run that worked because a guess happened to be right is a different
fact from one that worked because both ends agreed, and only one of them stays
true next week. A tool whose job is to tell you what your endpoint does may not
quietly settle for something and call it success.

Pin it when the version is the thing under test:

```yaml
  - name: files-on-the-old-one
    url: https://mcp.internal/mcp
    protocol_version: 2025-06-18
```

A pin skips both probes. Pinning a revision the server refuses gets you the
refusal, which is the point — declare the same URL twice under different pins and
you can say exactly which revisions your endpoint accepts. An unknown one is a
load issue naming what this build speaks, reported at startup like every other
bad entry, without taking the rest of the file down.

A session that the server has forgotten — a restart, an expiry, a different
replica — comes back as a `404` to a request that carried one. `mire` handshakes
again and replays the call once, so you see the listing rather than the
plumbing. Twice in a row is reported, because at that point it is not plumbing.

### A token that does not fit

`auth:` covers a credential in the ordinary place. For the rest — an API key, a
tenant header, a scheme nobody else uses — `headers:` takes MiniJinja templates:

```yaml
  - name: files
    url: https://mcp.internal/mcp
    headers:
      x-api-key: "{{ env.FILES_API_KEY }}"
      x-tenant: "{{ env.TENANT | default('dev') }}"
```

They are rendered **on every request**, with `env` read fresh each time, so a
rotated token is picked up without restarting — the same property that makes
`value.file` work for a projected service account token. Templates are compiled
when `mcp.yaml` loads, so a syntax error names the server at startup rather than
on the first agent run.

An undefined variable is an **error**, not an empty string. `Authorization:
Bearer ` is a header that looks present, passes every local check and fails at
the far end with something unhelpful; you get the variable's name instead. Write
`| default(...)` where a header really is optional.

Rendered values are masked everywhere a credential is masked, and only the header
*names* appear in `GET /api/mcp`. Use `auth:` when it fits — it is one word, and
it comes with the `401`-refresh-and-replay behaviour that a hand-written header
cannot have.

#### The credential can come from the auth registry

`env` is not the only source. `auth` is the registry itself, keyed by provider
name, each entry the **bare token** that provider would produce:

```yaml
  - name: files
    url: https://mcp.internal/mcp
    headers:
      x-api-key: '{{ auth["keycloak-workload"] }}'
```

So the two mechanisms compose instead of competing. `auth:` decides *where* a
credential goes and the provider owns that decision; this decides where it goes
when the provider's answer is wrong for one particular server. Everything the
registry can already do — a rotated token file, a `client_credentials` exchange
with its cache, the browser session you are signed into — works here unchanged,
including inside a larger value:

```yaml
      authorization: 'Custom tenant=acme token={{ auth["me"] }}'
```

Write the bracket form. `auth.keycloak-workload` parses as a subtraction, which
is a confusing way to find out that registry names have hyphens in them.

Only the providers a server actually names get resolved. Asking for a credential
costs a token exchange and can fail — a `client_credentials` round trip, a
refresh, a session nobody has signed into — and none of that belongs to a server
that never mentioned it. A provider that produces nothing (`anonymous`, and only
`anonymous`) is reported by name rather than sent as an empty header, and one
that needs a login comes back as `409 not_signed_in` **before** anything is sent
to the MCP server.

`GET /api/mcp` lists the providers each server reads in `usesAuth`, because a
server authenticating purely through a template shows no `auth:` at all and would
otherwise look anonymous.

**Tools are not filtered by default.** Whatever a server advertises, the model
may call — that is the honest default for a server you deliberately pointed at.
`tools:` narrows it when you are pointing at something that can delete things and
you only meant to read:

```yaml
  - name: files
    url: https://mcp.internal/mcp
    tools:
      - read_file
      - list_directory
```

Annotations (`readOnlyHint`, `destructiveHint`) are reported, never enforced:
they are a server's claim about itself, and this tool is in the business of
checking claims rather than trusting them.

## Streaming, and the number everybody actually wants

A non-streamed call answers one question about latency: how long the whole thing
took. That number is dominated by how much the model chose to say. The one worth
having is **time to first token** — how long before it started answering — and
you cannot measure it without reading the response in pieces.

Two lines make a profile streamable:

```yaml
request:
  template: |
    { "model": "…", "messages": {{ messages | tojson }},
      "stream": {{ stream | tojson }} }
decode:
  delta:
    - $.choices[0].delta.content   # OpenAI-shaped chunks
    - $.message.content            # Ollama's native NDJSON
```

`stream` comes from the call, not from the file, so one profile serves both
modes: **Send** asks for a whole answer, **Stream** asks for chunks. Keep the
`| tojson`. MiniJinja renders a bare boolean as `True`, which is Python and is
not JSON — rendering catches it and shows you the body, but it is a nicer trap to
avoid than to diagnose.

`decode.delta` is a separate cascade from `decode.content` because a chunk is not
a small response: OpenAI moves the text from `message` to `delta`, Ollama keeps
`message` and sends one object per line. Both spellings fit in one block, and the
trace says which one matched.

Nothing here makes an endpoint stream. The endpoint has to be asked, in its own
request body — which is why the flag goes through the template rather than being
something `mire` does on your behalf.

```console
$ curl -N -X POST localhost:8787/api/call/stream -d '{"profile":"qwen3-chat","prompt":"…"}'
event: open
data: {"event":"open","status":200,…}

event: delta
data: {"event":"delta","text":"Three"}
…
event: done
data: {"event":"done",…}
```

The `done` event carries exactly what `POST /api/call` returns, so a client can
ignore every delta and still get the whole answer. Real numbers from the local
qwen3, on the stack below:

```
ttftMs 8426    latencyMs 31271    sse   11 chunks, 10 with text, ended cleanly
```

Eight seconds to the first token, thirty-one to the last. One number is about the
endpoint, the other is mostly about how chatty the model felt.

### What the framing tells you

The SSE-versus-NDJSON question is **detected, not declared** — the `content-type`
already answers it, and a config knob for something the endpoint states would be
a knob that can disagree with reality.

`response.stream` reports what the transport did, which is the half a normal call
cannot see:

| Field | Why you care |
| --- | --- |
| `chunks` / `deltas` | One chunk is not streaming. A preamble chunk carrying no text is why these differ |
| `terminated` | `false` means no sentinel and no stop reason: the connection went quiet rather than finishing — what a proxy cutting a long generation looks like |
| `unparsable` | Frames that were not JSON. An HTML error page mid-stream lands here |
| `bytes`, `firstChunkMs` | First byte versus first token, which is how you tell a slow endpoint from a chatty preamble |

A stream that dies halfway is **not** an error. Whatever arrived is decoded,
shown, and marked as unterminated — the partial answer is the evidence.

Tool calls are not reassembled from a stream: OpenAI splits a call's arguments
across chunks, and stitching those back together is guesswork this tool would
rather not do on your behalf. Agent mode calls whole, which is where tool calling
is tested anyway.

## Embeddings

An embedding profile takes `input` — a string or a list of strings — instead of
`messages`, and the answer is judged on its *shape*:

```sh
curl -s localhost:8787/api/call \
  -H 'content-type: application/json' \
  -d '{"profile": "nomic-embed", "input": ["one", "two"], "repeat": 2}' | jq .response.decoded
```

```json
{
  "kind": "embedding",
  "count": 2,
  "dimensions": {"kind": "uniform", "value": 768},
  "encoding": "float",
  "vectors": [{"index": 0, "dimensions": 768, "norm": 1.0, "sample": [...], "finite": true, "histogram": {...}}],
  "checks": {
    "count": {"status": "pass"},
    "dimensions": {"status": "pass"},
    "finite": {"status": "pass"},
    "nonZeroNorm": {"status": "pass"},
    "determinism": {"status": "pass"}
  }
}
```

`count` and `dimensions` are **derived from the vectors that came back**, never
read out of the response — an endpoint that claims 768 and returns 384 is
exactly what this catches. Inconsistent widths surface as
`{"kind": "ragged", "values": [...]}` rather than being averaged away.

`repeat: 2` sends the request twice and compares: the same input must give the
same vectors, within `tolerance` (default `1e-6`). This is the check that catches
a replica quietly serving a different model from its siblings — everything else
about its answer looks perfectly fine. Without `repeat`, the check reports
`skipped` and says so, rather than passing by default.

Three response shapes decode without a script: one node per vector
(`$.data[*].embedding`), one node holding a list of vectors (`$.embeddings`), and
a bare vector at the root. Base64 payloads — what `encoding_format: base64`
produces — are decoded as little-endian `f32` and counted like any other.

### Vectors are never rendered whole

Not in the logs, not in the API, not in the UI. You get the width, the L2 norm, a
sample of the first values and a distribution histogram. The raw response is
elided too (`"<1024 values elided; set includeVectors to see them>"`), because a
careful summary next to a `raw` field carrying all 1024 floats would be theatre —
the rest of the raw tree, which is what you actually read, is untouched.

`includeVectors: true` turns all of that off and gives you the full payload. It
is the only way to get it.

## Agent mode

Agent mode is not a third payload format and not a second profile. It is the same
`kind: chat` profile, run in a loop: render, call, decode; if the stop condition
is not met, answer the tool calls with their simulated results, feed them back,
go round again. `POST /api/call` runs one turn of exactly the same thing.

```yaml
agent:
  stop_when:
    no_tool_calls: true          # the default, and almost always what you want
    finish_reason_in: [stop, end_turn]
  max_iterations: 6
  max_duration_ms: 600000
tools:
  - name: get_weather
    description: Look up the weather in a city.
    schema:
      type: object
      properties:
        city:
          type: string
      required: [city]
    response: '{"temp": 21, "conditions": "clear"}'
```

**Nothing is executed.** The tools are simulated — a fixed string, or a Rhai
script that sees `arguments`, `name` and `turn`. What is being checked is that
the model emits calls matching the schema it was given, and knows what to do with
a result. Arguments are validated against that schema and the mismatches are
reported; the model still gets an answer, so it has a chance to correct itself. A
tool the profile never declared gets an error back rather than silence.

`POST /api/agent` streams server-sent events: a `turn` event per turn as it
happens, then one `done` carrying the whole trace. Each turn holds the rendered
request, the masked headers, the `curl` equivalent, the raw response, the decode
trace and the tool results — the same shape `POST /api/call` returns.

### Every way out is named

There is no silent loop. The one worth spelling out:

```json
{"outcome": "predicateNeverEvaluable", "predicate": "stop_when.finish_reason_in", "turns": 3}
```

A profile that stops only on `finish_reason`, pointed at an endpoint that never
reports one, would otherwise run to `max_iterations` and look like a slow agent.
It is not — the condition could never be evaluated once, and that is what gets
reported. The others are `stopped` (a predicate held), `maxIterations`,
`deadline`, and `repeatedCall` — the model asking for the same tool with the same
arguments twice, which is a loop rather than progress.

Try it against the local stack: `qwen3-native` declares a `get_weather` tool and
qwen3 does call it. On a CPU-only Ollama a two-turn run takes about ninety
seconds.

## API

`/docs` serves a Scalar reference against `/openapi.json`.

| Route | What it does |
| --- | --- |
| `GET /api/profiles` | Every profile, plus the files that failed to load and why |
| `GET /api/profiles/{name}` | One profile, as declared |
| `GET /api/auth` | Auth providers, for the selector, with session status |
| `GET /api/mcp` | MCP servers declared in `mcp.yaml` |
| `GET /api/mcp/{name}/tools` | Ask a server what it offers, right now, and on which revision |
| `POST /api/auth/{name}/login` | Start a browser login; returns where to send it |
| `POST /api/auth/{name}/logout` | Forget the session `mire` holds |
| `POST /api/call` | Render, authenticate, send, decode |
| `POST /api/call/stream` | The same, read chunk by chunk, with time to first token |
| `POST /api/agent` | The same, in a loop, streamed as server-sent events |
| `GET /auth/callback` | Where the identity provider sends the browser back |
| `GET /healthz` | Liveness |

`/auth/callback` and `/healthz` are the two routes outside the OpenAPI document:
one is a page for a human, the other is ops plumbing. Neither is API surface.

A `4xx` or `5xx` **from the endpoint under test** is a successful call: read
`response.http.status`. The API only returns an error when `mire` itself could
not do its job.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings

npm --prefix ui test
npm --prefix ui run typecheck   # a blocking gate, same as the linter
npm --prefix ui run check       # Biome: format and lint

pre-commit install
```

For UI work, `npm --prefix ui run dev` serves the front end with hot reload and
proxies `/api` to a `mire` already running on its default port. In a debug build
the assets are read from `ui/dist` at runtime, so `npm run build` alone is enough
to see a change in the real binary.

Outbound HTTP is covered by `wiremock` on every failure path that matters: an
expected `401`, a timeout, a malformed body, an empty body, a decode path that
misses, and a cross-host redirect (where the `Authorization` header must not
follow). OIDC runs against a mock identity provider: discovery, caching, expiry,
a rejected cached token refreshed and replayed exactly once, a rotated service
account token, and a failed exchange. Several tests fail if a credential appears
anywhere in an API response — including the case where the endpoint or the IdP
quotes it back at us in an error message.

### CI and releases

`pre-commit` **is** the CI gate. The `quality` workflow installs the tools the
hooks expect and runs `pre-commit run --all-files` — nothing is checked in CI
that a local run does not check, and nothing is checked twice. A second job runs
Trivy over the repository. Whatever is deliberately waived lives in
`.trivyignore.yaml`, in one place, with the reasoning attached.

Tagging `vX.Y.Z` releases. The tag must match the `version` in `Cargo.toml` — the
release fails on the mismatch rather than publishing a binary that lies about
which release it is. What comes out:

* `ghcr.io/therealm-tech/mire:X.Y.Z` and `:latest`, a multi-arch manifest over
  `linux/amd64` and `linux/arm64`, each built on its own native runner. No QEMU.
* `mire-X.Y.Z-linux-x86_64.tar.gz` and `mire-X.Y.Z-linux-aarch64.tar.gz` on the
  GitHub Release — the binary and `LICENSE`, nothing else — each with its
  `.sha256` published next to it rather than packed inside.

The binaries are **taken out of the images**, not built a second time from the
same source: what you download is what was scanned and published. They are
statically linked against musl, so there is no glibc version to match against
whatever the notebook happens to be running.
