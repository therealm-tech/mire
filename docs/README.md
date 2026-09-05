# mire documentation

Depth on each part of [`mire`](../README.md). The root
[README](../README.md#getting-started) covers installing and running it, and
[ARCHITECTURE.md](../ARCHITECTURE.md) covers how it is put together and why.

| Document | What it covers |
| --- | --- |
| [configuration.md](configuration.md) | The options file, configuration directories and how they layer, containers, notebook path proxies |
| [models.md](models.md) | Turning a `curl` into a model file: templates, decode cascades, Rhai scripts, `multipart/form-data` endpoints |
| [auth.md](auth.md) | Credential providers: static tokens, OIDC workload identities, browser logins, `allowed_hosts` |
| [mcp.md](mcp.md) | Real tool calls: declaring servers, protocol revisions, per-run selection, header templates, hooks, captured variables |
| [agent-loop.md](agent-loop.md) | The turn loop, simulated tools, stop conditions and named outcomes |
| [streaming.md](streaming.md) | Streaming a call or a loop, and time to first token |
| [embeddings.md](embeddings.md) | Embedding models and the checks their answers are judged on |
| [ui.md](ui.md) | The web UI: conversation, attachments, saved prompts, the traffic panel |
| [api.md](api.md) | The HTTP routes |
| [dev-stack.md](dev-stack.md) | The `docker-compose.yaml` stack to point it at |
