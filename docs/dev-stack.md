# A stack to point it at

`docker-compose.yaml` brings up everything needed to exercise `mire` for real:
a model runtime, an identity provider, and a door in front of the model so that
"is this route actually protected?" has an answer.

```sh
docker compose up -d          # first run pulls ~950 MB of models
export OIDC_CLIENT_SECRET=mire-dev-secret
export MODEL_TOKEN=anything   # the gateway only checks that a credential exists
mire
```

The models in `./config/models` — under the default configuration directory —
point at that stack, so there is nothing to pass.

| Service | Port | What it is |
| --- | --- | --- |
| `ollama` | 11434 | The models. Unauthenticated, on purpose |
| `gateway` | 11435 | nginx in front of Ollama, rejecting requests with no credential |
| `whisper` | 9000 | `speaches`, so `request.multipart:` has an endpoint that takes a form rather than a JSON document |
| `mcp` | 11436 | A minimal MCP server on `2026-07-28`, so the loop has something real to call — plus `POST /policy` and `POST /audit`, the two plain HTTP routes a hook talks to |
| `keycloak` | 8080 | Realm `mire`: `mire-workload` (service account) and `mire-ui` (browser login, user `mire` / `mire`) |

Models, as of August 2026 — these rankings move monthly, so revisit the choice:

- **`qwen3:0.6b-q4_K_M`** (523 MB, 40K context). The smallest tag that still does
  tool calling, which the loop will need. `gemma3:270m` is lighter (~292 MB)
  but cannot call tools.
- **`nomic-embed-text:v1.5`** (274 MB, 768 dimensions, MTEB 62.4). The most
  pulled embedding model in the Ollama library, and it runs on a CPU.
  `qwen3-embedding:0.6b` scores higher (70.7 on MTEB-eng-v2) at 639 MB.
- **`Systran/faster-whisper-base`** (~145 MB), served by `whisper` rather than by
  Ollama. Faster than real time on a laptop CPU at `int8`. `-tiny` is a third of
  the size and audibly worse; `-small` is better and four times the download.

Four models come with it:

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

- **`qwen3-staged`** — the same weights, reached two ways, from one file. Its two
  [stages](configuration.md#stages) are the two halves of the matrix above at
  once: `qwen3-staged@ollama` goes straight at Ollama's own API anonymously, and
  `qwen3-staged@gateway` takes the OpenAI-compatible route through nginx as the
  workload identity. One `decode:` cascade covers both, and which of its paths
  won is in the trace — the first for one stage, the second for the other.

- **`nomic`** — embeddings, straight at Ollama with no credential, and with
  `$.data[*].embedding` deliberately kept first in the cascade so you can watch
  it miss and `$.embeddings` take over. No `auth:`, so it calls as `anonymous`
  and the panel says as much.

- **`whisper`** — the odd one out, and the reason `whisper` is in the stack at
  all: an endpoint that does not read a JSON document. The audio goes out as a
  form part and the knobs beside it, which is [a request built a third
  way](models.md#when-the-endpoint-takes-a-form-not-json). Attach an audio file, press
  **Send**, and read the parts in **Traffic**. The pyannote variant is written
  out in a comment beside it.

  Say something into a file and try it without leaving the terminal — on a Mac,
  `say` will do:

  ```sh
  say -o /tmp/speech.wav --data-format=LEI16@16000 \
    "Mire is a test pattern for model endpoints."
  ```

The three are commented with the edit that turns them into the next
experiment — `qwen3` in particular is two lines away from Ollama's *own* chat API, where the
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
