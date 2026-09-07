# Configuration keys

Every key of every file `mire` reads, and what it does. The task-shaped versions
of the same ground are [Configuration](/docs/guides/configuration),
[Models](/docs/guides/models), [Credentials](/docs/guides/auth) and
[MCP servers](/docs/guides/mcp); this page is the exhaustive list.

The options file — `mire.yaml`, which configures the tool rather than what it
tests — is in [Command line and options](/docs/reference/cli#the-options-file).

## How a directory is read

A configuration directory holds one subdirectory per kind of thing, and one file
per entry:

| Subdirectory | One file declares |
| --- | --- |
| `models/` | [a model endpoint](#models--a-model-endpoint) |
| `auth/` | [a credential provider](#auth--a-credential-provider) |
| `mcp/` | [an MCP server](#mcp--an-mcp-server) |
| `prompts/` | [a saved prompt](#prompts--a-saved-prompt) |
| `decodes/` | [a named decode](#decodes--a-named-decode) |

Rules that hold for all five:

- **The entry's fields are at the top level of the document.** One document, one
  entry.
- **`name:` is the identity, not the file name.** Renaming a file must not
  silently rename the thing every other file references.
- **`.yaml` and `.yml` only**, and **not recursive**: `models/old/qwen3.yaml` is
  somebody's own arrangement, not a live model.
- **A subdirectory that is not there declares nothing.** That is what a directory
  layered on top of another one to add two prompts looks like.
- **An empty document is skipped**, which is what makes a fully commented-out
  example file a usable way to ship one.
- **Unknown keys are refused**, everywhere on this page. A `templte:` that parsed
  cleanly and did nothing is the worst answer available.
- **Everything compiles at load time** — templates, Rhai scripts, JSONPaths and
  the regexes behind `tools:` — so a typo names the file at startup rather than
  surfacing twenty minutes into a run.
- **A file that does not load is skipped and reported**, on `GET /api/config` and
  in the UI, rather than stopping the process.

Files are read in the directory listing's order, sorted by name; directories are
layered in the order given to `--config-dir`, and the last one to declare a name
owns it.

## Stages: keys every entry may carry

Any entry of any kind can declare several environments in one file. Two keys do
it, and they are the only keys shared across all five kinds.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `stages` | map of stage name → map of variables | *(none)* | One reading of the file per stage. The variables are read back with `${ stage.… }` |
| `default_stage` | string | the first stage declared | Which stage a bare `name` reaches |

```yaml
name: models
kind: chat
url: ${ stage.base }/v1/chat/completions
default_stage: dev
stages:
  dev:
    base: http://127.0.0.1:11435
  prod:
    base: https://models.internal
```

- **`${ … }` resolves when the file loads** and sees exactly one thing: `stage`,
  that stage's variables. `{{ … }}` resolves when a call goes out and sees the
  [template context](#template-context). The delimiters differ because both live
  in the same `request.template:`.
- **A file that declares no `stages:` is not substituted at all**, so a `${` in a
  saved prompt stays the text somebody wrote. In a file that does declare them,
  `$${` is a literal `${`.
- **An undeclared stage variable is an error**, not an empty string.
  `| default(…)` covers the ones that really are optional.
- **All or nothing, per file.** A stage that does not expand takes the whole
  entry down, so a stage missing from the picker is never explained only by a
  line in the log.
- **The entry is then addressed as `name@stage`**, so `@` is not allowed in a
  `name:`. Stages are independent across entries and across kinds: a `prod`
  model, a `preprod` credential and a `dev` server is an ordinary run.

## `models/` — a model endpoint

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | Identifier used in the API and the UI. Unique across the directories, no `@` |
| `kind` | `chat` \| `embedding` | *required* | What the endpoint does. Decides the template variables, the decoded shape and the checks that apply |
| `url` | URL | *required* | Full endpoint URL. Pointing anywhere is the feature |
| `method` | `POST` \| `GET` \| `PUT` \| `PATCH` | `POST` | HTTP method |
| `auth` | string | *(none)* | Name — or `name@stage` — of an entry in `auth/`. Absent means anonymous |
| `headers` | map of string → string | `{}` | Extra headers sent verbatim. Never put a credential here |
| `timeout_ms` | integer | `30000` | Request timeout |
| `has_prompt` | boolean | `true` | Whether a call carries something somebody typed. `false` hides the message box and sends an empty conversation — for an endpoint whose input is not text |
| `requires_upload` | boolean | `false` | Refuse a call that attaches no file, before anything leaves the process |
| `request` | map | *required* | [How the body is built](#modelsrequest--the-body) |
| `decode` | map | `{}` | [How the response is read](#modelsdecode--the-answer) |
| `agent` | map | *(none)* | [Agent-loop configuration](#modelsagent--the-loop). `kind: chat` only |
| `tools` | list of map | `[]` | [Simulated tools](#modelstools--simulated-tools). Nothing is executed |

`has_prompt` says nothing about the wire: what goes out is still whatever the
template asks for, and a template reading `messages` on a `has_prompt: false`
model renders against an empty list.

`requires_upload` says the call must carry *a* file, not which one. What the
template does with `uploads` is still the model's business.

### `models/…/request` — the body

Exactly one of the three. A template is the one to reach for; a script is code in
a config file and earns its place only when a template cannot express the shape.

| Key | Type | What it does |
| --- | --- | --- |
| `template` | string | MiniJinja template. Must render to valid JSON |
| `script` | string (Rhai) | Script seeing the same variables, returning the body — a string, or a map or array serialised to JSON |
| `multipart` | map of field → part | A `multipart/form-data` body, one entry per form field |

Rendering validates that the result is JSON, so a stray comma — the classic
`{% if tools %}"tools": …,{% endif %}` — fails here with the rendered text
attached, rather than as a bewildering `400` from the endpoint.

**`multipart` parts** go out in the order the file wrote them. A part is a text
field or a file field, and the two are told apart by which key was used, never by
guessing at the rendered value.

| Written as | Is |
| --- | --- |
| `field: <scalar>` | A text part. Any YAML scalar — `temperature: 0` and `translate: false` are how anybody writes a knob, and every part goes out as text regardless |
| `field: {text: <template>}` | The same, with room for `type:` |
| `field: {upload: <template>}` | A file part |
| `field: {upload: [<template>, …]}` | Several files under one field name, which is what every upload handler already reads |

| Part key | Type | Applies to | What it does |
| --- | --- | --- | --- |
| `text` | string | text parts | The template |
| `upload` | string or list of string | file parts | Templates, each naming one or more uploads of the call |
| `type` | string | both | `content-type` of the part. On a file part it overrides the guess made from the extension |
| `filename` | string | file parts | Overrides the stored name. Only for a field carrying exactly one file |

```yaml
request:
  multipart:
    file:
      upload: '{{ uploads[0] }}'
    model: whisper-1
    response_format: json
```

### `models/…/decode` — the answer

Every field is a **cascade**: paths are tried in order and the first that
resolves wins. A field whose paths all miss is absent from the decoded output —
the raw response and the decode trace are what you read then. Decoding never
fails a call.

| Key | Type | `kind` | What it reads |
| --- | --- | --- | --- |
| `from` | list of string | both | Named decodes this one is built from, tried in the order given |
| `script` | string (Rhai) | both | Replaces the cascades entirely. Exclusive with every path below |
| `content` | list of JSONPath | `chat` | Assistant text |
| `delta` | list of JSONPath | `chat` | Assistant text inside **one chunk** of a streamed response |
| `tool_calls` | list of JSONPath | `chat` | Tool calls emitted by the model |
| `finish_reason` | list of JSONPath | `chat` | Why generation stopped |
| `terminal_reasons` | list of string | `chat` | The `finish_reason` values that mean *done*, for the agent loop |
| `usage` | list of JSONPath | both | Token accounting |
| `error` | list of JSONPath | both | What the endpoint says went wrong |
| `vectors` | list of JSONPath | `embedding` | The vectors themselves |

- **`delta` is not optional for a streaming model.** The chunk shape is not the
  whole response's — OpenAI moves the text from `message.content` to
  `delta.content` — so without it a model streams and decodes nothing. A
  `decode.script` is not run per chunk, so a scripted model streams without text
  deltas.
- **`terminal_reasons` is a vocabulary, not a cascade**, which is why two named
  decodes pool their lists instead of racing them. It sits here rather than under
  `agent:` because which values are terminal is a fact about the endpoint, not
  about the run. Empty means the loop does not read `finish_reason` to decide.
- **`error` is read regardless of status.** An endpoint answering `200` with the
  complaint in the body is precisely the mismatch worth catching. It describes
  the *model's* answer — a gateway or an identity provider's refusal is
  deliberately not decoded, and reaches you as the status, the headers and the
  raw body.
- **`from` and a path written beside it mix.** For each field, the paths written
  here come first, then each named decode's in turn. It is resolved once, when
  the model loads; what runs is a flat cascade.

**Built-in decode names**, available to `from:` with no file:

| Name | `kind` | The endpoints that answer it |
| --- | --- | --- |
| `openai-chat` | `chat` | OpenAI, vLLM, TGI, llama.cpp, LM Studio, Ollama's `/v1`, and the gateways that copied it |
| `openai-embeddings` | `embedding` | the same crowd, on `/v1/embeddings` |
| `anthropic-chat` | `chat` | Anthropic's Messages API |
| `gemini-chat` | `chat` | Google's `generateContent`, on the Gemini API and on Vertex AI |
| `ollama-native-chat` | `chat` | Ollama's own `/api/chat` |
| `ollama-native-embeddings` | `embedding` | Ollama's own `/api/embed` |

### `models/…/agent` — the loop

Only meaningful for `kind: chat`. Absent means the defaults below.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `stop_when.no_tool_calls` | boolean | `true` | Stop as soon as a turn produces no tool calls |
| `stop_when.repeated_call` | boolean | `false` | Stop when the model asks for the same tool with the same arguments twice |
| `default_max_turns` | integer, 1–100 | `10` | Hard cap on turns, and the budget a run takes when it asks for none |
| `max_duration_ms` | integer | *(none)* | Hard cap on wall-clock time for the whole loop |

Predicates combine with OR. `repeated_call` is off unless asked for: a model that
re-reads a tool it already called is often working, not looping.

Stopping on a finish reason is not a key here — it is
[`decode.terminal_reasons`](#modelsdecode--the-answer). Setting
`no_tool_calls: false` and relying on `terminal_reasons` against an endpoint that
reports no stop reason is refused rather than looking like a slow agent.

### `models/…/tools` — simulated tools

The model may call these and gets a canned result back. **Nothing is executed.**
A simulated tool of the same name as a live one wins over the server's, which is
how you stub exactly one tool of an otherwise live server.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | Tool name, as advertised to the model |
| `description` | string | *(none)* | What the tool is for. Passed to the model, which is what makes it call the tool at the right moment |
| `schema` | JSON Schema | *required* | Arguments, used both to advertise the tool and to check what the model sends back |
| `response` | string | — | Canned result. Exclusive with `script` |
| `script` | string (Rhai) | — | Result computed from the call. Exclusive with `response` |

Exactly one of `response` and `script` is required.

## `auth/` — a credential provider

`kind:` selects the shape. The directory says *where to look*, never the value,
so it is safe to commit.

Every kind takes these:

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | How a model's `auth:` names it. No `@` |
| `kind` | `anonymous` \| `token` \| `oidc` \| `oidc_browser` | *required* | What it sends |
| `allowed_hosts` | list of string | `[]` | Hosts this credential may be sent to. Empty means anywhere. Checked against the **resolved** URL, after a templated one has been rendered |

### `kind: anonymous`

Sends nothing. No further keys — which is what makes a `401` from it a *passing*
result rather than a failure.

### `kind: token`

A static credential, read fresh on every call.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `header` | string | `authorization` | Header the credential goes in |
| `scheme` | string or `null` | `Bearer` | Prefix before the value. `scheme: null` sends the credential bare |
| `value.env` | string | *(none)* | Environment variable holding the token, read on every call |
| `value.file` | path | *(none)* | File holding the token, re-read on every call so rotation is picked up |

**Both `value` fields absent means the UI prompts for it**, and what is typed
stays in that tab.

### `kind: oidc`

`client_credentials` — the mode that reproduces what a workload actually does.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `issuer` | URL | *required* | Discovery starts here |
| `token_endpoint` | URL | from discovery | Overrides what discovery found |
| `client_id` | string | *required* | The client |
| `client_secret.env` / `client_secret.file` | string / path | *(none)* | Where the secret is read from, same shape as `value` above |
| `client_assertion.file` | path | *(none)* | A projected service account token, re-read on every exchange |
| `scope` | list of string | `[]` | Scopes requested |
| `audience` | string | *(none)* | `audience` parameter of the exchange |
| `header` | string | `authorization` | Header the access token goes in |
| `scheme` | string or `null` | `Bearer` | Prefix before it |

### `kind: oidc_browser`

Authorization code with PKCE. No client credential is required — `mire` runs from
a directory of YAML files and has no secret to keep, which is the case PKCE
exists for.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `issuer` | URL | *required* | Discovery starts here |
| `authorization_endpoint` | URL | from discovery | Where the browser is sent |
| `token_endpoint` | URL | from discovery | Where the code is exchanged |
| `client_id` | string | *required* | The client |
| `client_secret.env` / `client_secret.file` | string / path | *(none)* | For a provider that insists on one |
| `scope` | list of string | `[]` | Scopes requested |
| `audience` | string | *(none)* | `audience` parameter of the exchange |
| `header` | string | `authorization` | Header the access token goes in |
| `scheme` | string or `null` | `Bearer` | Prefix before it |

Sessions live in memory, outside the registry, so a configuration reload does not
sign anybody out. Two stages of one provider hold two sessions.

## `mcp/` — an MCP server

Declaring a server makes it *available*. A run reaches it only when it is
switched on in the UI or named in `mcpServers:` on the request.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | How a `mcpServers:` list names it. No `@` |
| `url` | URL | *required* | The Streamable HTTP endpoint |
| `auth` | string | *(none)* | Name of an entry in `auth/` |
| `tools` | list of regex | `[]` | Restricts what the model may reach. Empty offers whatever the server advertises. Each pattern must match the **whole** name |
| `headers` | map of string → template | `{}` | [Extra headers](#template-context), rendered on every request |
| `timeout_ms` | integer | `30000` | Request timeout |
| `protocol_version` | string | *(negotiated)* | Revision to speak, skipping negotiation |
| `hooks` | list of map | `[]` | [What fires around a `tools/call`](#mcphooks--around-a-tool-call) |
| `capture` | list of map | `[]` | [What to keep out of tool results](#mcpcapture--keeping-a-value) |

A server's `headers:` render on *every* request it makes, and the first of those
is the `tools/list` at setup — before anything has been captured. So a header
naming a captured variable needs `| default('')` or the run dies negotiating.

### `mcp/…/hooks` — around a tool call

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | Unique within the server. What the trace calls it |
| `on` | list of `before` \| `after` | *required* | Phases it fires on. At least one |
| `tools` | list of regex | `[]` | Tools it applies to. Empty is every tool |
| `if` | MiniJinja expression | *(none)* | What has to hold for it to fire |
| `on_error` | `fail` \| `continue` | `fail` | What its failure does to the call |
| `actions` | list of map | *required* | What it does, in order. At least one |

- **`before` failing stops the call** — a policy gate for free: the `tools/call`
  never goes out, and neither do the hook's remaining actions. `after` failing is
  a report rather than an undo.
- **`if:` is the one place undefined is false rather than an error**, because a
  condition is *asking* whether something is there. Lookups chain, so
  `{{ vars.job.id }}` on a run with no `job` is false rather than a failure.
- **Not firing is not failing.** `on_error` does not apply, nothing is sent, no
  credential is resolved — and the skip is recorded, quoting the condition, so a
  hook that never ran and a hook that was never declared do not look the same.

**Actions** are written under the key naming their kind. `http` is the only one.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `http.url` | string or template | *required* | Where it goes. Resolved **before** the credential, so `allowed_hosts` is checked against the address actually used |
| `http.method` | string | `POST` | Method |
| `http.auth` | string | *(none)* | Name of an entry in `auth/` |
| `http.headers` | map of string → template | `{}` | Extra headers |
| `http.json` | any YAML document | *(none)* | The body, sent as JSON, with every string in it a template. Exclusive with `multipart` |
| `http.multipart` | map of field → template or list | *(none)* | A form carrying the run's files. Exclusive with `json` |
| `http.timeout_ms` | integer | `10000` | Request timeout |

An action declaring neither `json:` nor `multipart:` sends **no body at all**.

In `json:`, a string that is one `{{ … }}` and nothing else keeps the type of
what it names, so `'{{ arguments }}'` is the arguments object rather than a
quoted rendering of one; text around the expression makes it a string again.
`'{{ call }}'` is the whole call in one line.

Each `multipart:` entry names **one** upload — the object itself, or a string
matching its `path`, `name` or `id` — or a list of them, which go out as several
parts under the same field name. A field naming something the run is not carrying
fails the hook.

### `mcp/…/capture` — keeping a value

One rule is one `tools:`-plus-`vars:` pair, because the tool a value comes from
is part of what the value *means*.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `tools` | list of regex | `[]` | Tools it applies to. Empty is every tool the server offers |
| `vars` | map of name → list of JSONPath | *required* | Variable name to the cascade that fills it. Tried in order, first hit wins |

```yaml
capture:
  - tools:
      - create_session
    vars:
      session:
        - $.sessionId
```

Read from `structuredContent` when the server sent one, and from the result text
parsed as JSON otherwise — a tool answering prose captures nothing. Variables are
one bag per run, shared across every server that run reaches, and thrown away
with the run.

## `prompts/` — a saved prompt

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | What it is called, unique within the directory |
| `text` | string | *required* | The text itself, dropped in the box exactly as written |

The whole of the metadata, deliberately: a prompt *is* its text. It is never sent
on its own — what a message becomes on the wire is the model's template's
decision.

Prompts appear in the order of the directory listing, which is why naming them
`01-ping.yaml`, `02-refusal.yaml` is how a library's arrangement gets written
down.

## `decodes/` — a named decode

The only kind whose entries exist without a file: the shapes every endpoint
answers are compiled into the binary, and this directory layers on top of them.
An entry taking a built-in's name displaces it for every model.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | *required* | How a model's `decode.from` asks for it |
| `kind` | `chat` \| `embedding` | *required* | Which kind of model this describes the answer of |
| `decode` | map | *required* | The cascades, in [the shape a model file writes them](#modelsdecode--the-answer) |

A decode is not portable across kinds — `content` is meaningless to an embedding
endpoint and `vectors` to a chat one — so a model reaching for the wrong one
fails to load rather than decoding nothing.

**`decode.script` is refused here.** A script replaces the cascades, and one
arriving from another file would make that rule a question about two documents at
once.

`decodes/` is the one kind whose load issues do not appear in a listing:
`GET /api/config` is where they surface.

## Template context

`{{ … }}` is MiniJinja, rendered when a call goes out. **An undefined variable is
an error**, not an empty string: `Authorization: Bearer ` is a header that looks
present, passes every local check, and fails at the far end with something
unhelpful. `| default(…)` covers the ones that really are optional. The single
exception is a hook's `if:`.

### A model's `request.template` and multipart parts

| Variable | Type | What it holds |
| --- | --- | --- |
| `messages` | list of message | Conversation so far. `kind: chat` |
| `input` | list of string | Text to embed. `kind: embedding`. A single string still arrives as a list |
| `tools` | list | Simulated tools, in OpenAI function shape so `{{ tools \| tojson }}` works as-is |
| `model_id` | string or null | Model identifier, when the caller overrides the one baked into the template |
| `params` | map | Free-form knobs — `params.max_tokens`, `params.temperature` |
| `uploads` | list of upload | Files the caller attached, in the order they were asked for |
| `stream` | boolean | Whether this call was asked to stream |

**`"stream": {{ stream | tojson }}` is what makes the composer's stream box do
anything** — nothing client-side makes a response arrive in chunks, the endpoint
has to be told. The `| tojson` is not decoration: MiniJinja renders a bare
boolean as `True`, which is Python and is not JSON.

An **upload** carries the file whole, already read:

| Field | Type | What it holds |
| --- | --- | --- |
| `id` | string | The handle the caller asked for |
| `name` | string | File name, sanitised, without the stored prefix |
| `stored_as` | string | The full name on disk |
| `path` | string | Where it is, for a template that only wants to say so |
| `size` | integer | Size in bytes |
| `content_type` | string or null | Guessed from the extension. `null` is a template's cue to decide for itself |
| `base64` | string | The whole file, standard base64 |
| `data_url` | string | The same bytes as `data:<type>;base64,…`, which is what most vision endpoints read |
| `text` | string or null | The file decoded as UTF-8 when it decodes, so `{% if upload.text %}` is the test for "is this readable text" |

### An MCP server's `headers`

| Variable | What it holds |
| --- | --- |
| `env` | The process environment, read fresh on every request |
| `auth` | The auth registry, keyed by provider name, each entry the bare token that provider would produce |
| `vars` | What the run's tool calls have captured |

`{{ auth["keycloak-workload"] }}` lets a credential `mire` already knows how to
obtain go anywhere in the request, not only where the provider itself would put
it. Reach for `auth:` first when the server takes an ordinary bearer token: it
brings the `401`-refresh-and-replay behaviour a hand-written header cannot have.

### A hook's `url`, `json`, `headers`, `multipart` and `if`

One context, so there is no second table to remember.

| Variable | What it holds |
| --- | --- |
| `phase` | `before` or `after` |
| `server` | The server the call is on |
| `tool` | The tool being called |
| `arguments` | The arguments the model produced |
| `result` | What the tool answered. `after` only |
| `call` | The whole call in one value |
| `vars` | What earlier tool calls captured |
| `env` | The process environment |
| `uploads` | The run's files, for `multipart:` |

**`auth` is deliberately not in scope in `json:`.** A credential belongs in a
header, where the redactor is; a body that can reach the auth registry is a
credential one typo away from a webhook's access log. A hook's `headers:` do see
`auth`.

## Script context

Rhai, compiled when the file loads. A script is deliberately awkward to prefer:
it is code in a config file, harder to read and harder to review than a path.

| Where | Sees | Returns |
| --- | --- | --- |
| `request.script` | The same variables as `request.template` | The body — a string, or a map or array serialised to JSON |
| `decode.script` | `raw`, `status`, `headers` | A map: `content` / `tool_calls` / `finish_reason` / `usage` for a chat model, `vectors` / `usage` for an embedding one |
| A simulated tool's `script` | `arguments`, `name`, `turn` | The tool's result |

Every script runs in a sandbox with **no file, network or process access**,
`eval` disabled, and bounded on five axes. A test asserts it stays that way.

| Bound | Value |
| --- | --- |
| Operations | 500 000 |
| Wall clock | 1 s |
| String size | 1 000 000 |
| Array size | 100 000 |
| Map size | 10 000 |
| Call depth | 32 |
