# The loop

The loop is not a third payload format and not a second model. It is the same
`kind: chat` model, run round: render, call, decode; if the stop condition is
not met, answer the tool calls with their simulated results, feed them back, go
round again. `POST /api/call` runs one turn of exactly the same thing.

Which is why the UI has no mode to pick. **Send** is the loop, whatever the
model declares, and **max turns** says how far it may go — at **1**, one turn
of the very same thing, which is one call and one answer. Unticked, which is
where it starts, the run is bounded by the model's own `agent.default_max_turns`
rather than by anything the browser remembered. A model that declares no tool
stops on turn one anyway. So the only question the composer asks about
turns is *how many*, never *which mechanism*: there has only ever been one. How
the answer *arrives* is the **stream** box, which is a
[separate question](streaming.md) with its
own answer, asked whatever the count.

The servers are not part of that count. A server switched on is set up for a chat
model's run whether it has one turn or twenty — one turn against a real server
is a fair question, since "does the model ask for the tool `tools/list` showed
it?" is answerable without ever answering the call. Which servers a run reaches
is [its own question](mcp.md#choosing-what-a-run-reaches), asked in the **MCP
servers** block and starting at none: a run with none leaves the model offered
its own `tools:` and nothing else, which answers the question that comes after —
what does the loop do when the tool it wants is not there?

The single-shot routes stay where they are useful, on the API rather than behind
a button. `POST /api/call` and `POST /api/call/stream` never discover, list or
call a tool, whatever `mcp/` declares — which is exactly what you want from
a `curl` that is asking one question about one request.

It runs on the [conversation](ui.md#having-a-conversation) in the browser, and when it
stops it appends the answer it finished on. The turns in between — the tool calls
and their results — stay out of the history: they are what the run was about, and
replaying them into the next request without their results is how you get a `400`
from an endpoint that was working fine. They are not lost, though. Each one lands
in the transcript where it happened and in full in
[**Traffic**](ui.md#reading-the-traffic), request, decode and response, next to the
model call that asked for it.

```yaml
decode:
  from: [openai-chat]            # brings `terminal_reasons: [stop, length, …]`
agent:
  stop_when:
    no_tool_calls: true          # the default, and almost always what you want
    repeated_call: true          # off by default: stop on the same call twice
  default_max_turns: 6
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

The third predicate is not in that block. Stopping on the model's stop reason
needs the list of values that mean *finished*, and that list is a property of the
endpoint's vocabulary rather than of the run — OpenAI says `tool_calls` for a turn
still working and `stop` for one that is done, Gemini says `STOP` for both. So it
travels with the shape that defines it, as
[`decode.terminal_reasons`](models.md#which-stop-reasons-mean-done), and naming a
built-in decode is all it takes to get it right.

These tools capture nothing: `capture:` is declared on an MCP server, and a
simulated tool belongs to none. See [keeping something a tool call
answered](mcp.md#keeping-something-a-tool-call-answered).

**Nothing is executed.** The tools are simulated — a fixed string, or a Rhai
script that sees `arguments`, `name` and `turn`. What is being checked is that
the model emits calls matching the schema it was given, and knows what to do with
a result. Arguments are validated against that schema and the mismatches are
reported; the model still gets an answer, so it has a chance to correct itself. A
tool the model never declared gets an error back rather than silence.

`POST /api/agent` streams server-sent events: one `setup` event if the model
has MCP servers, a `turn` event per turn as it happens, then one `done` carrying
the whole trace. Send `"stream": true` and each turn is preceded by one `delta`
event per chunk it was written in, each naming its turn — see [streaming a
loop](streaming.md#streaming-a-loop-and-what-it-costs), including the reason it
is off by default. Each turn holds the rendered request, the masked headers, the
`curl` equivalent, the raw response, the decode trace and the tool results — the
same shape `POST /api/call` returns — plus `mcp`, every JSON-RPC round trip that
turn made, request and response, credentials already masked, and `hooks`,
everything that fired around those tool calls.

`setup` carries the same shape for what happened before the loop: discovery, the
handshake, `tools/list`. It arrives first because it happened first, and a run
that dies negotiating never reaches a turn to report it on. `done` repeats it
under `setup`, so a client that only reads the trace still has it.

## Every way out is named

There is no silent loop. The one worth spelling out:

```json
{"outcome": "predicateNeverEvaluable", "predicate": "decode.terminal_reasons", "turns": 3}
```

A model that stops only on `finish_reason`, pointed at an endpoint that never
reports one, would otherwise run to `default_max_turns` and look like a slow
agent. It is not — the condition could never be evaluated once, and that is what
gets reported. The others are `stopped` (a predicate held), `maxTurns`,
`deadline`, and `repeatedCall` — the model asking for the same tool with the same
arguments twice, which is a loop rather than progress. That last one is opt-in
(`stop_when.repeated_call`): re-reading a tool it already called is often a model
working rather than spinning, and `default_max_turns` bounds the run either way.

Try it against the [dev stack](dev-stack.md): `qwen3` fetches `get_weather` from
the `weather` MCP server and really calls it. On a CPU-only Ollama a two-turn run takes about
ninety seconds.
