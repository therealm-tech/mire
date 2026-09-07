# Credentials

A model says where a request goes; `auth/` says who it goes as. One file per
provider, named by a model's `auth:` — and by an [MCP server's](mcp.md), and by
a hook's.

Start from the `curl` you already have. Its `Authorization` header becomes
`auth: gateway-token` in the model (see [writing a model](models.md)), a
reference to an entry in `auth/`, next to your models:

```yaml
# auth/gateway-token.yaml
---
name: gateway-token
kind: token
value:
  env: MODEL_TOKEN
```

**The token itself never goes in a model.** It comes from an environment
variable, a file re-read on every call (so a rotated service account token just
works), or the UI. `auth/` is safe to commit; it only says where to look.

`anonymous` always exists without being declared. That is what lets you ask "is
this route actually protected?" — and a `401` from it is a *passing* result, not
a failure.

Any provider may add `allowed_hosts`, and every kind honours it:

```yaml
allowed_hosts:
  - models.internal
```

An empty list — the default — means anywhere. A non-empty one is a rule about
where that credential may be sent, refused before anything goes out, and it is
also what keeps the UI from offering a provider against a model pointing
somewhere it is not allowed to go.

## Testing with a workload identity

The third mode is the one that reproduces what a pod actually does. `mire`
performs the `client_credentials` exchange itself, caches the access token and
renews it 60 seconds before expiry:

```yaml
# auth/models-workload.yaml — one to write; the directory ships
# `keycloak-workload.yaml`, which is this shape against the compose stack
---
name: models-workload
kind: oidc
issuer: https://idp.internal/realms/models
client_id: mire
client_assertion:
  file: /var/run/secrets/kubernetes.io/serviceaccount/token
audience: https://models.internal
```

`client_assertion` presents a **projected service account token** as an RFC 7523
assertion. It is re-read from disk on every exchange, so a rotated token is a
non-event rather than a mysterious `401` an hour after deploying. Use
`client_secret` (from `env` or `file`) instead for a plain confidential client.

Discovery reads `{issuer}/.well-known/openid-configuration`; set
`token_endpoint` explicitly to skip it when an IdP's well-known document is wrong
or unreachable. The exchange goes through the same HTTP client as everything
else, so `--ca-bundle` applies to your IdP too.

## Testing as yourself

A workload identity answers "what does the pod get?". Sometimes the question is
"what do *I* get?" — a gateway that accepts service accounts and user tokens
under different rules gives different answers, and only one of them is the one
your users will hit.

`kind: oidc_browser` runs the authorization code flow with PKCE: click **Sign
in**, a tab opens at your identity provider, and the token that comes back is
yours.

```yaml
# auth/me.yaml
---
name: me
kind: oidc_browser
issuer: https://idp.internal/realms/models
client_id: mire-ui
scope:
  - openid
  - profile
```

No `client_secret` — `mire` runs from a directory of YAML files and has no secret
to keep, which is exactly the case PKCE exists for. One is still accepted, for an
IdP configured with a confidential client.

**The callback follows the browser, not the socket.** This is the part that
matters in a Kubeflow notebook: the process binds `127.0.0.1:8787`, while your
browser is at `https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`.
Nothing inside the process can derive the second from the first, so the UI
computes the callback from `document.baseURI` and sends it with the login
request. Register `<that origin><base-path>/auth/callback` with your identity
provider and the flow works unchanged behind the proxy.

Set `--public-url` (or `PUBLIC_URL`) when a proxy rewrites paths and the
browser's own answer is wrong; it overrides everything else. Note that a redirect
URI the identity provider has not registered is refused *by the identity
provider* — that check is the one that counts, and it is not ours to weaken.

Tokens stay on the server: the UI is told a username, the granted scopes and a
countdown, never a token. When the access token expires, the refresh token is
used silently; when there is no refresh token, or the IdP has dropped the
session, the session is cleared and the button comes back rather than every call
failing the same way forever.

Signing out drops what `mire` holds. It does **not** sign you out of the identity
provider — the next login may complete without a prompt, which is worth knowing
when you are trying to come back as somebody else.

On a `401`, a token that had been *reused* is dropped and the request is replayed
exactly once with a fresh one. A token minted for that very call is left alone —
the `401` is then about something else (a missing scope, an audience mismatch),
and replaying would only hide it.

Since the same model can be pointed at all three modes without touching its
model, the matrix is two `POST /api/call` bodies apart:

```sh
for auth in anonymous static-token keycloak-workload; do
  curl -s localhost:8787/api/call -H 'content-type: application/json' \
    -d "{\"model\": \"qwen3\", \"auth\": \"$auth\", \"prompt\": \"ping\"}" |
    jq -r '"\(.auth): \(.response.http.status)"'
done
```

Editing anything in that directory — a model *or* a provider — reloads it: the
watcher picks the change up without a restart. Both swap together, so a call
never sees a new model against an old auth registry.

A broken file never stops `mire` from starting, and never takes the good ones
down with it. One malformed model, or one bad provider, is skipped and reported:
`GET /api/config` lists every directory's failures with the file, the message and
the position, and `GET /api/models` and `GET /api/auth` each carry the same
`issues` list for their own directory. You reach for this tool when something is
already wrong — it should come up and show you what, not refuse to run until its
own config is perfect.

The token values themselves are read on **every** call, not cached: a rotated
service account token file is picked up on the next request.

## The same credential against three environments

An issuer, a client id and a secret that differ per environment are one file with
a `stages:` block, not three files that quietly stop matching:

```yaml
---
name: keycloak-workload
kind: oidc
issuer: ${ stage.issuer }
client_id: mire
client_secret:
  env: ${ stage.secret_env }
audience: ${ stage.audience }
allowed_hosts: ${ stage.hosts }

default_stage: dev
stages:
  dev:
    issuer: http://127.0.0.1:8080/realms/mire
    secret_env: MIRE_CLIENT_SECRET_DEV
    audience: models-dev
    hosts:
      - 127.0.0.1
  preprod:
    issuer: https://idp.preprod.internal/realms/models
    secret_env: MIRE_CLIENT_SECRET_PREPROD
    audience: https://models.preprod.internal
    hosts:
      - models.preprod.internal
```

`keycloak-workload@preprod` is then a credential like any other, and it is
independent of the model's own stage — a `prod` model authenticated by a
`preprod` identity is a question worth asking, and asking it is one field on the
call. **Two stages are two identities**: their own token cache, their own browser
session, so signing in to one is not signing in to the other.

No credential is in that file, as ever: `${ stage.secret_env }` names the
variable to read, and the value never leaves the process.
See [configuration.md](configuration.md#stages) for the rest.
