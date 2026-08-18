<img src="mire.png" alt="" width="120" align="right">

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
| `--uploads` | `UPLOADS_DIR` | `./uploads` | Where **Attach** writes — see [below](#attaching-a-file) |
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
patch. It runs as UID 65532 and writes nothing, so `--read-only` costs nothing —
right up until somebody presses **Attach**, which is the one thing here that
wants a disk. Give it one, owned by the user the container runs as:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/profiles:/etc/mire/profiles:ro" \
  -v "$PWD/uploads:/var/lib/mire/uploads" \
  -e UPLOADS_DIR=/var/lib/mire/uploads mire:0.1.0
```

Without the mount `--read-only` is still exactly right, and the only thing that
fails is an upload — with a `500` naming the path it could not write. The
directory is created on the first attachment rather than at startup, so nothing
about this changes how the container comes up.

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

- **What the next call will do**, above the box you would make it from. Where it
  goes, who it goes as, and which MCP servers it would set up first — the
  "known signal in" half, said before it happens rather than reconstructed from a
  trace afterwards. When something would refuse the call it says so there and
  offers the way out: every blocker it lists is a refusal `mire` is already known
  to make — an identity or a server no file declares, a credential outside its
  `allowed_hosts` or missing from this tab, a browser session nobody has fetched.
  It says nothing about whether the endpoint is up. That is the question you came
  to ask, and answering it here would be answering it by guessing.
- **Auth, folded away until it is wanted, and read-only.** The identity is the
  profile's, declared in its `auth:` next to the URL it authenticates against,
  so the panel shows it rather than offering alternatives — what you read in the
  file is what went out, and the UI never puts an `auth` of its own on the wire.
  To ask the same endpoint as somebody else, copy the profile and change one
  line; that copy is a thing you can name, keep and re-run, which a click never
  was. A profile with no `auth:` says so and resolves to `anonymous`, where a
  `401` shows up green with a note that the route is protected, because that is
  a pass. A profile naming a credential whose `allowed_hosts` excludes its own
  URL is flagged outright — every call it makes is refused before anything goes
  out.

  It opens from **Auth** on the bar above, and by itself when the way out of a
  blocker is a field inside it. What stays interactive is what no file could
  hold: a credential typed into this tab, and a browser session somebody has to
  go and fetch — **Sign in**, then who you are and a countdown.

  Under it, in its own section, the same panel lists the identities the profile's
  **MCP servers** will use. A separate question answered in a separate file: the
  model's identity comes from the profile, a server's from `mcp.yaml`, and
  neither follows the other. Each row names the provider (or `anonymous`), says
  when it comes from a header template rather than `auth:`, and warns when it is
  a browser provider nobody has signed in to — that call answers
  `409 not_signed_in` and sends nothing, so the **Sign in** button for it is on
  the row itself. Once somebody has been through, the row says who, and carries
  the **Sign out** that drops that identity again — a server's provider is often
  not the profile's, so this row is the only place it appears. A profile naming a
  server `mcp.yaml` does not declare is called out there too.

  That whole section is there in **Agent** mode and gone in **Chat**, along with
  the servers on the bar above and any refusal they would have caused. A chat is
  one turn: it discovers nothing, lists nothing and calls nothing, so the
  profile's servers are not idle during it — they are not in the run at all, and
  neither are the sign-ins they would have needed. A server unticked in
  **Servers** leaves the same way and for the same reason: this run does not
  reach it, so it needs nothing from you.
- **Conversation**, for chat profiles. A transcript: your question on the right,
  the answer on the left, the tools the run called in between, and a composer at
  the bottom. `Enter` sends, `Shift`+`Enter` starts a line. There is one button,
  **Send**, and a **mode** dropdown next to it saying how it goes out. **Agent**,
  which is how it starts: the [loop](#agent-mode), which answers the tool calls
  the model makes until it stops making them, and which a profile with no tools
  ends on turn one anyway — the same one turn a single call would have made.
  **Chat**: one turn, streamed — read chunk by chunk, the text appearing as it
  arrives, the only way to see time to first token. **max turns** is shown as
  inert on **Chat**, because a stream has no second turn to cap, and **Servers**
  and **Protocol** go away entirely, because there is no server for either of
  them to be about. **Servers** is a checkbox per server the profile names:
  untick one and this run does not set it up, does not sign in to it and is not
  offered its tools — the file still names it, and the run
  [says so](#switching-one-off-for-a-run) rather than shrinking quietly.
  While a run is in flight, **Stop** drops the request where it stands and
  whatever had arrived stays on the page — a stream cut off after four tokens
  produced four tokens, and that is a finding rather than a mess to clear up.
  Nothing is sent upstream to call the work off: an endpoint that has been asked
  a question is going to answer it, so this is about your tab and says only that.
  More on what the transcript is [below](#having-a-conversation).
- **Saved**, a dropdown above either box. The prompts `prompts.yaml` declares,
  picked by name and dropped in the box — nothing is sent, and what the text
  becomes on the wire is still the profile's template's decision. More
  [below](#saving-a-prompt).
- **Input**, for embedding profiles. One text per line, a run count, and a
  checkbox for the full vectors. There is no second turn of an embedding, so
  there is no conversation and no loop.
- **Traffic**, under the conversation. Everything that left the process, in the
  order it left, one card per exchange, filtered by kind or down to the failures
  — and reachable from the transcript above, which names the card each of its
  rows summarises. See [below](#reading-the-traffic).
- **Profiles**, a column where there is room for one and a fold-away where there
  is not — on a phone the list was a screenful to scroll past before reaching the
  thing it configures.
- **Embedding.** Count, width, encoding, the five checks, and per vector its
  norm, a sample of the first values and a distribution histogram. Never a wall
  of floats.

A credential typed into the UI lives in that tab and nowhere else: it is sent
with the call and never stored, never logged, never echoed back. A credential
`mire` fetched for you never reaches the tab at all — the browser sees a
username, the granted scopes and a countdown.

**The tab remembers a little, and never that.** Which profile you were on, what
you had half typed, how many turns you allow, which revision you pinned, which
MCP servers you switched off — small
settings whose loss is pure annoyance, kept in the browser's own storage. The
credential is not among them, and neither is the conversation or the traffic: a
session's bodies are unbounded, and the first oversized run would start throwing
quota errors at a tool whose job is to be dependable while other things fail.
Storage that is missing or full is a browser with no memory, never a page that
fails to load. `mire` still holds nothing — this is the same side of the wire the
conversation has always been on.

### Having a conversation

A chat profile keeps its turns. Send, get an answer, ask a follow-up: the
question goes out with everything that came before it. It reads like a chat
window, because that is the fastest way to tell whether a model is following you.

**The conversation lives in the browser, not in `mire`.** There is no session,
no identifier, nothing to expire. The whole history travels in the body of every
request, which is what keeps the promise the rest of this tool makes: the *Copy
as curl* of turn five reproduces turn five, in a shell, tomorrow, on a machine
that never had the tab open. A server-side conversation would turn that button
into a lie.

So the transcript is not a log *of* the `messages` array — it **is** the array,
laid out. Every turn keeps a **Retry**, because asking again is the point:
dropping the model's answer and putting the question back on the wire is how you
find out whether it only said that because it had already said it.
**New conversation** clears the lot.

Four things follow, each of which is a decision:

- **Any turn can be run again, and an empty box sends nothing.** **Retry** on an
  answer drops it and asks the question underneath it again; **Retry** on a
  question — which is what a failed call leaves behind — sends it as it stands.
  A turn that went perfectly well is worth repeating too, and usually the most
  interesting one to repeat, which is why it is offered on every turn rather
  than only on the last. Either way the turns after it go: the transcript *is*
  the next request, so no turn can be replayed with its own future still
  attached — the button says how many it takes with it, and what leaves the
  conversation stays in **Traffic**, which keeps every exchange this tab ever
  made. **Send** stays greyed out until there is something to say, so no request
  ever leaves without the transcript showing what it carried.
- **Only the answer the run finished on rejoins the history.** The tool calls in
  between and their results stay out of it: replaying them into the next request
  without their results is how you get a `400` from an endpoint that was working
  fine. They still appear in the transcript, where they happened, as a line
  naming the tool, whether it really ran, and any variable it captured — the
  full exchange, and what that variable is worth, is in **Traffic**. A tool call that *does* land in the history — from a streamed
  send, which does not loop, or from a run that stopped on one — is flagged on
  its bubble, because most endpoints refuse the next turn until it has a result.
- **Nothing waits for the endpoint.** The question appears the moment you press
  Enter, tool calls appear as the loop makes them, streamed text appears as it
  arrives. A call that fails leaves the question in the history, which is
  exactly what **Retry** picks back up.
- **The model's half is rendered as markdown, and only the model's half.**
  Endpoints answer in markdown whether or not anyone asked them to, and reading
  the asterisks and the backticks instead of what they meant is a tax on every
  answer. So headings, lists, tables and fenced code are rendered — while the
  answer is still streaming, too, caret and all. Your questions, tool results and
  system prompts are shown exactly as they are, because those are not prose: they
  are what is about to go on a wire. Nothing is rendered from HTML, so a
  `<script>` in an answer is text about a script, and a `javascript:` link is not
  a link. And the raw string is never more than a glance away — the response body
  the endpoint actually sent is a card down in **Traffic**, byte for byte.

### Attaching a file

**Attach** writes a file to `mire`'s upload directory — `--uploads`, `./uploads`
by default — and lists what it stored. That is the whole feature, and the next
sentence is the important one.

**The file goes to the template, not to the endpoint.** The next **Send** hands
it over as `uploads`, and what happens next is the profile's decision: a template
that never mentions `uploads` sends exactly what it always sent, the same way one
that never mentions `stream` never streams. That is not a limitation to work
around — it is the only arrangement in which "what did we send?" has one answer,
written down, in a file you can read.

So an attachment is an ingredient, and the profile is the recipe:

```jinja
{
  "model": "gpt-4o",
  "messages": [
    {
      "role": "user",
      "content": [
        {"type": "text", "text": {{ messages[-1].content | tojson }}}
        {% for file in uploads %},
        {"type": "image_url", "image_url": {"url": "{{ file.dataUrl }}"}}
        {% endfor %}
      ]
    }
  ]
}
```

Each entry of `uploads` carries the file whole, three ways, so the template picks
the one its endpoint reads:

| Field | What it is |
| --- | --- |
| `base64` | The bytes, standard base64 |
| `dataUrl` | The same, as `data:<type>;base64,…` — what most vision endpoints want |
| `text` | The file decoded as UTF-8, or `null` when it is not text |
| `name` | File name, without the random prefix |
| `storedAs` | What it is called on disk, prefix included |
| `path` | Where it is, for a template that only wants to say so |
| `size` | Bytes |
| `contentType` | Guessed from the extension; `null` when the extension says nothing |

`text` is the test for "is this readable", which is what makes the other common
case a two-line template — a log or a CSV inlined into the question:

```jinja
"messages": [
  {% for file in uploads %}{% if file.text %}
  {"role": "user", "content": {{ ("Contents of " ~ file.name ~ ":\n" ~ file.text) | tojson }}},
  {% endif %}{% endfor %}
  {% for message in messages %}{{ message | tojson }}{% if not loop.last %},{% endif %}{% endfor %}
]
```

A request `script:` sees the same `uploads`, because it is the same context
serialised — nothing is reachable from one request source and not the other.

Two things the fields cannot tell you, both of them the price of `mire` keeping
no state: `name` is the **sanitised** name rather than what your browser called
the file, and `contentType` is guessed from the extension rather than taken from
what the browser claimed. Neither was written to disk, and the disk is the only
thing that survives a restart. A template that knows better writes the type
itself.

Attachments are re-rendered on **every turn** of an agent loop, since the body is
built from the template each time. And they are inlined into a request body, so
they arrive in **Traffic** at their full base64 size — a 12 MB photo is a 16 MB
request to scroll past. Attach the file you meant to test with.

What the server does with the name it is given is worth knowing, since it is the
one place `mire` writes anything:

- **The name is a display name, never a path.** It is reduced to its last
  segment, non-portable characters are replaced, and leading dots go. A file
  called `../../.ssh/authorized_keys` is stored as `authorized_keys`, in the
  upload directory, like everything else. Nothing a client sends can write
  outside it.
- **Nothing is overwritten.** Every stored name carries a random prefix, so
  attaching `payload.json` twice is two files. The response says which name is
  yours and which is the one on disk; they are never the same string.
- **25 MB per file**, refused with a `413` naming the limit rather than a
  truncated file.
- **The directory is created on the first upload**, not at startup — so a
  read-only filesystem is only a problem for somebody who actually attaches
  something.
- **× forgets, it does not delete.** The file stays where it was written.
  Deleting things off a disk because a browser tab said so is not a thing this
  process does; the directory is yours to empty.

### Saving a prompt

A profile says how to reach an endpoint. A prompt says what to send it, and that
half is worth keeping for the same reason the first one is — the question that
makes it call the tool, the one that makes it refuse, the paragraph that
reproduces the bug. Retyping those from memory is how a comparison quietly stops
being one.

They live in `prompts.yaml`, next to the profiles, `auth.yaml` and `mcp.yaml`:

```yaml
prompts:
  - name: ping
    text: ping

  - name: call a tool
    text: What is the weather in Lyon right now?

  - name: strict json
    text: |
      Answer with a JSON object and nothing else — no prose, no fences.
      Keys: "city" (string), "temperature_c" (number), "measured_at" (RFC 3339).
```

A name and its text. That is the whole shape, and the omissions are the design:

**Read-only, like everything else in this directory.** The file is the source of
truth, your editor writes it, the watcher picks the change up without a restart.
So a prompt is a thing you can commit, review and hand to somebody else — which
a button that wrote into a config directory would not have been, and which is
also why the container can still run `--read-only`. A bad entry is reported in
the UI and skipped; the rest still work, the same policy every other file here
gets.

**A prompt says nothing about where it goes.** No profile, no kind, no `auth:`.
Picking one fills the box and stops there — nothing is sent, and what the text
becomes on the wire is still the profile's template's decision. That is what
lets the same question be replayed against every endpoint in the directory,
which is the comparison you came here to make. The same library is offered to
an embedding profile's **Input** box, where one saved text can be several: the
box is one text per line and a multi-line `text:` arrives whole.

**Picking replaces what is in the box.** It is the honest reading of "load the
saved one", and the dropdown says so. It returns to `pick one…` afterwards, so
reaching for the same prompt a second time works rather than looking like a
control that stopped responding.

The whole library is `GET /api/prompts`, in the order the file writes them — a
list somebody arranged is not re-sorted on the way out.

### Reading the traffic

Under the conversation, **Traffic** is every wire this process touched, in the
order it touched them. **Cards land folded**: the summary line — turn, kind,
status, latency, the badges that say something went wrong — is the list, and the
list is a table of contents before it is a transcript. You open the one you came
for, and *Expand all* opens the lot when the whole run is what you are reading.
Three kinds of card, each showing what went out and what came back:

| | Model call | MCP round trip | Tool call |
| --- | --- | --- | --- |
| **Request** | Method, URL, masked headers, body, *Copy as curl* | The JSON-RPC that went out, with its headers and the revision it went out on | The arguments the model produced |
| **Decode** | Which configured path matched which field, which missed, and everything that was tried | — | Whether those arguments match the schema the tool was declared with |
| **Response** | Status, latency, decoded content and tool calls, whatever the endpoint said went wrong, stream counters, the body, the raw JSON as a tree | Status, latency, and the JSON-RPC that came back | What the tool handed back, and whether it reported a problem |
| **Captured** | — | — | Every variable [`agent.capture`](#keeping-something-a-tool-call-answered) pulled out of that answer, by name and by worth — only when a rule matched |

**The transcript points at the cards.** A tool row in the conversation is a
summary of one of these, so it takes you to it: the tool's name opens its own
card, and the turn beside it opens the model call that asked for the tool. **The
hooks are rows there too**, on the side of the call they fired on — the gate
above the call it let through, the audit below it — each naming what it did:
`fired before get_weather · 204 · 8 ms`, `stopped the call`, or `sat out
get_weather, waiting for session`. A gate that refused explains a tool that never
ran, and a hook nobody can see explains nothing. Any filter in the way is dropped
on the way — a click that appeared to do nothing because the card was behind a
filter set four minutes ago would be worse than no link at all.

**And the list can be asked a narrower question.** A run puts five cards on the
page and a session puts fifty, so **Model**, **Tools** and **Protocol** each show
one kind, and the count beside them says how much is being hidden. **Failed**
picks out the exchanges worth looking at first: a status the endpoint should not
have answered, a stream that stopped rather than ended, a handshake that never
landed, a tool that failed, reported a problem, or was called with arguments its
own schema refuses, or a hook that was refused or never landed. A `401` you asked
for anonymously is a pass, so it is not one of them — and neither is a hook that
[sat a call out](#a-hook-that-waits-for-one), which sent nothing and broke
nothing. When nothing failed the button says so rather than offering an empty
list.

**And the whole run comes out as a file.** *Export* writes every exchange above
to JSON, with the endpoint it was pointed at, the identity it went as, and the
history as the next request would have carried it. What *Copy as curl* does for
one request, this does for the run — the order, the turns, and what the decoder
made of each answer, which a single reproduced call loses. Nothing is summarised
on the way out: the person you send it to will want the part you did not think
was interesting. It is built in the page, because the page is the only place the
run exists as a whole — the server answers one call at a time and keeps none of
them.

**Every body is a foldable tree**, in both directions and on all three cards —
the same view the raw response always had, because finding where an endpoint hid
a field is the job and a wall of text is the one shape that does not help with
it. A body that is not JSON is shown as itself: an HTML error page from a gateway
is a finding, and prettifying it would hide the finding. The one-line version is
what *Copy as curl* still hands you — that button reproduces the call, this panel
explains it.

**Every word said to an MCP server is here, not just the tool calls.**
`server/discover`, `initialize`, `notifications/initialized` and `tools/list`
happen before the first prompt is spent, so they appear before the first turn,
labelled **Setup**. That is not bookkeeping: a run whose handshake came back
`401` calls no tools at all, and a panel that only listed tool calls would show
an empty run and no reason for it. The same goes for a session the server forgot
mid-run — the second `initialize` is right there between the turns that straddle
it.

Model, protocol and tool sit in one list on purpose. A run is only explicable
when you can read them against each other in order — the model asked for a tool
with *these* arguments, *this* went to the server, it answered *that*, and the
next request carried it *like this*. Split across panels, or reset per turn, and
the comparison stops being possible.

Every tool card says whether it really left the process: a call that did names
the server that answered and how long it took, and one that did not is marked
**simulated, nothing executed** (see [agent mode](#agent-mode)). A
plausible-looking result from a tool nothing wired up is the easiest way to
believe an integration works.

The list **accumulates across the whole conversation** rather than resetting on
every send, because "it worked on turn one and not on turn four" is a comparison
and a panel showing only the latest turn cannot make one. *Expand all* opens
everything it holds and *Collapse all* folds it back; *Clear* empties it, on
purpose, when you want a clean read.

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

Two profiles come with it, one per kind:

- **`qwen3`** — the chat one, and it carries everything `mire` does with a chat
  endpoint at once: a template driven by the call, a decode cascade, an agent
  loop, real MCP tools, and an `auth:`. It points at the **gateway**, not at
  Ollama, which is what turns auth into a question with an answer. Change that
  one line — or `POST /api/call` with an `auth` override — and you get the matrix
  this tool exists for:

  ```
  anonymous          -> 401   the route is protected — a pass
  static-token       -> 200
  keycloak-workload  -> 200   a token fetched for a service account
  keycloak-user      -> 200   a token fetched for you, after signing in
  ```

- **`nomic`** — embeddings, straight at Ollama with no credential, and with
  `$.data[*].embedding` deliberately kept first in the cascade so you can watch
  it miss and `$.embeddings` take over. No `auth:`, so it calls as `anonymous`
  and the panel says as much.

Both files are commented with the edit that turns them into the next experiment
— `qwen3` in particular is two lines away from Ollama's *own* chat API, where the
content sits at `$.message.content`, the stop reason is called `done_reason` and
the token counters are at the top level. Its cascades already list both shapes,
so nothing has to change to follow it and the decode trace names the path that
won. That divergence — same model, same weights, two answers to "are you
finished?" — is the sort of thing this tool exists to make visible.

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

Any provider may add `allowed_hosts`, and every kind honours it:

```yaml
    allowed_hosts:
      - models.internal
```

An empty list — the default — means anywhere. A non-empty one is a rule about
where that credential may be sent, refused before anything goes out, and it is
also what keeps the UI from offering a provider against a profile pointing
somewhere it is not allowed to go.

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
    -d "{\"profile\": \"qwen3\", \"auth\": \"$auth\", \"prompt\": \"ping\"}" |
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

### See exactly what was sent

Every call hands back the request it made, with its `curl` equivalent. This is
the thing you paste into a ticket:

```sh
curl -s localhost:8787/api/call \
  -H 'content-type: application/json' \
  -d '{"profile": "qwen3", "prompt": "ping"}' | jq -r .curl
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

One of those fields is not about the answer at all. `decode.error` points at
whatever the endpoint says when there is no answer, and what comes back is
normalised the same way everything else is:

```yaml
decode:
  error:
    - $               # read wherever the complaint sits inside the body
    - $.detail        # a gateway that answers like FastAPI
```

```json
{
  "error": {
    "message": "This model's maximum context length is 32768 tokens.",
    "type": "invalid_request_error",
    "code": "context_length_exceeded",
    "raw": {"message": "…", "type": "…", "code": "…", "param": "messages"}
  }
}
```

`{"error": {"message": …}}`, a bare `{"error": "model not found"}`, a flat
`{"message": …, "code": 503}` and an OAuth2 `{"error": "invalid_token",
"error_description": …}` all land in those three fields, and `raw` keeps the node
verbatim so nothing normalisation did not understand is lost.

**The status is never consulted.** A gateway that swallows an upstream failure
and answers `200` with the complaint in the body is exactly the mismatch this
catches — the UI badges it, and the traffic panel's failures filter finds it. The
rule runs the other way too: a cascade that finds nothing under a `2xx` is not
reported as a miss, because there was nothing to find; under a `4xx` or a `5xx`
it is, because that is a profile with a blind spot.

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
edit the file. See [`profiles/qwen3.yaml`](profiles/qwen3.yaml) for a worked
example — one `decode:` block covering two unrelated response shapes, of which
only the first wins until you point the profile at the other endpoint.

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
`content` / `tool_calls` / `finish_reason` / `usage` / `error` for a chat profile,
`vectors` / `usage` / `error` for an embedding one. `error` is read like the
cascade reads one — a string is the message, a map is looked at for the usual
keys — so a script can report a failure the body alone does not admit to:

```yaml
decode:
  script: |
    let upstream = headers["x-upstream-status"];
    if upstream != "200" {
      #{ error: #{ message: `upstream answered ${upstream}`, code: upstream } }
    } else {
      #{ content: raw.message.content }
    }
```

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

Neither shipped profile uses one, on purpose: they are meant to be read, and a
script is what you add once a cascade has already failed you. The snippet above
is the real motivating case — qwen3 emits its reasoning inside a
`<think>…</think>` block, and no path can strip a prefix.

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

#### Or choose it per run

Editing a file, restarting and putting it back is a lot of ceremony for one
question. The **Protocol** dropdown in the composer — next to **max turns**,
because both are parameters of the run — asks it directly, and `POST /api/agent`
takes the same thing:

```json
{ "profile": "chat", "prompt": "weather in Paris?", "mcpProtocol": "2025-03-26" }
```

The dropdown is there in **Agent** mode only, and so is the endpoint that reads
it: a chat opens no connection to a server, so there is no revision for it to be
spoken in.

`auto` — the default, and the field simply left out — is the negotiation as
described above, with `protocol_version:` still in charge where a server declares
one. Naming a revision overrides both, for that run and no other: it applies to
every server the profile reaches (one trace speaking two revisions is a result
nobody can attribute), it is stated rather than probed for, and it leaves the
revision every other caller is speaking exactly where it was. `mcp.yaml` remains
the place for a pin you want to keep. A revision this build does not speak is a
`422` before anything is sent, and `GET /api/mcp` lists the ones it does.

A session that the server has forgotten — a restart, an expiry, a different
replica — comes back as a `404` to a request that carried one. `mire` handshakes
again and replays the call once, so you see the listing rather than the
plumbing. Twice in a row is reported, because at that point it is not plumbing.

### Switching one off for a run

Which servers a profile *may* reach is the profile's business: `mcp:` is opt-in,
never implied, because a tool call here really runs somewhere. Which of them
**this** run reaches is a different question, and it comes up constantly — does
the model still get there without the search tool, is that server the thing that
has been failing for ten minutes, what does the loop do when the tool it wants is
not there. All three used to be a profile edit, a run, and a profile edit back.

The **Servers** row in the composer, above **Protocol**, is one checkbox per
server the profile names. Untick one and this run does not set it up: nothing is
discovered, nothing is listed, no credential is fetched, and its tools are not
offered to the model. `POST /api/agent` takes the same thing:

```json
{ "profile": "chat", "prompt": "weather in Paris?", "mcpServers": ["files"] }
```

Leave the field out — the default — and the run reaches every server the profile
names, which is what the file says. Send a list and it reaches those, `[]`
included: a loop with nothing set up, offered the profile's own simulated
`tools:` and nothing else. That empty list is not the same as saying nothing, on
purpose — "none of them" is an answer, and it should not be spelled the same way
as "whatever the file says".

**It only ever narrows.** Naming a server the profile does not is a `422`
(`mcp_server_not_offered`) before anything is sent, not a server this request
gets to add — the opt-in lives in a file somebody wrote and reviewed, and a
request body is not where it is granted. So the file stays the authority on what
a profile may touch, and the checkbox decides what this run actually did.

Switching one off takes its blockers with it. A server whose browser identity
nobody has signed in to would have refused the first tool call with a `409`; off,
it is not in the run, so the preflight bar goes green and the sign-in it was
asking for disappears from the auth panel — the same way both vanish in **Chat**
mode. What stays is a line naming what was left out, because a run reaching fewer
servers than its profile declares is a fact you want in front of you rather than
one to reconstruct from the traffic afterwards.

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

### Hooks: something that happens around a tool call

Calling a live tool is the one thing `mire` does that has effects outside this
process, which makes it the one thing somebody else usually wants to know about
— an audit trail, a policy service, a webhook that pages whoever owns the server
being poked. `hooks:` on a server declares that, fired `before` the call goes
out, `after` it comes back, or both:

```yaml
servers:
  - name: files
    url: https://mcp.internal/mcp
    hooks:
      - name: audit
        on:
          - before
          - after
        action:
          kind: http
          url: https://audit.internal/tool-calls
          auth: keycloak-workload
```

`kind: http` is the only action so far. It is tagged anyway, because a `kind:`
added after the fact is a breaking change to every file that already exists.

With no `body:`, the payload is the call itself:

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
the calls that worked is not one.

A `body:` template replaces the payload and sees the same fields by name, plus
`env` and [`vars`](#keeping-something-a-tool-call-answered):

```yaml
        action:
          kind: http
          url: https://chat.internal/webhook
          body: '{"text": "{{ tool }} on {{ server }} by {{ env.USER }}"}'
```

Undefined is an error there, exactly as it is in a header template, and the
template is compiled when `mcp.yaml` loads — so a typo names the hook at startup
rather than twenty minutes into a run. `auth` is deliberately **not** in scope:
a credential belongs in a header, where the redactor is.

**A hook can stop a call.** `on_error: fail` — the default — means a hook that
could not be run, or whose endpoint answered outside `2xx`, fails the tool call
it belongs to. On a `before` hook that is a policy gate for free: the
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
        action:
          kind: http
          url: https://policy.internal/decide
```

`on_error: continue` records the failure and gets out of the way, which is what
you want from an audit sink you would rather not have gating your runs. The
default is the loud one on purpose: a hook is something you asked for, and a
harness that quietly skipped it would be answering a question you did not ask.

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
`mcp.yaml` loads, like the body template: a `tools:` entry that is not a regex
names its hook at startup rather than covering nothing in silence.

**It can carry the run's files.** `files:` names uploads the way `tools:` names
tools, and what it names goes out for real: the request becomes a
`multipart/form-data` with the body demoted to a `payload` part and one `file`
part per attachment, filename and media type included.

```yaml
        action:
          kind: http
          url: https://audit.internal/tool-calls
          files:
            - .*\.pdf
```

Empty — the default — attaches **nothing**, which is the opposite of what an
empty `tools:` means. The asymmetry is on purpose: a hook covering every tool is
merely wide, while a hook shipping every file somebody attached to a third
address is a leak. `.*` asks for all of them, out loud.

The same files reach a `body:` template as `uploads`, whole — `base64`,
`dataUrl`, `text`, the entries a model template gets from the same run — so a
webhook wanting the bytes inline can have them:

```yaml
          body: '{"file": "{{ uploads[0].base64 }}"}'
```

The default payload describes them instead, by `id`, `name`, `size` and
`contentType`. The bytes are already going out as parts, and putting them in the
body as well would send every file twice. The trace makes the same choice: it
names what was attached and carries none of it, because 25 MB of base64 in a
panel costs everything and tells nobody anything.

**It authenticates like everything else.** `auth:` names a provider and the
credential goes where that provider says; `headers:` takes the same MiniJinja
templates a server's own headers take, `{{ auth["…"] }}` included. Two details
are the hook's own:

- The credential is resolved against the **hook's** URL, not the server's, so a
  provider's `allowed_hosts` means what it says. A rule written to keep a token
  off the public internet is not satisfied by the MCP server being internal.
- It is resolved **only when the hook fires**. A `tools/list`, or a call to a
  tool this hook does not cover, never pays for a token exchange it has no use
  for.

Every firing lands in the trace, next to the JSON-RPC rather than inside it: a
hook talks to a third address over plain HTTP, and filing a webhook's `POST`
among the MCP methods would make both unreadable. Each turn carries a `hooks`
array, the **Hooks** lens in the traffic panel shows nothing else, the
conversation puts a row where the firing happened, and a record that failed says
whether that is also why the tool never ran:

```json
{
  "hook": "gate",
  "phase": "before",
  "tool": "write_file",
  "url": "https://policy.internal/decide",
  "status": 403,
  "error": "answered 403 Forbidden: write_file is not allowed here",
  "stoppedTheCall": true
}
```

Credentials are masked there exactly as they are everywhere else, and only the
header *names* appear in `GET /api/mcp`.

Other knobs: `method:` (`POST` by default, because a hook carries a payload) and
`timeout_ms:` (10 s, shorter than a tool's — a slow audit sink must not look like
a slow tool).

### Keeping something a tool call answered

A tool answers, and something in that answer is what the next thing needs: a
session id, a job handle, the path a server just wrote. `capture:` in a profile's
`agent:` block names those, by JSONPath, per tool:

```yaml
agent:
  capture:
    - tools: [create_session]
      vars:
        session: [$.sessionId]
```

and a hook reads them back as `vars` — in its `url:`, its `body:` and its
`headers:`:

```yaml
      - name: audit
        on:
          - after
        action:
          kind: http
          url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
          body: '{"session": "{{ vars.session }}", "ran": "{{ tool }}"}'
          headers:
            x-session: '{{ vars.session }}'
```

A **server's** own `headers:` see them too, which is the other half of the point:
a tool that opens a session can put that session on every later request to the
server it opened it on.

```yaml
servers:
  - name: files
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

`url:` is ordinarily just a URL, parsed and checked when `mcp.yaml` loads, and it
stays that: a template is only a template when it contains one, so a typo in a
scheme is still a startup issue rather than a string that renders beautifully and
fails on the first tool call. When it *is* a template it sees exactly what
`body:` sees — one context, so there is no second vocabulary to look up.

**A templated URL is resolved before the credential is.** `allowed_hosts` is a
statement about where a credential may go, so it is checked against the address
the request will actually use, not against the one the file happened to be
written with. A template that renders to a host the provider does not allow gets
the refusal.

The notation is JSONPath because [`decode:`](#when-a-cascade-is-not-enough)
already reads responses that way, cascades and all — a list of paths tried in
order, first hit wins:

```yaml
      vars:
        session: [$.sessionId, $.session.id]
```

Paths and `tools:` patterns compile when the profile loads, so a typo names the
file and the field at startup. So does a variable name a template could not read:
`{{ vars.my id }}` is not a thing, and finding that out in a rendered URL is
finding it out too late.

Three rules, each of them a decision rather than an accident:

- **Simulated and live tools capture alike.** What a variable is worth does not
  depend on which of the two answered, and stubbing `create_session` is how you
  try a capture rule out before pointing it at a server. `tools:` is the same
  anchored-regex list a hook's is; empty is every tool.
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
endpoint that may well answer `200`. The bag lasts one run, is shared by every
server that run talks to, and last write wins.

#### A hook that waits for one

Failing loudly is right when the variable *should* be there. When it legitimately
is not there **yet** — a session that a tool opens partway through a run —
`when_defined:` says so:

```yaml
      - name: session-audit
        on:
          - after
        when_defined:
          - session
        action:
          kind: http
          url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
```

Plain names, not patterns: this asks whether a value exists, and the place to
*use* it is `url:`, `body:` or `headers:`. Every name listed has to be there;
empty — the default — is no condition at all.

The pairing is the point. Without `when_defined:`, that URL fails every call made
before a session exists. With it, the hook sits those out and starts firing once
one does.

**Not firing is not failing.** `on_error` does not apply, nothing is sent, no
credential is resolved, and the tool call proceeds untouched — the model gets the
real answer. The skip is recorded all the same, naming what it waited for:

```json
{ "hook": "session-audit", "phase": "after", "skipped": "session", "status": 0 }
```

A hook that quietly never ran and a hook that was never declared must not look
the same in a trace, and neither must a hook that sat a call out and a hook that
failed: the transcript says `sat out get_weather, waiting for session`, the card
is badged **did not fire** rather than a red status it never got, and **Failed**
leaves it alone. Note that presence is the test, not truthiness: a path that
resolved to `null` resolved, and `when_defined:` does not second-guess the
capture that accepted it.

Names are checked against what the run captured and nothing else — there is no
startup check that some profile fills them, because `mcp.yaml` does not know
which profiles will use the server. A name nothing ever captures shows up as a
skip naming it, run after run, which is the readable version of that mistake.

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
modes: the **mode** dropdown next to **Send** is what asks for chunks rather than
a whole answer, and **Chat** is the mode to be on when time to first token is the
number you came for. Keep the `| tojson`. MiniJinja renders a bare boolean as
`True`, which is Python and is not JSON — rendering catches it and shows you the
body, but it is a nicer trap to avoid than to diagnose.

`decode.delta` is a separate cascade from `decode.content` because a chunk is not
a small response: OpenAI moves the text from `message` to `delta`, Ollama keeps
`message` and sends one object per line. Both spellings fit in one block, and the
trace says which one matched.

Nothing here makes an endpoint stream. The endpoint has to be asked, in its own
request body — which is why the flag goes through the template rather than being
something `mire` does on your behalf.

```console
$ curl -N -X POST localhost:8787/api/call/stream -d '{"profile":"qwen3","prompt":"…"}'
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
  -d '{"profile": "nomic", "input": ["one", "two"], "repeat": 2}' | jq .response.decoded
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

Which is why the UI's two modes are not two profiles: **Send** on **Agent** is
the loop, whatever the profile declares, and **Send** on **Chat** is one streamed
turn of the very same thing. A profile that declares no tool stops on turn one,
and turn one is the single call **Chat** would have made — so the choice is about
how the answer arrives, not about which of two mechanisms the profile is for.

The one thing that does not carry over is the servers. Only the loop sets an MCP
server up, so **Chat** speaks to none of them — `POST /api/call` and `POST
/api/call/stream` never discover, list or call a tool, whatever the profile's
`mcp:` says, and the UI drops their identities and their revision from the page
while that is the mode. The tool list the model is offered is then the profile's
own `tools:` and nothing else — which is also what a loop reaching
[no server](#switching-one-off-for-a-run) is offered, and the reason switching
them all off answers a question a chat cannot: what the *loop* does when the tool
it wants is not there.

It runs on the [conversation](#having-a-conversation) in the browser, and when it
stops it appends the answer it finished on. The turns in between — the tool calls
and their results — stay out of the history: they are what the run was about, and
replaying them into the next request without their results is how you get a `400`
from an endpoint that was working fine. They are not lost, though. Each one lands
in the transcript where it happened and in full in
[**Traffic**](#reading-the-traffic), request, decode and response, next to the
model call that asked for it.

```yaml
agent:
  stop_when:
    no_tool_calls: true          # the default, and almost always what you want
    finish_reason_in: [stop, end_turn]
    repeated_call: true          # off by default: stop on the same call twice
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

`agent:` also takes `capture:`, which keeps something out of a tool's result for
a hook to use later — see
[above](#keeping-something-a-tool-call-answered).

**Nothing is executed.** The tools are simulated — a fixed string, or a Rhai
script that sees `arguments`, `name` and `turn`. What is being checked is that
the model emits calls matching the schema it was given, and knows what to do with
a result. Arguments are validated against that schema and the mismatches are
reported; the model still gets an answer, so it has a chance to correct itself. A
tool the profile never declared gets an error back rather than silence.

`POST /api/agent` streams server-sent events: one `setup` event if the profile
has MCP servers, a `turn` event per turn as it happens, then one `done` carrying
the whole trace. Each turn holds the rendered request, the masked headers, the
`curl` equivalent, the raw response, the decode trace and the tool results — the
same shape `POST /api/call` returns — plus `mcp`, every JSON-RPC round trip that
turn made, request and response, credentials already masked, and `hooks`,
everything that fired around those tool calls.

`setup` carries the same shape for what happened before the loop: discovery, the
handshake, `tools/list`. It arrives first because it happened first, and a run
that dies negotiating never reaches a turn to report it on. `done` repeats it
under `setup`, so a client that only reads the trace still has it.

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
arguments twice, which is a loop rather than progress. That last one is opt-in
(`stop_when.repeated_call`): re-reading a tool it already called is often a model
working rather than spinning, and `max_iterations` bounds the run either way.

Try it against the local stack: `qwen3` fetches `get_weather` from the `dev` MCP
server and really calls it. On a CPU-only Ollama a two-turn run takes about
ninety seconds.

## API

`/docs` serves a Scalar reference against `/openapi.json`.

| Route | What it does |
| --- | --- |
| `GET /api/profiles` | Every profile, plus the files that failed to load and why |
| `GET /api/profiles/{name}` | One profile, as declared |
| `GET /api/prompts` | Prompts declared in `prompts.yaml`, plus the entries that did not load |
| `GET /api/auth` | Auth providers, with session status |
| `GET /api/mcp` | MCP servers declared in `mcp.yaml` |
| `GET /api/mcp/{name}/tools` | Ask a server what it offers, right now, and on which revision |
| `POST /api/auth/{name}/login` | Start a browser login; returns where to send it |
| `POST /api/auth/{name}/logout` | Forget the session `mire` holds |
| `POST /api/call` | Render, authenticate, send, decode |
| `POST /api/call/stream` | The same, read chunk by chunk, with time to first token |
| `POST /api/agent` | The same, in a loop, streamed as server-sent events |
| `POST /api/uploads` | Store one attached file; returns the id a call names it by |
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

The palette is the logo and nothing else: near-black ink on an all-but-white
sheet — warmed a couple of degrees towards the cream it is inverted to in the
dark. `ui/src/index.css` holds the whole of it — a brand scale sampled from the
mark, and above it the roles a component names (`paper`, `panel`, `line`, `ink`,
`muted`, `brand`, and the three verdict tones).
The stock Tailwind palette is switched off there, so a stray `text-stone-400`
compiles to no utility at all rather than to a colour that nearly fits. The roles
follow `prefers-color-scheme` on their own, which is why no component carries a
`dark:` variant. The mark itself is drawn in `ui/src/components/Mark.tsx` rather
than shipped as an image: it inherits `currentColor`, so one tag serves both
schemes.

Markdown is the one place the front end takes a real dependency:
`ui/src/components/Markdown.tsx` wraps `react-markdown` and `remark-gfm`, and
maps every tag to the palette above rather than pulling in a typography plugin.
It builds React elements instead of an HTML string, which is why nothing in this
tool ever hands `dangerouslySetInnerHTML` a document written by the thing under
test. It costs about 48 kB gzipped in the embedded bundle — a hand-rolled parser
would cost less and be wrong about a corner of CommonMark nobody would find until
a model landed on it.

Outbound HTTP is covered by `wiremock` on every failure path that matters: an
expected `401`, a timeout, a malformed body, an empty body, a decode path that
misses, and a cross-host redirect (where the `Authorization` header must not
follow). OIDC runs against a mock identity provider: discovery, caching, expiry,
a rejected cached token refreshed and replayed exactly once, a rotated service
account token, and a failed exchange. Several tests fail if a credential appears
anywhere in an API response — including the case where the endpoint or the IdP
quotes it back at us in an error message.

### CI and releases

`pre-commit` **is** the lint gate. The `quality` workflow installs the tools the
hooks expect and runs `pre-commit run --all-files` — nothing is linted in CI
that a local run does not lint, and nothing is linted twice. The hooks stop
short of the test suites: `cargo test` and `vitest` are two jobs of their own in
the same workflow, so a commit stays fast and the suites still gate every push
and every pull request. A fourth job runs Trivy over the repository. Whatever
is deliberately waived lives in `.trivyignore.yaml`, in one place, with the
reasoning attached.

Tagging `vX.Y.Z` releases. The tag must match the `version` in `Cargo.toml` — the
release fails on the mismatch rather than publishing a binary that lies about
which release it is. `scripts/release.sh 0.4.0` is what keeps the two in step: it
refuses a dirty tree, a branch other than `main`, a branch out of sync with
`origin`, and a tag that already exists; then it bumps the manifest, runs the
tests, commits, tags and — after one confirmation — pushes. `--no-push` stops
before the push, `--skip-tests` before the suite. What comes out:

* `ghcr.io/therealm-tech/mire:X.Y.Z` and `:latest`, a multi-arch manifest over
  `linux/amd64` and `linux/arm64`, each built on its own native runner. No QEMU.
* `mire-X.Y.Z-linux-x86_64.tar.gz` and `mire-X.Y.Z-linux-aarch64.tar.gz` on the
  GitHub Release — the binary and `LICENSE`, nothing else — each with its
  `.sha256` published next to it rather than packed inside.

The binaries are **taken out of the images**, not built a second time from the
same source: what you download is what was scanned and published. They are
statically linked against musl, so there is no glibc version to match against
whatever the notebook happens to be running.
