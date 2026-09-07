# The UI

Deliberately small. It does not edit anything — the models are yours and your
editor's — and it holds no logic of its own: it shows what the API returns.

- **What the next call will do**, above the box you would make it from. Where it
  goes and who it goes as — the "known signal in" half, said before it happens
  rather than reconstructed from a trace afterwards. It says nothing about whether the endpoint is up. That is the
  question you came to ask, and answering it here would be answering it by
  guessing.

  Under the address, **one line per thing there is to say about this call**,
  always the same four parts: a badge, what the line is about, what there is to
  say, and — where there is one — the button that fixes it. **The badge is the
  sentence**, and what follows it is only ever what a badge cannot hold: a name,
  a path, a host, a countdown. The badge carries the verdict, so an identity that is signed in and one that is not are the same line
  in the same place, and watching one become the other moves nothing on the page.
  Every red badge is a refusal `mire` is already known to make: an identity or a
  server no file declares, a credential outside its `allowed_hosts` or missing
  from this tab, a browser session nobody has fetched. There is no verdict over
  the top of them — the line that is red is the answer, and it says which of them
  it is about, which a word above the address never did.

  What the request *carries* is not here. A `requires_upload:` model with nothing
  attached is refused just as squarely, but the file that fixes it is picked two
  buttons along on the composer, so that is where it is answered — **Send** stays
  shut, and says why, until **Upload files** has been pressed.

  **Send waits for one of these lines, and only one**: an identity with nobody
  signed in to it, the model's own or one a server in this run needs. That call
  never reaches the endpoint — `mire` answers `409 not_signed_in` itself and puts
  nothing on the wire — so the click buys the sentence already on the page, next
  to the button that fetches the session. Every other red line leaves **Send**
  alone on purpose: a credential the endpoint refuses is the endpoint's verdict,
  and collecting it is the reason you came.
- **The identity, on one of those lines.** It is the model's, declared in its
  `auth:` next to the URL it authenticates against, so the line shows it rather
  than offering alternatives — what you read in the file is what went out, and
  the UI never puts an `auth` of its own on the wire. To ask the same endpoint as
  somebody else, copy the model and change one line; that copy is a thing you can
  name, keep and re-run, which a click never was.

  The badge says which of the nine states it is in, and most of them stop there.
  `anonymous` sends nothing at all, which is what makes a `401` from it a
  *passing* result rather than a failure — and it carries a word only when the
  model declares no `auth:`, because an identity nobody chose and one somebody
  did are worth telling apart. `token` and `workload` carry the `env:` or the
  `file:` the server reads on every call: an empty variable and a misspelt
  variable name are the same `401` until one of them is named, and the name is
  not the value. `out of allowed_hosts` carries the hosts it may be sent to,
  against the URL a line above. Those two and `undeclared` are repaired in a file
  rather than here, so they carry no button.

  The rest is what no file could hold, and it is asked for on the line that would
  otherwise only report it: `no value` carries the box to paste a credential into
  — it stays there once something is typed, and the badge turns to `in this tab`
  — while `not signed in` carries **Sign in**, and `signed in` carries who you
  are, a countdown and the granted scopes, with the way back out. A sign-in that
  failed carries the reason, and a second button that forces the identity
  provider to stop reusing its own session and ask.

  The identity a *server* authenticates with is not here. It is a separate
  question answered in a separate file — the model's comes from the model, a
  server's from `mcp/`, and neither follows the other — so it is asked on the
  card of the server it is about, in the block below. The browser identities that
  run's servers need are still here, because it is this run that would fail
  without them: **one line per identity**, in the same three states and with the
  same buttons, folded into the model's own line when it is the same identity.
  Which server wanted it is not said — it is the same identity either way, and
  three servers and a model wanting one missing session is one line and one
  button, because that is what it takes to fix all four. The line stays once
  somebody has signed in, saying who: an answer you can only have while it is
  still wrong is not much of an answer.
