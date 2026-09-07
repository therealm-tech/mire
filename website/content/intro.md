# Introduction

`mire` is a test pattern for model endpoints. You put a known signal in, and you
look at what comes out.

You deployed a model, or you changed a route, and you want to know four things:
does the endpoint answer, is the auth actually enforced, is the response shaped
the way you expect, does tool calling work. Today that is a copy-pasted `curl`.
`mire` is the same thing, reproducible, with the credential handling a browser
tab cannot give you.

## What it is

One binary you run yourself, next to your work. It listens on loopback, serves a
small web UI and an HTTP API, and makes every outbound call on your behalf — to
the model endpoint under test, to the MCP servers a run reaches, to an identity
provider, and to whatever a hook points at.

That last part is the shape everything else follows from. Because the calls
originate in the process rather than in the tab:

- **There is no CORS to fight.** The endpoint never sees a browser.
- **A workload identity is testable.** The credential is exchanged in the
  process and never has to reach a page.
- **There is exactly one place a credential can leak**, so masking it is an
  invariant with one implementation rather than four.

Endpoints, credentials, MCP servers and saved prompts are YAML files you own,
read from directories you point at and reloaded when you save — no restart, and
no button in the UI that writes into your configuration behind your back.

## What it is not

It is not a gateway, not a proxy you put in front of anything, and not a
deployment: no chart, no cluster, nothing running for anybody else. There is a
container image, but it is the same binary with the same lifetime, for when a
notebook is easier to hand an image than a binary.

Nothing is stored between runs. The conversation and the traffic live in the
browser tab; the only thing `mire` writes to disk is a file you attached.

## Where to start

| If you want to | Read |
| --- | --- |
| Get the binary or the image | [Installation](/docs/getting-started/installation) |
| Send one call and read the answer | [Your first call](/docs/getting-started/first-call) |
| Point it at your own endpoints | [Configuration](/docs/guides/configuration) and [Models](/docs/guides/models) |
| Put a real credential on a call | [Credentials](/docs/guides/auth) |
| Watch a model actually call tools | [MCP servers](/docs/guides/mcp) and [The agent loop](/docs/guides/agent-loop) |
| Look up a key you saw in a file | [Configuration keys](/docs/reference/configuration-keys) |
| Drive it from a script instead of the UI | [HTTP API](/docs/reference/api) |
| Know why it is built this way | [Architecture](/docs/internals/architecture) |

## How the documentation is organised

**Getting started** takes you from nothing to a decoded answer. **Guides** are
one document per part of the tool, each one written around a task rather than
around a module. **Reference** is the exhaustive surface: every option, every
route. **Internals** describes the system as it stands and records the decisions
that shaped it.

Every page is generated from the markdown in the repository, so the "Edit this
page" link at the bottom leads to the file that produced it.
