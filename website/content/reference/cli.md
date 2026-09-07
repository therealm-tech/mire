# Command line and options

`mire` is one binary with no subcommand: it serves the API and the UI, and that
is all it does.

```sh
mire [OPTIONS]
```

## Every option, three ways

Every option is a flag, an environment variable, and a key in the configuration
file — and they win **in that order**: a flag beats the environment, the
environment beats the file, the file beats the default.

| Flag | Variable | File key | Default | What it does |
| --- | --- | --- | --- | --- |
| `--config <PATH>` | `CONFIG_FILE` | *(n/a)* | `~/.config/mire/mire.yaml` | YAML file carrying every key below |
| `--config-dir <PATH>` | `CONFIG_DIR` | `config_dir` | `./config` | Directories holding `models/`, `auth/`, `mcp/`, `prompts/` and `decodes/` |
| `--uploads <PATH>` | `UPLOADS_DIR` | `uploads` | `./uploads` | Where attached files are written |
| `--host <ADDR>` | `HOST` | `host` | `127.0.0.1` | Address to listen on |
| `--port <PORT>` | `PORT` | `port` | `8787` | Port to listen on |
| `--base-path <PATH>` | `BASE_PATH` | `base_path` | *(none)* | Path prefix for every route, when a proxy forwards one |
| `--public-url <URL>` | `PUBLIC_URL` | `public_url` | *(none)* | Origin the browser reaches `mire` on, for the OIDC callback |
| `--ca-bundle <PATH>` | `CA_BUNDLE` | `ca_bundle` | *(none)* | PEM bundle of extra certificate authorities to trust |
| `--log-filter <FILTER>` | `LOG_FILTER` | `log_filter` | `info` | `tracing` filter directive |

No flag carries a default of its own. A default there would be
indistinguishable from something you asked for, and would quietly outrank the
file; the defaults are applied once, after every source has been folded in.

`--help` prints the same table with the same defaults, and `--version` prints
the version.

## The options file

Nobody has to write it. It is for the settings that stopped being a decision —
where your configuration lives, the CA bundle somebody put on the machine, the
port you already bookmarked.

```yaml
---
config_dir:
  - /etc/mire/config
  - ~/mire
uploads: ~/mire/uploads
host: 127.0.0.1
port: 8787
base_path: /notebook/team/lab/proxy/8787
public_url: https://kubeflow.example
ca_bundle: ~/certs/internal.pem
log_filter: mire=debug,tower_http=debug
```

Read **once, at startup, and never again**. The configuration directories are
watched because their contents are the input to the tool and change while you
work; this file says which directories those are and which address to bind, and
neither of those can change under a running process.

### Where it is looked for

`$XDG_CONFIG_HOME/mire/mire.yaml`, falling back to `$HOME/.config/mire/mire.yaml`.
With no home directory at all — a container, typically — there is simply
nowhere to look, and the flags and the environment are all of it.

### A leading `~` is expanded, in this file only

A flag or an environment assignment is written in a shell, which expands `~`
before `mire` sees anything; a YAML document has no shell behind it. `~someone`
is left alone: resolving another user's home needs the password database, and a
wrong guess is worse than the literal path.

### A broken file is fatal

This is the opposite of the policy for everything *in* the configuration
directories, where one malformed model is skipped and reported so the tool can
come up and show you what is wrong. Here the file is `mire`'s own wiring:

- **A key that does not parse stops the process.** A `port:` that did not parse
  means listening somewhere nobody asked for.
- **A key that is not a key stops the process.** `log_fitler:` would otherwise
  parse cleanly and do nothing, which is the worst answer available — so unknown
  keys are refused, and the error names the one it did not recognise.
- **A file named with `--config` has to exist.** The default location not
  existing is the ordinary case and not a complaint; a path you typed is a
  promise, and falling back to the defaults would turn a typo into a silently
  different configuration.

## Notes on individual options

### `--config-dir`

**Repeatable**, and `:`-separated in the environment variable, the way `PATH`
is:

```sh
mire --config-dir ./base --config-dir ./mine
CONFIG_DIR=./base:./mine mire
```

Directories are layered in the order given. An entry declared in more than one
belongs to the **last** directory that declares it, and the one it displaced is
named in a warning. A directory somebody else maintains, and yours on top of it,
without copying theirs to change one line.

In the file the key takes one path or a list — a file that can hold a list has
no reason to pack one into a string:

```yaml
config_dir: /etc/mire/config
```

A flag **replaces** the file's list rather than extending it.

Each directory holds one subdirectory per kind of thing and one file per entry:
`models/qwen3.yaml` is the model named `qwen3`. A subdirectory that is not there
declares nothing, which is how a directory that only adds prompts works.
[Configuration keys](/docs/reference/configuration-keys) is every key of every
one of those files; [Configuration](/docs/guides/configuration) is the same
ground written around a task.

### `--uploads`

Created on the first upload, not at startup. `mire` otherwise writes nothing at
all, so a read-only filesystem is only ever a problem for somebody who actually
attaches something. Nothing is overwritten and nothing is deleted: every stored
name carries a random prefix.

### `--host`

Loopback by default. Widening it is a deliberate act — the process holds
credentials and makes outbound calls on your behalf.

### `--base-path`

A prefix applied to every route, for a notebook served behind a path proxy —
`/notebook/<namespace>/<name>/proxy/8787`. The embedded UI's base URL is
rewritten to match.

### `--public-url`

Used by the OIDC **browser** login alone, to build its callback URL. Left unset,
the UI supplies the origin it is actually being served from, which is right in
every case that does not involve a proxy rewriting paths. The value must match
what the identity provider has registered.

### `--log-filter`

A `tracing` filter directive: `info`, or `mire=debug,tower_http=debug`. The
[`EnvFilter` directive syntax](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives)
is the full grammar.

Credentials never reach a log line, whatever the level.

## Build-time environment

One variable affects the build rather than the run. `build.rs` builds the front
end with Vite and embeds it, so `npm` has to be on `PATH`:

| Variable | Values | What it does |
| --- | --- | --- |
| `MIRE_BUILD_UI` | `0` or `1` | `0` skips the front-end build and embeds a placeholder page instead |

Anything other than `0` or `1` fails the build rather than being guessed at.
With `MIRE_BUILD_UI=0` the API is fully functional and only the page is a stub —
which is what the Rust test suite builds against.

## Routes outside the API

Two routes exist that the OpenAPI document does not describe, because neither is
API surface: `GET /auth/callback`, the page an identity provider sends a browser
back to, and `GET /healthz`, liveness. Everything else is in
[HTTP API](/docs/reference/api).