- **Conversation**, for chat models. A transcript: your question on the right,
  the answer on the left, the tools the run called in between, and a composer at
  the bottom. `Enter` sends, `Shift`+`Enter` starts a line. There is one button,
  **Send**, and it runs the [loop](agent-loop.md) — it answers the tool calls the
  model makes until it stops making them, and a model with no tools ends on
  turn one anyway. Two controls next to it say how far it goes and how the answer
  arrives. **max turns** is the budget, and it is also the whole of the
  "one turn or several" question: at **1** the run sends a turn and stops, which
  is the single call and the single answer, with a tool call — if the model makes
  one — coming back unanswered and flagged. Anything above it is a loop with room
  to finish. It is **off by default**, and off it is not a budget of zero: the run
  takes the model's own `agent.default_max_turns`, which is what the file says and
  what a call from anywhere else gets. Ticking it is this tab overriding that for
  this run. The number stays on screen either way, greyed while the model's own
  budget stands, and it is remembered — unticking the cap does not throw away the
  number somebody settled on. A **stream** box is the other question: ticked, the answer is read
  chunk by chunk, the text appearing as it is written and the only way to see
  time to first token. The two are independent — a streamed loop and a
  whole-bodied single turn are both a tick away — and the box is **off by
  default**, at every turn count. More on what that costs a loop in
  [streaming](streaming.md).
  While a run is in flight, **Stop** drops the request where it stands and
  whatever had arrived stays on the page — a stream cut off after four tokens
  produced four tokens, and that is a finding rather than a mess to clear up.
  Nothing is sent upstream to call the work off: an endpoint that has been asked
  a question is going to answer it, so this is about your tab and says only that.
  More on what the transcript is [below](#having-a-conversation).
- **Saved**, a dropdown above either box. The prompts in `prompts/`,
  picked by name and dropped in the box — nothing is sent, and what the text
  becomes on the wire is still the model's template's decision. More
  [below](#saving-a-prompt).
- **MCP servers**, opened from **MCP** on the bar, for a chat model — an
  embedding one has no loop for a tool call to be part of, so the button goes
  with the block. One card per file in `mcp/`, carrying the whole of what this
  run does with that server: a switch for whether it is in the run at all, the
  address it would reach, the identity it would go out as (or `anonymous`, and
  whether it comes from a header template rather than `auth:`), and the
  **Sign in** that identity needs when it is a browser provider nobody has
  fetched a session for — that call answers `409 not_signed_in` and sends
  nothing, so the button is on the card rather than somewhere else. Once somebody
  has been through, the card says who, and carries the **Sign out** that drops
  that identity again.

  **Every switch starts off**, and that is the point: a tool call really runs, on
  somebody's real server, so a run reaches one because you said so here and never
  because a file was sitting in `mcp/`. A server left off is not set up — nothing
  discovered, nothing listed, no credential fetched, its tools not offered, and
  the refusal it was causing gone with it. The card stays, with its switch off and
  nothing on it left to act on — [what that
  means](mcp.md#choosing-what-a-run-reaches) is the same for every server that is
  out, so it is said once there rather than under each of them.

  A server declaring [stages](configuration.md#stages) is one card all the same,
  with a button per stage and a dot on the one a bare name means — the same shape
  the model list uses, for the same reason. Two stages are two endpoints, with
  their own address, their own session and their own negotiated revision, and a
  run takes **one** of them: what the card shows is the stage that is pressed,
  and the call carries the `name@stage` the button stands for. The revision they
  are spoken in is not asked here — it is `protocol_version:` in `mcp/`, per
  server, and a run does not get to differ from the file. More in
  [MCP](mcp.md).
- **Configuration**, opened from **Config** on the bar, beside **MCP** and for
  the same reason: the bar says where the next call goes, this says what decided
  it. Three blocks and nothing else — the request template, the decode cascades,
  and when the loop stops. No summary of the URL and the identity, because they
  are the line above.

  **What it shows is resolved rather than copied.** `${ stage.… }` is already
  substituted, so the template is the one that goes out at the stage that is
  pressed rather than the one written in the file. `decode.from` is already
  flattened, so the shapes a model is built on are a list of paths per field, in
  the order they are tried — the same order a call's decode trace names its
  winner in. More on where those paths come from in [naming a
  shape](models.md#name-a-shape-instead-of-spelling-it-out). A model that declares no `agent:`
  still gets the block, saying what the loop does without one: an absent block is
  not an absent answer.

  **YAML** hands the same model over as a file, written the way `models/` reads
  it. Saved there it loads the same endpoint back, which is also how a stage
  becomes a model of its own. It is the model's own route asked for in YAML, so
  `curl -H 'accept: application/yaml' …/api/models/qwen3 > models/qwen3.yaml` is
  that act without the tab.
- **Input**, for embedding models. One text per line and a run count — two or
  more runs is what the determinism check compares. The call always asks for the
  full vectors, so the summary the panel reads has the payload behind it. There
  is no second turn of an embedding, so there is no conversation and no loop.
- **Traffic**, under the conversation. Everything that left the process, in the
  order it left, one card per exchange, filtered by kind or down to the failures
  — and reachable from the transcript above, which names the card each of its
  rows summarises. See [below](#reading-the-traffic).
- **Models**, a column where there is room for one and a fold-away where there
  is not — on a phone the list was a screenful to scroll past before reaching the
  thing it configures. A model declaring
  [stages](configuration.md#stages) is one row all the same, with a button per
  stage under its name and a dot on the one a bare name means. Picking an
  endpoint and picking where it runs are two questions, and the row asks them in
  that order; what the row shows — the URL, the badges — is the stage that is
  pressed, because that is what **Send** would ask. Two stages are still two
  endpoints, with their own URL, their own credential and their own answer, and
  the call carries the `name@stage` the button stands for.
- **Embedding.** Count, width, encoding, the five checks, and per vector its
  norm, a sample of the first values and a distribution histogram. Never a wall
  of floats. A multi-vector answer stays grouped under the input it belongs to,
  with only the first few of each item's vectors drawn.

**The page follows the files.** Save a model, a provider, a server or a prompt in
your editor and the lists move under you a moment later, load errors included —
the tab is subscribed to the same reload the rest of the process acts on, so it
never shows the directory as it was when you opened it. A short *Configuration
reloaded* note in the corner says why something moved. The one thing that
survives the reload is what you had selected, as long as its file still declares
it: a model deleted under an open tab drops back to the first one rather than
leaving the page pointing at nothing.

A credential typed into the UI lives in that tab and nowhere else: it is sent
with the call and never stored, never logged, never echoed back. A credential
`mire` fetched for you never reaches the tab at all — the browser sees a
username, the granted scopes and a countdown.

**The tab remembers a little, and never that.** Which model you were on and the
stage you were asking it at, what you had half typed, whether and how far you cap
the turns, which MCP servers you switched on and at which stage you were asking
them — small settings whose loss is pure annoyance, kept in the browser's own
storage. The stage is remembered per model and per server, so
coming back to one comes back to where the question was rather than to its
default; a stage the file no longer declares is simply the default again. The credential is not among them, and neither is
the conversation or the traffic: a session's bodies are unbounded, and the first
oversized run would start throwing quota errors at a tool whose job is to be
dependable while other things fail. Storage that is missing or full is a browser
with no memory, never a page that fails to load. `mire` still holds nothing —
this is the same side of the wire the conversation has always been on.

## Having a conversation

A chat model keeps its turns. Send, get an answer, ask a follow-up: the
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
  ever leaves without the transcript showing what it carried — unless the model
  says there is nothing to say at all, which is what
  [`has_prompt: false`](models.md#a-model-with-nothing-to-type) is for.
- **Only the answer the run finished on rejoins the history.** The tool calls in
  between and their results stay out of it: replaying them into the next request
  without their results is how you get a `400` from an endpoint that was working
  fine. They still appear in the transcript, where they happened, as a line
  naming the tool, whether it really ran, and any variable it captured — the
  full exchange, and what that variable is worth, is in **Traffic**. A tool call
  that *does* land in the history — from a run that stopped on one, whether
  because **max turns** was reached or because it was **1** to begin with — is
  flagged on its bubble, because most endpoints refuse the next turn until it has
  a result.
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

## Attaching a file

**Upload files** writes a file to `mire`'s upload directory — `--uploads`,
`./uploads` by default — and lists what it stored. That is the whole feature, and the next
sentence is the important one.

**The file goes to the template, not to the endpoint.** The next **Send** hands
it over as `uploads`, and what happens next is the model's decision: a template
that never mentions `uploads` sends exactly what it always sent, the same way one
that never mentions `stream` never streams. That is not a limitation to work
around — it is the only arrangement in which "what did we send?" has one answer,
written down, in a file you can read.

So an attachment is an ingredient, and the model is the recipe:

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

The third recipe does not inline the file at all. An endpoint that wants a
`multipart/form-data` — a transcriber, a diariser — gets the bytes as a form
part, and the model says so with `request.multipart:` instead of a template;
see [When the endpoint takes a form, not
JSON](models.md#when-the-endpoint-takes-a-form-not-json).

Two things the fields cannot tell you, both of them the price of `mire` keeping
no state: `name` is the **sanitised** name rather than what your browser called
the file, and `contentType` is guessed from the extension rather than taken from
what the browser claimed. Neither was written to disk, and the disk is the only
thing that survives a restart. A template that knows better writes the type
itself.

Attachments are re-rendered on **every turn** of an agent loop, since the body is
built from the template each time. And a template that inlines one puts it in the
request body, so it arrives in **Traffic** at its full base64 size — a 12 MB photo
is a 16 MB request to scroll past. Upload the file you meant to test with. (A
`multipart:` model does not have this problem: the panel names its parts rather
than repeating their bytes.)

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

### When the model cannot work without one

Some models have no call in them without a file. A transcriber, a diariser, an
OCR service: the file *is* the question, and pressing **Send** with nothing
attached renders a request built around something that is not there. Say so, and
the refusal happens here rather than at the endpoint:

```yaml
name: whisper
kind: chat
url: http://127.0.0.1:9000/v1/audio/transcriptions
requires_upload: true
```

**Send** is then shut until **Upload files** has been pressed — and so is
**Retry**, which is a send like any other. The button carries the reason, so what
holds it is readable from the button itself rather than from a sentence somewhere
else on the page:

```
This model is built around a file. Upload one, and Send comes back.
```

The button is the courtesy; the rule is the server's. A call arriving at
`POST /api/call`, `/api/call/stream` or `/api/agent` with nothing attached is
refused before a body is rendered, whatever sent it:

```json
{
  "code": "upload_required",
  "message": "model `whisper` needs a file attached, and this call carries none"
}
```

It says the call must carry *a* file, not which one, and not what becomes of it.
Which upload the template reads is still the template's decision — a model that
asks for a file and then never mentions `uploads` is asking for one it throws
away, and that is visible in the same file, two lines down. This is a rule about
whether there is a call to make at all.

It is not the same check as a `multipart:` field naming a file nobody attached
— see [When the endpoint takes a form, not
JSON](models.md#when-the-endpoint-takes-a-form-not-json). That one fires while the form is
being built, and only for the models that build one; this one fires before any
of that, for a `template:` and a `script:` too, and is what greys the button.

It pairs with [`has_prompt: false`](models.md#a-model-with-nothing-to-type), which is
what `config/models/whisper.yaml` declares: no box to type in, and the file the
only thing left holding **Send** back. The two are independent — a vision
endpoint asking a question *about* an attachment wants a box and a required file
both — but on the models whose input is bytes they travel together.

## Saving a prompt

A model says how to reach an endpoint. A prompt says what to send it, and that
half is worth keeping for the same reason the first one is — the question that
makes it call the tool, the one that makes it refuse, the paragraph that
reproduces the bug. Retyping those from memory is how a comparison quietly stops
being one.

They live one per file in `prompts/`, next to `models/`, `auth/` and `mcp/`:

```yaml
# prompts/01-ping.yaml
---
name: ping
text: ping
```

```yaml
# prompts/05-strict-json.yaml
---
name: strict json
text: |
  Answer with a JSON object and nothing else — no prose, no fences.
  Keys: "city" (string), "temperature_c" (number), "measured_at" (RFC 3339).
```

A name and its text. That is the whole shape, and the omissions are the design.
The file names carry the order the dropdown lists them in, which is why they are
numbered — a library is a list somebody arranged:

**Read-only, like everything else in this directory.** The file is the source of
truth, your editor writes it, the watcher picks the change up without a restart.
So a prompt is a thing you can commit, review and hand to somebody else — which
a button that wrote into a config directory would not have been, and which is
also why the container can still run `--read-only`. A bad entry is reported in
the UI and skipped; the rest still work, the same policy every other file here
gets.

**A prompt says nothing about where it goes.** No model, no kind, no `auth:`.
Picking one fills the box and stops there — nothing is sent, and what the text
becomes on the wire is still the model's template's decision. That is what
lets the same question be replayed against every endpoint in the directory,
which is the comparison you came here to make. The same library is offered to
an embedding model's **Input** box, where one saved text can be several: the
box is one text per line and a multi-line `text:` arrives whole.

**Picking replaces what is in the box.** It is the honest reading of "load the
saved one", and the dropdown says so. It returns to `pick one…` afterwards, so
reaching for the same prompt a second time works rather than looking like a
control that stopped responding.

The whole library is `GET /api/prompts`, in the order the file writes them — a
list somebody arranged is not re-sorted on the way out.

## Reading the traffic

Under the conversation, **Traffic** is every wire this process touched, in the
order it touched them. **Cards land folded**, and the folded list is a grid:
every row puts the same thing in the same column, so fifty exchanges are read by
running down one column rather than by reading fifty headlines.

**A card goes up when the request leaves, not when the answer lands.** A model
call sits there with `···` in the status column while the endpoint is thinking,
and its left half — the body, the headers, *Copy as curl* — is complete and
readable the whole time: what was asked is settled long before what was
answered. Each tool, each JSON-RPC round trip and each hook appears the moment it
lands, rather than the whole turn arriving at once when the last of them comes
back. A call the run walked away from — a stop, a failure, a closed tab — keeps
its card and reads `none`, which is a different statement from a call that is
still out.

| | | | | | | |
| --- | --- | --- | --- | --- | --- | --- |
| kind | what was asked | who was asked, and where | turn | status | received | took |
| `mcp` | `server/discover` | weather · 127.0.0.1:11436/mcp | setup | 200 | 892 B | 2 ms |
| `model` | `/v1/chat/completions` | gpt-oss-20b · api.groq.com | T1 | 200 | 18.2 kB | 1240 ms |
| `model` | `/v1/chat/completions` | gpt-oss-20b · api.groq.com | T2 | ··· | | |
| `hook` | `gate` | before get_weather · vault.internal/v1/sign | T1 | 403 | 96 B | 61 ms |

The bar beside the duration is drawn against the slowest call in view, so the one
that did the waiting is picked out without reading a single number. A status
nobody answered with is not shown as one: a request that never reached anybody
leaves the column empty and says **never answered** in words. The few things
worth knowing before opening anything — *simulated, nothing executed*, *stopped
the call*, *first token 310 ms*, *error in the body* — sit beside the address.

**Open one and it is request on the left, response on the right**, in the same
three strata on both sides — the start line, the headers, the body — because the
thing you are checking is then in the same place on both, and comparing the two
halves is a glance rather than a scroll. Underneath, full width, whatever belongs
to neither wire:

| | Model call | MCP round trip | Tool call | Hook |
| --- | --- | --- | --- | --- |
| **Request** | Method, path, masked headers, body, *Copy as curl* | The JSON-RPC that went out, with its headers and the revision | The arguments the model produced | Method, the address it rendered, headers, body or attached files |
| **Response** | Status, latency, response headers, the decoded answer, the body, stream counters | Status, latency, response headers, the JSON-RPC that came back | Status, latency, what the tool handed back | Status, latency, response headers, what the gate answered |
| **Underneath** | — | — | The schema check, and every variable the answering server's [`capture:`](mcp.md#keeping-something-a-tool-call-answered) pulled out — only when a rule matched | What the failure cost the tool call |

**Both sets of headers are here, folded.** They cost one line each until you open
them, with the count and the first few names on the fold; credentials arrive
already masked as `***`. The response's are the half a request-only record cannot
answer — a rate limit, `retry-after`, the `Mcp-Session-Id` a server issues on the
handshake and nowhere else, the request id an operator is going to ask you for.

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
[sat a call out](mcp.md#a-hook-that-waits-for-one), which sent nothing and broke
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

**Every body is a foldable tree**, in both directions and on every card, because
finding where an endpoint hid a field is the job and a wall of text is the one
shape that does not help with it. **A branch is named by its key**, with a
glimpse of what is inside it while it is folded — `messages · 2  [{ role,
content }, …]` — so you can tell which branch you want without opening all of
them. An empty value is said in place rather than hidden behind a toggle that
reveals nothing. The same bytes are also a piece of text and, for a request, a
command you can paste: the tabs over the body are the readings of it, and a
response that came back `400` opens on the body rather than on the decoder's
empty hands. A body that is not JSON is shown as itself — an HTML error page from
a gateway is a finding, and prettifying it would hide the finding.

**The JSON-RPC envelope is lifted off the payload.** `jsonrpc`, `id` and `method`
are three of the four fields of every MCP message ever sent; on a line of their
own they cost nothing, and the tree opens on `params` — or on `result`, which is
the answer somebody came to read.

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
the server that answered, the status it answered with and how long it took, and
one that did not is marked **simulated, nothing executed** (see [the
loop](agent-loop.md)). A plausible-looking result from a tool nothing wired up is
the easiest way to believe an integration works.

**The status is on the tool card, not only on the round trip beside it.** A
`tools/call` that came back `401` is a tool call that failed for a reason the
words "tool failed" do not carry, and reading it used to mean finding the
protocol card underneath. It reads like the others now — the status column of the folded
row, `never answered` when the request never reached anybody at all, and nothing
at all when nothing was sent, which is what a simulated tool and a call [a gate
refused](mcp.md#hooks-something-that-happens-around-a-tool-call) have in common.

The list **accumulates across the whole conversation** rather than resetting on
every send, because "it worked on turn one and not on turn four" is a comparison
and a panel showing only the latest turn cannot make one. *Expand all* opens
everything it holds and *Collapse all* folds it back; *Clear* empties it, on
purpose, when you want a clean read.
