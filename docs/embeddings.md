# Embeddings

An embedding model takes `input` — a string or a list of strings — instead of
`messages`, and the answer is judged on its *shape*:

```sh
curl -s localhost:8787/api/call \
  -H 'content-type: application/json' \
  -d '{"model": "nomic", "input": ["one", "two"], "repeat": 2}' | jq .response.decoded
```

```json
{
  "kind": "embedding",
  "count": 2,
  "vectorCount": 2,
  "vectorsPerItem": [1, 1],
  "dimensions": {"kind": "uniform", "value": 768},
  "encoding": "float",
  "vectors": [{"index": 0, "item": 0, "position": 0, "dimensions": 768, "norm": 1.0, "sample": [...], "finite": true, "histogram": {...}}],
  "checks": {
    "count": {"status": "pass"},
    "finite": {"status": "pass"},
    "nonZeroNorm": {"status": "pass"},
    "determinism": {"status": "pass"}
  }
}
```

`count` and `dimensions` are **read off the vectors that came back**, never out
of what the response claims about itself. Inconsistent widths surface as
`{"kind": "ragged", "values": [...]}` rather than being averaged away.

`count` is one per **input**, not one per vector: see
[multi-vector answers](#multi-vector-answers) below.

`repeat: 2` sends the request twice and compares: the same input must give the
same vectors, within `tolerance` (default `1e-6`). This is the check that catches
a replica quietly serving a different model from its siblings — everything else
about its answer looks perfectly fine. Without `repeat`, the check reports
`skipped` and says so, rather than passing by default.

Three response shapes decode without a script: one node per item
(`$.data[*].embedding`), one node holding the whole batch (`$.embeddings`), and a
bare vector at the root. Base64 payloads — what `encoding_format: base64`
produces — are decoded as little-endian `f32` and counted like any other.

## Multi-vector answers

A late-interaction model, or any server with pooling turned off (`pooling: none`
and friends), answers **one vector per token** rather than one per input:
`data[0].embedding` is then a list of 1024-wide vectors, not a vector. That
decodes without a script too, and stays grouped by input:

```json
{
  "count": 2,
  "vectorCount": 17,
  "vectorsPerItem": [11, 6],
  "dimensions": {"kind": "uniform", "value": 1024},
  "vectors": [{"index": 0, "item": 0, "position": 0, "dimensions": 1024, ...}]
}
```

`count` stays the number of **items** — one per input, which is what the `count`
check compares against — and `vectorCount` is how many vectors that came to.
`vectorsPerItem` is the shape itself, and it is also what the `full` payload, a
flat list, is regrouped by.

One shape is genuinely ambiguous: a single node holding a flat list of vectors is
a *batch* of pooled vectors under `$.embeddings`, and byte for byte the same JSON
is one input's token vectors. The number of inputs sent settles it — one input
means they are all its own — and when it settles nothing the batch reading wins,
so the `count` check is what reports the disagreement rather than a guess hiding
it.

Only the first few vectors of each item are summarised — five hundred histograms
help nobody — and the panel says so (`first 8 shown`). The **checks still read
every vector**: a hole in token 300 fails `finite` even though nothing drew it,
and names it `0#300`.

## Vectors are never rendered whole

Not in the logs, not in the API, not in the UI. You get the width, the L2 norm, a
sample of the first values and a distribution histogram. The raw response is
elided too (`"<1024 values elided; set includeVectors to see them>"`), because a
careful summary next to a `raw` field carrying all 1024 floats would be theatre —
the rest of the raw tree, which is what you actually read, is untouched.

`includeVectors: true` turns all of that off and gives you the full payload. It
is the only way to get it.
