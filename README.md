<p align="center">
  <img src="https://raw.githubusercontent.com/therealm-tech/mire/main/mire.svg"
       alt="mire logo" width="120">
</p>

# mire

A test pattern for model endpoints. You put a known signal in, and you look at
what comes out.

## Description

You deployed a model, or you changed a route, and you want to know four things:
does the endpoint answer, is the auth actually enforced, is the response shaped
the way you expect, does tool calling work. Today that is a copy-pasted `curl`.
This is the same thing, reproducible, with the credential handling that a browser
tab cannot give you.

`mire` is a single binary you run yourself, next to your work. It listens on
localhost, serves a small web UI and an HTTP API, and makes the outbound calls on
your behalf — which is why there is no CORS to fight and why workload identities
stay testable. Endpoints, credentials, MCP servers and saved prompts are YAML
files you own and reload without a restart.

It is not a gateway, a proxy you put in front of anything, or a deployment: no
chart, no cluster, nothing running for anybody else. There is a container image,
but it is the same binary with the same lifetime, for when a notebook is easier
to hand an image than a binary.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the design and the reasoning behind
it.

## Getting started

### Prerequisites

To run a released binary or the image, nothing but the binary or a container
runtime. To build from source:

- **Rust** — the toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml),
  installed for you by [`rustup`](https://rustup.rs)
- **Node.js ≥ 24** and `npm`, for the embedded UI — see
  [nodejs.org](https://nodejs.org/en/download)
- **Docker** (optional) — only for the container image and for
  [the dev stack](docs/dev-stack.md)

### Installation

**A released binary** — Linux only, statically linked against musl, so there is
no glibc version to match. Pick a version from the
[releases](https://github.com/therealm-tech/mire/releases) and the archive for
your architecture, `x86_64` or `aarch64`:

```sh
VERSION=0.2.0
curl -fsSLO "https://github.com/therealm-tech/mire/releases/download/v${VERSION}/mire-${VERSION}-linux-x86_64.tar.gz"
curl -fsSLO "https://github.com/therealm-tech/mire/releases/download/v${VERSION}/mire-${VERSION}-linux-x86_64.tar.gz.sha256"
sha256sum -c "mire-${VERSION}-linux-x86_64.tar.gz.sha256"
tar -xzf "mire-${VERSION}-linux-x86_64.tar.gz"
install -m 0755 mire ~/.local/bin/mire
```

**The container image**, published for `linux/amd64` and `linux/arm64`:

```sh
docker pull ghcr.io/therealm-tech/mire:latest
```

**From source**, which is also the macOS route — the UI is built into the
binary, so that is the whole deployment:

```sh
git clone https://github.com/therealm-tech/mire.git
cd mire
(cd ui && npm install && npm run build)
cargo build --release
```

The binary lands at `./target/release/mire`. Building without the front end works
too: you get a placeholder page and a fully functional API.

### Configuration

Every option is a flag, an environment variable, and a key in a configuration
file — in that order of precedence:

| Flag | Variable | Default | What it does |
| --- | --- | --- | --- |
| `--config` | `CONFIG_FILE` | `~/.config/mire/mire.yaml` | YAML file carrying every option below |
| `--config-dir` | `CONFIG_DIR` | `./config` | Directories holding `models/`, `auth/`, `mcp/` and `prompts/` |
| `--uploads` | `UPLOADS_DIR` | `./uploads` | Where **Upload files** writes |
| `--host` | `HOST` | `127.0.0.1` | Listen address; widening it is deliberate |
| `--port` | `PORT` | `8787` | Listen port |
| `--base-path` | `BASE_PATH` | *(none)* | Path prefix, when a proxy forwards one |
| `--public-url` | `PUBLIC_URL` | *(none)* | Origin the browser sees, for the OIDC callback |
| `--ca-bundle` | `CA_BUNDLE` | *(none)* | PEM bundle of extra trusted CAs |
| `--log-filter` | `LOG_FILTER` | `info` | `tracing` filter, e.g. `mire=debug` |

Nobody has to write the file, and no environment variable is required. See
[docs/configuration.md](docs/configuration.md) for the file, the configuration
directories and how several of them layer, for declaring one endpoint's `dev`,
`preprod` and `prod` in a single file with
[stages](docs/configuration.md#stages), and for running behind a container or a
notebook path proxy.

### Usage

Point it at a directory of endpoints and open the URL it prints:

```sh
mire --config-dir ./config
```

The same call from the API, which is what you paste into a ticket:

```sh
curl -s localhost:8787/api/call -H 'content-type: application/json' \
  -d '{"model": "qwen3", "prompt": "ping"}' | jq .response.decoded
```

`docker compose up -d` brings up a model runtime, an identity provider and a
gateway for the shipped `config/` to point at — see
[docs/dev-stack.md](docs/dev-stack.md).

In a container, mount the configuration rather than baking it in:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/config:/etc/mire/config:ro" ghcr.io/therealm-tech/mire:latest
```

### Documentation

| Document | What it covers |
| --- | --- |
| [docs/configuration.md](docs/configuration.md) | The options file, configuration directories and how they layer, stages (`dev`/`preprod`/`prod` in one file), containers, notebook path proxies |
| [docs/models.md](docs/models.md) | Turning a `curl` into a model file: templates, decode cascades, Rhai scripts, `multipart/form-data` endpoints |
| [docs/auth.md](docs/auth.md) | Credential providers: static tokens, OIDC workload identities, browser logins, `allowed_hosts` |
| [docs/mcp.md](docs/mcp.md) | Real tool calls: declaring servers, protocol revisions, per-run selection, header templates, hooks, captured variables |
| [docs/agent-loop.md](docs/agent-loop.md) | The turn loop, simulated tools, stop conditions and named outcomes |
| [docs/streaming.md](docs/streaming.md) | Streaming a call or a loop, and time to first token |
| [docs/embeddings.md](docs/embeddings.md) | Embedding models and the checks their answers are judged on |
| [docs/ui.md](docs/ui.md) | The web UI: conversation, attachments, saved prompts, the traffic panel |
| [docs/api.md](docs/api.md) | The HTTP routes |
| [docs/dev-stack.md](docs/dev-stack.md) | The `docker-compose.yaml` stack to point it at |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
