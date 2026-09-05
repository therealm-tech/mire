# Configuration

How `mire` is wired: the options file, the directories it reads your models,
credentials, MCP servers and saved prompts from, and the two deployments that
need something said about them — a container, and a notebook behind a path
proxy.

The options themselves are the table in
[README.md](../README.md#configuration).

## The configuration file

Every flag in that table is also a key in a YAML file, read from
`~/.config/mire/mire.yaml` — or from wherever `--config` points:

```yaml
---
config_dir:
  - /etc/mire/config
  - ~/.config/mire/config
uploads: ~/.local/share/mire/uploads
port: 8788
ca_bundle: /etc/ssl/certs/internal.pem
log_filter: mire=debug
```

The keys are the long flag names with underscores — `base_path`, `public_url`,
`ca_bundle`, `log_filter` — and every one of them is optional: a file that sets
one thing is a perfectly good file. `config_dir:` takes a list, or a bare string
when there is only one.

**A flag beats the environment, the environment beats the file, and the file
beats the defaults.** The file is for the settings that stopped being a decision
— where your configuration lives, the CA bundle somebody put on the machine, the
port you already bookmarked. A shell alias covers those too, right up until you
open a different shell. The flag is still there for the afternoon you want
something else:

```sh
mire --port 9000            # the file's port, overruled, for this run
mire --config ./mire.yaml   # a different file entirely
```

`$XDG_CONFIG_HOME` is honoured when it is set. A leading `~` in a path is
expanded, because a YAML document has no shell behind it to do that for you — and
a file living in your home directory is precisely where you would write one.
`~someone-else` is left alone, since resolving another user's home takes more
than string handling.

Nobody has to write this file. The default location not being there is the
ordinary case, and `mire` comes up on its defaults without a word about it. A
file you *name* with `--config` does have to be there: a typo in that path must
not read as "you have no configuration file".

A file that is there and broken, on the other hand, is fatal — the opposite of
what happens to a broken model. Those are the input to the tool, and coming up
to show you the problem beats refusing to start. This file is the tool's own
wiring: a `port:` that did not parse means listening somewhere you did not ask
for, and `log_fitler:` means a setting you believe is in effect and is not. So it
says which key, and stops:

```
ERROR mire: mire stopped error=cannot parse the configuration file
  /home/you/.config/mire/mire.yaml: unknown field `log_fitler`, expected one of
  `config_dir`, `uploads`, `host`, `port`, `base_path`, `public_url`,
  `ca_bundle`, `log_filter`
```

It is read once, at startup. The configuration directories are watched because
what is in them changes while you work; the file that says *which* directories
those are, and which address to bind, cannot change under a running process.

## What's in a configuration directory

One subdirectory per kind of thing, one file per entry:

```
config/
├── models/          one model endpoint per file
│   ├── qwen3.yaml
│   ├── nomic.yaml
│   └── whisper.yaml
├── auth/            one credential provider per file
│   ├── static-token.yaml
│   ├── keycloak-workload.yaml
│   └── keycloak-user.yaml
├── mcp/             one MCP server per file
│   └── dev.yaml
└── prompts/         one saved prompt per file
    ├── 01-ping.yaml
    └── 02-call-a-tool.yaml
```

The name is the `name:` field inside the file, not the file name: renaming
`qwen3.yaml` must not silently rename the thing every other file refers to. The
file name is for whoever is reading the directory — which is why the prompts are
numbered, since the listing's order is the order the UI offers them in.

A subdirectory that is not there declares nothing. So does a file that declares
nothing — an empty one, or one that is entirely commented out, which is how
`config/mcp/` ships eight worked examples next to the one server it really has.
Uncomment one and it is live.

The directory itself does have to exist: a `--config-dir` that is not there is a
typo worth stopping for, and the startup error says which one.

## More than one configuration directory

`--config-dir` takes more than one directory, repeated or `:`-separated the way
`PATH` is:

```sh
mire --config-dir /etc/mire/config --config-dir ~/.config/mire/config
CONFIG_DIR=/etc/mire/config:~/.config/mire/config mire
```

This is for the case where the configuration is somebody else's. A team keeps a
directory of endpoints under review, checked into a repository, mounted
read-only; you want the same thing plus two of your own, and one of theirs
pointed at staging for the afternoon. Copying the whole directory to change one
line means never getting their next change.

Directories are layered in the order given, and **the last one wins**: a name
declared in more than one belongs to the last directory that declares it. That
is true of every kind of name in there — models, auth providers, MCP servers and
saved prompts alike, each merged on its own. Names nobody else claimed are simply
added, so the usual case is a base you leave alone and a short directory of your
own on top.

An override is a warning rather than an error, naming both files:

```
WARN mire::model::loader: model overridden by a later directory
  name=mistral-small path=/home/you/.config/mire/config/mistral-small.yaml
  shadowed=/etc/mire/config/mistral-small.yaml
```

Deliberate, so it does not belong in the UI's list of things that failed to
load — but not silent either, because "why is this model pointing at staging"
is a question that deserves an answer in the log rather than an afternoon.

The rule is about *different* directories. Two files in the **same** directory
claiming one name is still what it always was — a mistake, reported as a load
issue, with the first file keeping the name.

Every listed directory is watched, so editing your layer reloads exactly as
editing a single directory always did. All of them must exist: a directory you
named and `mire` cannot read is a typo worth stopping for, and the startup error
says which one.

## In a container

```sh
docker build -t mire:0.1.0 .
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/config:/etc/mire/config:ro" mire:0.1.0
```

Layering works the same way, and is most of the reason it exists — a shared
directory mounted read-only, yours writable on top:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/team-config:/etc/mire/config:ro" \
  -v "$PWD/config:/etc/mire/config.local:ro" \
  -e CONFIG_DIR=/etc/mire/config:/etc/mire/config.local mire:0.1.0
```

One static binary on `distroless/static` — a certificate bundle, timezone data,
`/etc/passwd`, and nothing else. No shell, no package manager, and nothing to
patch. It runs as UID 65532 and writes nothing, so `--read-only` costs nothing —
right up until somebody presses **Attach**, which is the one thing here that
wants a disk. Give it one, owned by the user the container runs as:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/config:/etc/mire/config:ro" \
  -v "$PWD/uploads:/var/lib/mire/uploads" \
  -e UPLOADS_DIR=/var/lib/mire/uploads mire:0.1.0
```

Without the mount `--read-only` is still exactly right, and the only thing that
fails is an upload — with a `500` naming the path it could not write. The
directory is created on the first attachment rather than at startup, so nothing
about this changes how the container comes up.

The configuration is **mounted, not baked in**. It is the input to the tool, not
part of it: an image carrying it would ship endpoints pointing at somebody
else's laptop, and testing a new endpoint would mean building a new image.
`/etc/mire/config` exists in the image so a run without a mount starts cleanly
with nothing to offer.

`HOST` defaults to `0.0.0.0` here, and only here. Inside a container, loopback is
a network nobody else can reach, so a published port would answer nothing. The
isolation is the container's to provide — publish to `127.0.0.1:8787` and the
exposure is the same as running the binary directly.

The image sets `HOST`, `PORT`, `CONFIG_DIR` and `LOG_FILTER` in its
environment, and the environment outranks the file — so a `mire.yaml` you mount
is read, but those four keys in it are not what takes effect. Change them with
`-e` and leave the file for everything else:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/config:/etc/mire/config:ro" \
  -v "$PWD/mire.yaml:/etc/mire/mire.yaml:ro" \
  -e CONFIG_FILE=/etc/mire/mire.yaml mire:0.1.0
```

The image sets no `HOME` at all — distroless does not, and a static binary has no
business guessing one — so there is no default location to find a file in. Name
it, as above, with `CONFIG_FILE`.

For an internal CA, mount the bundle and point `--ca-bundle` at it:

```sh
docker run --rm --read-only -p 127.0.0.1:8787:8787 \
  -v "$PWD/config:/etc/mire/config:ro" \
  -v /etc/ssl/certs/internal.pem:/etc/mire/ca.pem:ro \
  -e CA_BUNDLE=/etc/mire/ca.pem mire:0.1.0
```

There is no `HEALTHCHECK`: the image holds no HTTP client to run one with, and
adding one would mean adding a shell back. Point your orchestrator at `/healthz`
instead — that is the same check, run by something that already has a client.

## From a notebook behind a path proxy

Notebook proxies serve you at something like
`/notebook/<namespace>/<name>/proxy/8787/`, and they come in two kinds. **Which
one you have decides whether you want `--base-path`, and getting it wrong is the
one configuration mistake that produces a genuinely cryptic error** — so find out
first:

```sh
mire --config-dir ./config --log-filter 'mire=debug,tower_http=debug'
```

Load the page through the proxy and read the `uri=` field of the request log.

**If the prefix is forwarded** (`uri=/notebook/my-namespace/my-notebook/proxy/8787/`),
tell `mire` about it and every route moves under it:

```sh
mire --config-dir ./config --base-path /notebook/my-namespace/my-notebook/proxy/8787
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
