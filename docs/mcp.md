# Really calling tools

Simulated tools prove the model *emits* well-formed calls and knows what to do
with a result. They are deterministic, depend on nothing, and execute nothing —
which is most of what you want, most of the time.

The other half of "does tool calling work" needs a real server. Declare one in
`mcp/`, next to your models:

```yaml
---
name: files
url: https://mcp.internal/mcp
auth: keycloak-workload
```

That declares it, and declaring is all it does. Every server in that directory is
reachable by every `kind: chat` model — there is no second list to keep in step —
but **nothing is live until a run says so**: a tool call here really runs, on
somebody's real server, so which servers a run reaches is the switch on its card
in the **MCP servers** block and `mcpServers:` on the request
[decide](#choosing-what-a-run-reaches), and they start at none.

At the start of a run, `mire` asks each server what it offers, declares those
tools to the model, and when the model calls one, **calls it for real**. The
trace says which: every tool card carries `simulated` or `mcp`, the server name
and the round trip.

Everything goes through the same HTTP client as your model endpoints, so
`--ca-bundle` applies, and so does the whole auth registry:
`GET /api/mcp/{id}/tools` against `anonymous`, a token and a workload identity
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

## Which revision you are actually speaking

`mire` speaks four revisions of the Streamable HTTP transport, and settles on one
per server, once, on first use:

| Revision | Shape |
| --- | --- |
| `2026-07-28` | No handshake, no session. Selected body fields mirrored into `Mcp-Method` / `Mcp-Name` / `Mcp-Param-*` |
| `2025-11-25` | `initialize` handshake, `Mcp-Session-Id` on every later request. What the handshake proposes |
| `2025-06-18` | The same on the wire; only the version string the two ends agree on differs |
| `2025-03-26` | The same again, minus the `MCP-Protocol-Version` header it predates |

It settles them by asking, newest first. `server/discover` answers with every
version a server speaks — but it is itself a method of `2026-07-28`, so it cannot
be the only question. When it comes back empty handed, `initialize` is the older
revisions' own negotiation: `mire` proposes, the server replies with what it will
actually use, and an older answer is the mechanism working rather than a
downgrade to be suspicious of. If neither answers, the newest revision is assumed
and the request goes out as it always did — `server/discover` is a method a
perfectly good server may not implement, and failing there would break endpoints
that work in order to report a problem they do not have.

**It tells you which, every time.** `GET /api/mcp/{id}/tools` carries the
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
---
name: files-on-the-old-one
url: https://mcp.internal/mcp
protocol_version: 2025-06-18
```

A pin skips both probes. Pinning a revision the server refuses gets you the
refusal, which is the point — declare the same URL twice under different pins and
you can say exactly which revisions your endpoint accepts. An unknown one is a
load issue naming what this build speaks, reported at startup like every other
bad entry, without taking the rest of the file down.

Declaring the same server twice is what [stages](configuration.md#stages) are
for, and `config/mcp/weather-staged.yaml` is that against the
[dev stack](dev-stack.md): one file, two entries of one endpoint, one
negotiating its revision and one pinned to it, with their own timeout and their
own tenant header. Same revision either way, and not the same fact — `settled` is
`discovered` for one and `pinned` for the other. A stage is an identity here
rather than a label: each settles on first use and holds its own session, so
nothing one of them agreed is assumed by the other.

### Or state it on the request

`mcp/` is where a pin belongs, and the UI has nothing that disagrees with it: a
revision is a property of the server, not of the tab that happens to be asking.
`POST /api/agent` still takes one, for a caller scripting the question "does this
endpoint still work on `2025-03-26`?" without editing the file:

```json
{ "model": "chat", "prompt": "weather in Paris?", "mcpProtocol": "2025-03-26" }
```

The field is read on a chat run only: an embedding call opens no connection to a
server, so there is no revision for it to be spoken in.

Left out — which is what the UI always does — each server settles its own as
described above, with `protocol_version:` in charge where the file declares one.
Naming a revision overrides both, for that request and no other: it applies to
every server the run reaches (one trace speaking two revisions is a result nobody
can attribute), it is stated rather than probed for, and it leaves the revision
every other caller is speaking exactly where it was. A revision this build does
not speak is a `422` before anything is sent, and `GET /api/mcp` lists the ones
it does.

A session that the server has forgotten — a restart, an expiry, a different
replica — comes back as a `404` to a request that carried one. `mire` handshakes
again and replays the call once, so you see the listing rather than the
plumbing. Twice in a row is reported, because at that point it is not plumbing.

## Choosing what a run reaches

Which servers exist at all is `mcp/`'s business. Which of them **this** run
reaches is a different question, and it comes up constantly — does the model
still get there without the search tool, is that server the thing that has been
failing for ten minutes, what does the loop do when the tool it wants is not
there. All three used to be a config edit, a run, and a config edit back.

The **MCP servers** block — behind **MCP** on the bar above the box — is one card
per file in `mcp/`, each with a switch, and **every one of them starts off**. A
tool call really runs somewhere, so a run reaches a server because somebody said
so in this tab, never because a file was sitting in a directory. A server left
off is not set up: nothing is discovered, nothing is listed, no credential is
fetched, and its tools are not offered to the model. `POST /api/agent` takes the
same thing:

```json
{ "model": "chat", "prompt": "weather in Paris?", "mcpServers": ["files"] }
```

Send a list and the run reaches those, `[]` included: a loop with nothing set up,
offered the model's own simulated `tools:` and nothing else. That empty list is
not the same as saying nothing, on purpose — "none of them" is an answer, and it
should not be spelled the same way as "whatever the file says". Leave the field
out and the run does reach every declared server, which is the API's default and
never the UI's: the block always sends what it was switched on to, `[]` and
all.

**It only ever narrows.** Naming a server `mcp/` does not declare is a `404`
(`unknown_mcp_server`) before anything is sent — a typo, not a server this
request gets to invent. The file stays the authority on what exists, and the
switch decides what this run actually did.

A server that is off carries none of its refusals. One whose browser identity
nobody has signed in to would refuse the first tool call with a `409`; off, it is
not in the run, so the bar says nothing about it and the sign-in it wants is not
on its card either. The card is where being off is said — "nothing is set up, and
the model is offered no live tool", on the block that holds the switch, rather
than reconstructed from an empty trace afterwards.

## A token that does not fit

`auth:` covers a credential in the ordinary place. For the rest — an API key, a
tenant header, a scheme nobody else uses — `headers:` takes MiniJinja templates:

```yaml
---
name: files
url: https://mcp.internal/mcp
headers:
  x-api-key: "{{ env.FILES_API_KEY }}"
  x-tenant: "{{ env.TENANT | default('dev') }}"
```

They are rendered **on every request**, with `env` read fresh each time, so a
rotated token is picked up without restarting — the same property that makes
`value.file` work for a projected service account token. Templates are compiled
when the file loads, so a syntax error names the server at startup rather than
on the first agent run.

An undefined variable is an **error**, not an empty string. `Authorization:
Bearer ` is a header that looks present, passes every local check and fails at
the far end with something unhelpful; you get the variable's name instead. Write
`| default(...)` where a header really is optional.

Rendered values are masked everywhere a credential is masked, and only the header
*names* appear in `GET /api/mcp`. Use `auth:` when it fits — it is one word, and
it comes with the `401`-refresh-and-replay behaviour that a hand-written header
cannot have.

### The credential can come from the auth registry

`env` is not the only source. `auth` is the registry itself, keyed by provider
name, each entry the **bare token** that provider would produce:

```yaml
---
name: files
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
---
name: files
url: https://mcp.internal/mcp
tools:
  - read_file
  - list_directory
```

Annotations (`readOnlyHint`, `destructiveHint`) are reported, never enforced:
they are a server's claim about itself, and this tool is in the business of
checking claims rather than trusting them.

## Hooks: something that happens around a tool call

Calling a live tool is the one thing `mire` does that has effects outside this
process, which makes it the one thing somebody else usually wants to know about
— an audit trail, a policy service, an upload endpoint that has to be handed
the inputs before a task runs. `hooks:` on a server declares that, fired `before`
the call goes out, `after` it comes back, or both.

`config/mcp/weather-hooks.yaml` is the whole of this section as one runnable
file: a `before` gate that can stop the call, a `capture:` that keeps the
reading, and an `after` hook that files it. Both endpoints it talks to are the
[dev stack](dev-stack.md)'s, so `curl -sS localhost:11436/audit` shows what it
actually posted. Uncomment it to watch it run.

```yaml
---
name: files
url: https://mcp.internal/mcp
hooks:
  - name: audit
    on:
      - before
      - after
    actions:
      - http:
          url: https://audit.internal/tool-calls
          auth: keycloak-workload
          json:
            ran: '{{ tool }}'
            arguments: '{{ arguments }}'
```

`actions:` is a list because one event is usually two calls to two different
people: the file goes to the API about to run it, the line goes to the audit
sink. They go out in the order written, each with its own address, credential
and body, and each lands in the trace as its own record. `- http:` is the only
kind so far; the kind is the key it is written under, so the next one is an
addition rather than a break.

**An action sends what it says it sends, and nothing otherwise.** With neither
`json:` nor `multipart:`, the request carries no body at all. That is the point
of the shape: a payload nobody wrote is a request the endpoint never agreed to
read, and the first thing you learn about it is a `422` naming a field your
configuration never mentioned.

`json:` is a document, not a string — written as YAML, sent as JSON, with every
string in it a template:

```yaml
          - http:
              url: https://chat.internal/webhook
              json:
                text: '{{ tool }} on {{ server }} by {{ env.USER }}'
                arguments: '{{ arguments }}'
                attempt: 1
```

A string that is one `{{ … }}` and nothing else keeps the type of what it names,
so `arguments` above is the arguments *object* and `attempt` is a number. Text
around the expression makes it a string again, because that is what interpolation
is for. Without that rule `'{{ arguments }}'` would arrive as a quoted rendering
of a map: valid JSON, wrong type, and the endpoint is the one that finds out.

`{{ call }}` is the whole call in one line, for the sink that wants exactly that:

```yaml
              json: '{{ call }}'
```

```json
{
  "phase": "after",
  "server": "files",
  "tool": "write_file",
  "arguments": { "path": "/tmp/notes", "content": "…" },
  "result": { "text": "written", "isError": false, "latencyMs": 34 }
}
```

`result` appears on the way back only, and it appears even when the call failed
— with `error` naming what went wrong instead. An audit trail that only records
the calls that worked is not one. `env` and `vars` are never in there: a hook
shipping a run's whole variable bag to a third party because somebody wrote
`{{ call }}` is a decision nobody made. Ask for them by name and they are yours.

Undefined is an error in all of this, exactly as it is in a header template, and
every template is compiled when the file loads — so a typo names the hook and
the action at startup rather than twenty minutes into a run. `auth` is
deliberately **not** in scope: a credential belongs in a header, where the
redactor is.

**A hook can stop a call.** `on_error: fail` — the default — means a hook that
could not be run, or whose endpoint answered outside `2xx`, fails the tool call
it belongs to, and nothing after it runs: neither the hook's remaining actions
nor the hooks behind it. On a `before` hook that is a policy gate for free: the
`tools/call` never goes out, and the model is told why so it has a chance to
recover. On an `after` hook it is a report rather than an undo — the tool has
already run, and nothing here can take that back.

```yaml
      - name: gate
        on:
          - before
        tools:
          - write_file
          - delete_file
        on_error: fail
        actions:
          - http:
              url: https://policy.internal/decide
```

`on_error: continue` records the failure and gets out of the way — moving on to
the next action, then to the next hook — which is what you want from an audit
sink you would rather not have gating your runs. The default is the loud one on
purpose: a hook is something you asked for, and a harness that quietly skipped it
would be answering a question you did not ask.

`tools:` narrows a hook to the calls it cares about. Each entry is a regex
matched against the **whole** tool name, so a plain `write_file` still means that
one tool and nothing else:

```yaml
        tools:
          - write_.*
          - delete_file
```

Anchoring is the conservative half of the choice. A gate written as `write_file`
must not quietly grow to cover `overwrite_file_backup` because the matcher got
cleverer, so widening is something you ask for — `write_.*`, or `.*` for
everything. Empty — the default — is every tool. Patterns compile when
the file loads, like the templates beside them: a `tools:` entry that is not a
regex names its hook at startup rather than covering nothing in silence.

**It can send the run's files.** `multipart:` is one entry per form field, each
naming uploads of the run. The request becomes a `multipart/form-data` carrying
exactly those, under exactly those field names — which is what an upload endpoint
asking for `file` is actually asking for, filename and media type included.

```yaml
          - http:
              url: https://intake.internal/jobs/{{ arguments.job_id }}/inputs
              multipart:
                file: '{{ uploads[0].path }}'
```

Each entry names **one** upload — the object itself, or a string matching its
`path`, `name` or `id`. A field can carry several: write a list, or an expression
that produces one, and they go out as several parts under the same name, which is
what every server-side upload handler already reads.

```yaml
              multipart:
                file: '{{ uploads }}'          # everything the run is carrying
                reference: '{{ vars.baseline }}'
```

A field that names a file the run is not carrying **fails the hook**, and so does
one that resolves to nothing at all. Both are the same accident — a form going
out with the part missing — and both are worth stopping for, because the
alternative is an endpoint explaining your own configuration back to you.

`json:` and `multipart:` are two bodies, and declaring both is refused when
the file loads: whichever one `mire` picked would be the other one you meant.
For a webhook that wants the bytes inline instead, the files reach a `json:`
template as `uploads`, whole — `base64`, `dataUrl`, `text`, the entries a model
template gets from the same run:

```yaml
              json:
                file: '{{ uploads[0].base64 }}'
```

The trace never repeats them. It names the field, the file, its size and its
media type, because 25 MB of base64 in a panel costs everything and tells nobody
anything.

**It authenticates like everything else.** `auth:` names a provider and the
credential goes where that provider says; `headers:` takes the same MiniJinja
templates a server's own headers take, `{{ auth["…"] }}` included. Two details
are the hook's own:

- The credential is resolved against the **action's** URL, not the server's, so a
  provider's `allowed_hosts` means what it says. A rule written to keep a token
  off the public internet is not satisfied by the MCP server being internal — and
  two actions of one hook are two different addresses.
- It is resolved **only when the hook fires**. A `tools/list`, or a call to a
  tool this hook does not cover, never pays for a token exchange it has no use
  for.

Every firing lands in the trace, next to the JSON-RPC rather than inside it: a
hook talks to a third address over plain HTTP, and filing a webhook's `POST`
among the MCP methods would make both unreadable. Each turn carries a `hooks`
array — one record per action — the **Hooks** lens in the traffic panel shows
nothing else, the conversation puts a row where the firing happened, and a record
that failed says whether that is also why the tool never ran:

```json
{
  "hook": "gate",
  "step": 1,
  "phase": "before",
  "tool": "write_file",
  "url": "https://policy.internal/decide",
  "status": 403,
  "error": "answered 403 Forbidden: write_file is not allowed here",
  "stoppedTheCall": true
}
```

`step` says which action of the hook spoke, counting from one — two actions can
differ only by what they send, and two cards with the same title would be two
cards you cannot tell apart. Credentials are masked there exactly as they are
everywhere else, and only the header *names* appear in `GET /api/mcp`, beside
what each action sends (`json`, `multipart`, or `nothing`) and the fields it
fills.

Other knobs: `method:` (`POST` by default, because a hook usually carries
something) and `timeout_ms:` (10 s, shorter than a tool's — a slow audit sink must
not look like a slow tool).

## Keeping something a tool call answered

A tool answers, and something in that answer is what the next thing needs: a
session id, a job handle, the path a server just wrote. `capture:` on a server in
A server file names those, by JSONPath, per tool:

```yaml
---
name: files
url: https://mcp.internal/mcp
capture:
  - tools:
      - create_session
    vars:
      session:
        - $.sessionId
```

**It lives on the server, not on a model's model.** A rule is a statement about
a **tool**, and a tool does not belong to a model: `create_session` answers a
session id at `$.sessionId` whether the model that called it is the one you
deployed or the one you are comparing it against. Written once beside the server
that advertises the tool, every `kind: chat` model gets it — and the comparison
between two models is not a comparison between two copies of a rule that have to
be kept identical by hand.

and a hook reads them back as `vars` — in its `url:`, its `json:`, its
`multipart:` and its `headers:`:

```yaml
      - name: audit
        on:
          - after
        actions:
          - http:
              url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
              json:
                session: '{{ vars.session }}'
                ran: '{{ tool }}'
              headers:
                x-session: '{{ vars.session }}'
```

A **server's** own `headers:` see them too, which is the other half of the point:
a tool that opens a session can put that session on every later request to the
server it opened it on.

```yaml
---
name: files
url: https://mcp.internal/mcp
headers:
  x-session: "{{ vars.session | default('') }}"
```

**The `| default('')` there is not decoration.** A server's headers render on
*every* request it makes, and the first of those is the `tools/list` at setup —
before any tool has been called, so before anything has been captured. Without a
default, that run dies negotiating, which is a strange way to find out that a
session is opened by a tool. A hook's headers have no such problem: a hook only
fires around a call, so anything captured before that call is already there.

`url:` is ordinarily just a URL, parsed and checked when the file loads, and it
stays that: a template is only a template when it contains one, so a typo in a
scheme is still a startup issue rather than a string that renders beautifully and
fails on the first tool call. When it *is* a template it sees exactly what
`json:` sees — one context, so there is no second vocabulary to look up.

**A templated URL is resolved before the credential is.** `allowed_hosts` is a
statement about where a credential may go, so it is checked against the address
the request will actually use, not against the one the file happened to be
written with. A template that renders to a host the provider does not allow gets
the refusal.

The notation is JSONPath because [`decode:`](models.md#when-a-cascade-is-not-enough)
already reads responses that way, cascades and all — a list of paths tried in
order, first hit wins:

```yaml
      vars:
        session:
          - $.sessionId
          - $.session.id
```

Paths and `tools:` patterns compile when the file loads, so a typo names the
file and the field at startup. So does a variable name a template could not read:
`{{ vars.my id }}` is not a thing, and finding that out in a rendered URL is
finding it out too late.

Three rules, each of them a decision rather than an accident:

- **Only a real server's tools capture.** A model's simulated
  [`tools:`](agent-loop.md) are answered inside this process and belong to no
  server, so nothing is read out of them however JSON-shaped their answer is.
  `tools:` inside a rule is the same anchored-regex list a hook's is; empty is
  every tool the server offers.
- **An `after` hook sees what its own call just captured.** The capture happens
  as soon as the result lands, before the `after` hooks fire — which is the only
  ordering that lets a hook report on the session the call it wrapped has opened.
  A `before` hook sees what earlier calls captured, since that is all there is.
- **What gets read is `structuredContent` when the server sent one, and the
  result text parsed as JSON otherwise.** A tool answering prose captures
  nothing: there is no path into it. That is not an error — the run carries on —
  but it is a warning, and so is a cascade that resolved none of its paths:

  ```text
  WARN capture: no path resolved, so the variable stays unset
       tool=create_session var=session tried=$.sessionId, $.session.id
  WARN capture: the result is not JSON, so there is nothing for a path to select
       tool=create_session result=`session opened, id 7`
  ```

  A rule that covers a tool has said that tool produces that variable, so a rule
  that comes back empty is a statement that did not hold. Which is the reason to
  name `tools:` rather than leave it empty: an empty one covers every tool, so it
  warns on every call that does not carry the variable.

Nothing about it is silent. Every tool card in the trace carries `captured`, what
*that* call set:

```json
{
  "call": { "name": "create_session", "arguments": {} },
  "source": "mcp",
  "captured": { "session": "abc-123" }
}
```

so a rule that quietly matched nothing is a fact you read there rather than a
mystery in a rendered URL. The UI reads it out: the tool's row in the transcript
names what that call captured, and the tool's card in **Traffic** carries a
**Captured** section with the value each name is worth — the same value the hook
that fired after it rendered. A variable a template names and nobody captured
fails loudly, and the message **names the variable** — `undefined value — `session` is
not set`, the way a header template already reports a missing `env` or `auth` —
rather than rendering away to `/sessions//tool-calls`, which is a different
endpoint that may well answer `200`.

### One bag, however many servers

`capture:` is a list, because "what this server's tools leave behind" is usually
more than one statement, and the rules run in the order the file writes them:

```yaml
---
name: files
url: https://mcp.internal/mcp
capture:
  - tools:
      - create_session
    vars:
      session:
        - $.sessionId
        - $.session.id
  - tools:
      - read_.*
    vars:
      path:
        - $.path
```

The **rules** belong to a server; the **bag** they fill does not. A run reaching
`files` and `audit` applies each server's rules only to its own tools — pooling
them would let one server's `$.sessionId` claim another's answer — and both write
into one set of variables, so a session `files` opened is an address an `audit`
hook can render. One bag per run, thrown away with it, last write wins.

Reloading and issue reporting are the same as everywhere else in the directory: a
broken rule is skipped along with the server that declared it, the rest still
work, and the issue comes back on `GET /api/mcp` naming the file, the server and
what is wrong with it:

```json
{
  "file": "/etc/mire/config/mcp/files.yaml",
  "message": "MCP server `files`: capture rule 1: `my id` cannot be read as `vars.my id`: use letters, digits and underscores"
}
```

`GET /api/mcp` also lists what each server captures — the tool patterns and the
variable **names**, never a path and never a value. A hook's templated URL a few
lines above it is unreadable without knowing where `vars.session` is supposed to
come from.

### A hook that waits for one

Failing loudly is right when the variable *should* be there. When it legitimately
is not there **yet** — a session that a tool opens partway through a run — `if:`
says so:

```yaml
      - name: session-audit
        on:
          - after
        if: '{{ vars.session is defined }}'
        actions:
          - http:
              url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
```

`if:` is a MiniJinja expression, asked once per firing. True and the hook fires;
false and it sits the call out. It sees exactly what `url:`, `json:` and
`headers:` see — `phase`, `tool`, `arguments`, `result`, `env`, `vars`,
`uploads` — because a condition about a call and a body about the same call
should not need two vocabularies. Leaving it out is no condition at all.

The pairing is the point. Without `if:`, that URL fails every call made before a
session exists. With it, the hook sits those out and starts firing once one does.

Being an expression is what makes the rest reachable — the audit that only cares
about the writes that actually worked, the gate that only asks about the big
files, the hook that is for staging only:

```yaml
        if: '{{ result.isError == false }}'
        if: '{{ arguments.size > 1048576 }}'
        if: '{{ vars.session is defined and env.STAGE == "prod" }}'
```

**Undefined is false here, not an error** — the one place in a hook where it is.
Everywhere else a template names something absent, the request would go out with
a hole in it, so it fails loudly; a condition is *asking* whether something is
there, and it has to be able to hear "no" without falling over. Lookups chain, so
`{{ vars.job.id }}` on a run carrying no `job` is false rather than a failure
about `id`. Truthiness is MiniJinja's, so a variable captured as `null` is false
— write `is defined` when presence is the question you meant.

**Not firing is not failing.** `on_error` does not apply, nothing is sent, no
credential is resolved, and the tool call proceeds untouched — the model gets the
real answer. The skip is recorded all the same, quoting what was asked:

```json
{
  "hook": "session-audit", "step": 1, "phase": "after",
  "skipped": "{{ vars.session is defined }}", "status": 0
}
```

A hook that quietly never ran and a hook that was never declared must not look
the same in a trace, and neither must a hook that sat a call out and a hook that
failed: the transcript says `sat out get_weather: {{ vars.session is defined }}
was false`, the card is badged **did not fire** rather than a red status it never
got, and **Failed** leaves it alone.

A condition that cannot be *evaluated* at all — an unknown filter, a call to
something that is not callable — is a different thing, and it is a hook failure
like any other: `on_error` decides what it does to the call. The expression
itself is compiled when the file loads, so a syntax error is a startup issue
naming the hook rather than a surprise on the first tool call. What it *reads* is
checked against nothing — a variable may be captured by this server, by another
one the run happens to reach, or by nobody — so a condition naming a variable
nothing ever captures shows up as a skip quoting it, run after run, which is the
readable version of that mistake.

## The same server in two environments

`stages:` works here exactly as it does on a model:

```yaml
---
name: files
url: ${ stage.base }/mcp
auth: keycloak-user
headers:
  x-tenant: ${ stage.tenant }

default_stage: local
stages:
  local:
    base: http://127.0.0.1:11436
    tenant: sandbox
  prod:
    base: https://mcp.internal
    tenant: acme
```

Both readings are declared, as `files@local` and `files@prod`, and a run takes
one of them: the block shows `files` on one card with a button per stage, and the
call carries `mcpServers: ["files@prod"]` for the one that is pressed. They
negotiate their revision and hold their session separately, which is what keeps a
`local` session from travelling to the `prod` endpoint.

Note the two syntaxes standing side by side in `headers:`: `${ stage.tenant }` is
resolved once, when the file loads, and `{{ env.API_KEY }}` on the line below it
would be resolved on every request. See
[configuration.md](configuration.md#stages).
