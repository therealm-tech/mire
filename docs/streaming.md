# Streaming, and the number everybody actually wants

A non-streamed call answers one question about latency: how long the whole thing
took. That number is dominated by how much the model chose to say. The one worth
having is **time to first token** — how long before it started answering — and
you cannot measure it without reading the response in pieces.

Two lines make a model streamable:

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

`stream` comes from the call, not from the file, so one model serves both
shapes: the **stream** box next to **Send** is what asks for chunks rather than a
whole answer, and it asks it of the run whatever **max turns** says — one turn or
the whole loop. It is off by default, because streaming is a second thing for an
endpoint to get right and a first run should fail for one reason at a time. Tick
it when time to first token is the number you came for. Keep the `| tojson`.
MiniJinja renders a bare boolean as
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
$ curl -N -X POST localhost:8787/api/call/stream -d '{"model":"qwen3","prompt":"…"}'
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
qwen3, on the [dev stack](dev-stack.md):

```
ttftMs 8426    latencyMs 31271    sse   11 chunks, 10 with text, ended cleanly
```

Eight seconds to the first token, thirty-one to the last. One number is about the
endpoint, the other is mostly about how chatty the model felt.

## What the framing tells you

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

## Streaming a loop, and what it costs

The **stream** box says nothing about how many turns there are, so a loop reads
it exactly as a single turn does: tick it and every turn arrives chunk by chunk. `POST /api/agent` then emits one `delta`
event per chunk, each naming the turn it belongs to, before that turn's own
`turn` event — which still carries the whole exchange, so a client can ignore
every delta and lose nothing.

```console
$ curl -N -X POST localhost:8787/api/agent \
    -d '{"model":"qwen3","prompt":"weather in Lyon?","stream":true}'
event: delta
data: {"event":"delta","turn":1,"text":"Let me"}
…
event: turn
data: {"event":"turn","index":1,…}
```

**One thing to know before you tick it.** Tool calls are not reassembled from a
stream. `mire` decodes a streamed answer from its *last* chunk, and OpenAI splits
a call's arguments across chunks — stitching those back together is guesswork
this tool would rather not do on your behalf. So against such an endpoint a turn
that really did ask for a tool comes back looking like a turn that asked for
nothing, and the loop stops on `noToolCalls` at turn one. That is the endpoint's
behaviour made visible rather than a setting to fix, and it is why the box is off
by default: **untick it to test tool calling**, whatever **max turns** says. An endpoint that puts the whole
call in its final chunk streams and loops perfectly well, which is precisely the
sort of difference between two backends this tool exists to surface.
