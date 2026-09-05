# Writing a model

One YAML file per endpoint, in `models/`. Take the `curl` you are
pasting around today and split it into three parts: the URL, the body, the
credential.

Say this is what you run now:

```sh
curl -X POST https://models.internal/mistral-small/v1/chat/completions \
  -H "Authorization: Bearer $MODEL_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model": "mistral-small", "messages": [{"role": "user", "content": "ping"}]}'
```

The model is:

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
## Teach it a non-standard endpoint

Not every endpoint answers like OpenAI. Each `decode:` field is a **cascade**:
paths are tried in order and the first one that resolves wins, so one model can
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
it is, because that is a model with a blind spot.

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

That is the fast way to fix a model: look at the raw tree, pick the right path,
edit the file. See [`config/models/qwen3.yaml`](../config/models/qwen3.yaml) for a
worked example — one `decode:` block covering two unrelated response shapes, of
which only the first wins until you point the model at the other endpoint.

A model with no `decode:` block at all is valid — that is the normal state of
an endpoint you have not figured out yet.

### When a cascade is not enough

Some things a path cannot do: strip a `<think>` block out of the content, join
segments conditionally, compute anything. For those, and only for those, a
model can carry a Rhai script instead — on the request side, the response side,
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

A request script sees `messages`, `input`, `tools`, `model_id` and `params`, and
returns the body — a string used verbatim, or a map or array that gets serialised
for you. A decode script sees `raw`, `status` and `headers`, and returns a map:
`content` / `tool_calls` / `finish_reason` / `usage` / `error` for a chat model,
`vectors` / `usage` / `error` for an embedding one — `vectors` being a list of
vectors, or a list of *lists* of vectors for a multi-vector endpoint. `error` is read like the
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
when the alternative is not supporting the endpoint at all. `template`, `script`
and `multipart` are mutually exclusive, and so are `decode` paths and
`decode.script` — a model declaring two fails to load, so there is no
precedence rule to remember. A request is one body.

Scripts are compiled when the model loads, so a syntax error names the file at
startup. At call time they are bounded: 500k operations, a one-second deadline,
caps on string, array and map sizes, and `eval` disabled. Rhai has no file,
network or process access to begin with — there is nothing to take away, and a
test asserts it stays that way. A decode script that fails is *not* fatal: its
message lands in the decode trace next to the raw response, exactly like a path
that missed.

Neither shipped chat model uses one, on purpose: they are meant to be read, and
a script is what you add once a cascade has already failed you. The snippet above
is the real motivating case — qwen3 emits its reasoning inside a
`<think>…</think>` block, and no path can strip a prefix.

### When the endpoint takes a form, not JSON

Whisper does not read a JSON document. Neither does pyannote, or any of the
diarisers, classifiers and OCR services shaped like them: they take a
`multipart/form-data` — the bytes in one part, a handful of knobs as ordinary
form fields beside them. That is the third way to build a body:

```yaml
request:
  multipart:
    file:
      upload: '{{ uploads[0] }}'
    model: whisper-1
    response_format: json
    temperature: 0
    prompt: '{{ messages[-1].content }}'
```

**A bare value is a text field**, and it is a template like any other — the same
`messages`, `input`, `tools`, `model_id`, `params` and `uploads` a `template:`
sees. Write a number or a boolean as itself if that is how the knob reads; a form
sends text either way.

**`upload:` is what makes a field a file.** It names uploads of the call: the
object whole (`{{ uploads[0] }}`), the list of them (`{{ uploads }}`), or a
string matching a file's `path`, `name` or `id`. Several files under one field go
out as several parts under that name, which is what every server-side upload
handler already reads.

The two are never guessed at from the rendered value. A `model: whisper-1` that
turned into a file part because something in the upload directory happened to be
called `whisper-1` is exactly the surprise this tool exists not to have.

Both kinds take an optional `type:`, and a file part also takes `filename:`:

| Key | On | What it does |
| --- | --- | --- |
| `text` | text part | The template. The same thing a bare value is |
| `upload` | file part | One template naming uploads, or a list of them |
| `type` | either | The part's `content-type`, overriding the extension's guess |
| `filename` | file part | The part's `filename`, overriding the stored name |

Both overrides exist because both are guesses worth overriding. The media type
comes from the extension — `.mp3` is `audio/mpeg`, `.wav` is `audio/wav`, and
`application/octet-stream` when the extension said nothing — and some endpoints
validate the name's extension before they look at a single byte. A `filename:` on
a field carrying more than one file is refused: two parts under one name is a
form nobody meant.

`type:` on a *text* part is what a diariser usually wants, for the options blob
that travels beside the audio:

```yaml
request:
  multipart:
    file:
      upload: '{{ uploads[0] }}'
    config:
      text: '{{ params.config | tojson }}'
      type: application/json
```

**Fields go out in the order the file writes them**, not in alphabetical order.
Most parsers do not care, right up to the one that does.

**A field naming a file nobody attached fails the call**, before anything leaves:

```json
{
  "code": "multipart_error",
  "message": "request.multipart: `file` names a file, and nothing was attached to this call"
}
```

That refusal is the whole point of the shape. A form missing the one part the
endpoint asked for goes out looking perfectly well-formed and comes back a `422`
about a field nobody in the model ever mentioned, which costs an afternoon.

It fires while the form is being built, though, which is after **Send**. Add
`requires_upload: true` beside `kind:` and the same model refuses earlier and
in the UI as well — the button greys rather than producing a failure to read. See
[When the model cannot work without
one](ui.md#when-the-model-cannot-work-without-one).

You never write the `content-type`: the encoder settles it at send time, boundary
included, and a `headers.content-type` in the model is dropped with a warning
rather than sent beside the real one.

The rest is unchanged. `auth:` still puts the credential where the provider says,
the `401` replay still works, and a transcript still decodes like any other
completion — `kind: chat` plus a one-line cascade, because a transcript *is* text
in an answer:

```yaml
decode:
  content:
    - $.text
```

**Traffic** shows a form as its parts rather than as a body, since a form is not
text: the field each part went out under, the value for a text field, and the
name, type and size for a file — never its bytes. **Copy as curl** gives you `-F`
flags with the file by its path on disk, so the command actually runs:

```sh
curl -sS -X POST \
  'http://127.0.0.1:9000/v1/audio/transcriptions' \
  -F 'file=@./uploads/jcnp3-qNY8cJ-speech.wav;type=audio/wav;filename=speech.wav' \
  -F 'model=Systran/faster-whisper-base' \
  -F 'response_format=json' \
  -F 'temperature=0' \
  -F 'prompt=mire, endpoints'
```

[`config/models/whisper.yaml`](../config/models/whisper.yaml) is the worked
example, and `docker compose up -d` serves something for it to talk to — see
[A stack to point it at](dev-stack.md). The pyannote variant is written out in a comment
beside it.

### A model with nothing to type

The composer holds **Send** back until there is something in the box, because a
request nobody can see the input of is the one thing this tool exists not to
send. On a transcriber that rule is backwards: the input is the audio, and the
box is a place to write a question the endpoint is never asked.

So a model can say so:

```yaml
name: whisper
kind: chat
has_prompt: false
url: http://127.0.0.1:9000/v1/audio/transcriptions
```

`true` is the default and every other model takes it. `false` does two things
and no more: the message box and the saved-prompt picker go away, and **Send**
goes out with an empty conversation instead of waiting. **Attach**, **stream**,
**max turns**, the servers, the traffic and the `401` replay are all unchanged —
this is a statement about the composer, not about the wire.

"Instead of waiting" is about the box only. A model also declaring
[`requires_upload: true`](ui.md#when-the-model-cannot-work-without-one) still waits
— for the file, which on an endpoint like this one is the whole of the input.

Nothing else is special-cased. A template on such a model still renders, now
against an empty `messages`, which is the same rule `uploads` and `stream`
follow: what goes out is what the request says goes out.

`config/models/whisper.yaml` declares it, and moves whisper's `prompt` field —
which is a vocabulary hint rather than an instruction — to a knob, since that is
what it always was:

```yaml
    prompt: '{{ params.prompt | default("") }}'
```

```sh
curl -sS localhost:8787/api/call -H 'content-type: application/json' \
  -d '{"model": "whisper", "uploads": ["<id>"],
       "params": {"prompt": "mire, Keycloak, speaches"}}'
```

Put `{{ messages[-1].content }}` back and drop the `has_prompt: false` if you
would rather type it.
