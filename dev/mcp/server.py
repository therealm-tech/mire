#!/usr/bin/env python3
"""A minimal MCP server, revision 2026-07-28, Streamable HTTP.

Standard library only, so it runs on a bare `python:*-alpine` with nothing to
install. It exists to give `mire` something real to call — and, just as usefully,
to *check `mire`*: it validates the mirrored request headers the way the
specification requires, so a client that gets `Mcp-Method`, `Mcp-Name` or
`Mcp-Param-*` wrong is told so with `-32020` instead of being quietly tolerated.

Three tools, chosen to exercise the paths that matter:

* `get_weather` — the ordinary case, read-only.
* `slow_echo`   — answers as an SSE stream with progress first, so the streaming
                  branch of the client is exercised.
* `explode`     — always answers `isError: true`, which is a *result* the model
                  is supposed to react to, not a transport failure.

Nothing here is a real MCP server. It is a test pattern, like everything else in
this directory.
"""

import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROTOCOL_VERSION = "2026-07-28"
PORT = 11436

# `Mcp-Param-*` values that cannot travel as plain ASCII arrive wrapped.
SENTINEL = re.compile(r"^=\?base64\?(.*)\?=$")

TOOLS = [
    {
        "name": "get_weather",
        "title": "Current weather",
        "description": "Current weather for a city.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"},
                "units": {
                    "type": "string",
                    "description": "celsius or fahrenheit",
                    # Mirrored into `Mcp-Param-Units`, so a conforming client has
                    # to send it as a header too, matching the body.
                    "x-mcp-header": "Units",
                },
            },
            "required": ["city"],
        },
        "annotations": {"readOnlyHint": True, "destructiveHint": False},
    },
    {
        "name": "slow_echo",
        "description": "Echoes its text back after reporting progress.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "explode",
        "description": "Always fails. Use it to see what the model does about it.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
]


def decode_header(value):
    """Undoes the base64 sentinel wrapper, if there is one."""
    import base64

    match = SENTINEL.match(value or "")
    if not match:
        return value
    return base64.b64decode(match.group(1)).decode("utf-8")


def weather(arguments):
    city = arguments.get("city", "somewhere")
    units = arguments.get("units", "celsius")
    temperature = 21 if units == "celsius" else 70
    return {
        "content": [
            {
                "type": "text",
                "text": json.dumps(
                    {"city": city, "temp": temperature, "units": units, "conditions": "clear"}
                ),
            }
        ],
        "structuredContent": {"temp": temperature, "conditions": "clear"},
        "isError": False,
    }


def explode(_arguments):
    return {
        "content": [{"type": "text", "text": "the tool blew up, as advertised"}],
        "isError": True,
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # noqa: A002 - signature is the base class's
        print(f"mcp {self.address_string()} {fmt % args}", flush=True)

    def _json(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _error(self, status, code, message, request_id=1):
        self._json(
            status,
            {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}},
        )

    def _stream(self, events):
        """Answers as SSE: notifications, then the response, then close."""
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-store")
        self.send_header("x-accel-buffering", "no")
        self.send_header("connection", "close")
        self.end_headers()
        for event in events:
            self.wfile.write(f"event: message\ndata: {json.dumps(event)}\n\n".encode())
            self.wfile.flush()
        self.close_connection = True

    def do_GET(self):  # noqa: N802 - name fixed by the base class
        # The GET stream is gone in this revision.
        self.send_response(405)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self):  # noqa: N802 - name fixed by the base class
        if self.path.rstrip("/") != "/mcp":
            return self._error(404, -32601, f"no MCP endpoint at {self.path}")

        length = int(self.headers.get("content-length") or 0)
        try:
            message = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError as error:
            return self._error(400, -32700, f"parse error: {error}")

        request_id = message.get("id", 1)
        method = message.get("method")
        params = message.get("params") or {}

        version = self.headers.get("mcp-protocol-version")
        if version != PROTOCOL_VERSION:
            return self._error(
                400,
                -32022,
                f"unsupported protocol version {version!r}; this server speaks {PROTOCOL_VERSION}",
                request_id,
            )

        # Header/body agreement. The whole reason the headers exist is that an
        # intermediary may route on them without reading the body, so a
        # disagreement is a security problem rather than a nitpick.
        if self.headers.get("mcp-method") != method:
            return self._error(
                400, -32020, "Mcp-Method does not match the body", request_id
            )

        if method == "tools/call":
            name = params.get("name")
            if decode_header(self.headers.get("mcp-name")) != name:
                return self._error(
                    400, -32020, "Mcp-Name does not match the body", request_id
                )
            mismatch = self._check_mirrored(name, params.get("arguments") or {})
            if mismatch:
                return self._error(400, -32020, mismatch, request_id)

        if method == "server/discover":
            return self._json(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "resultType": "complete",
                        "protocolVersions": [PROTOCOL_VERSION],
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "mire-dev-mcp", "version": "0.1.0"},
                    },
                },
            )

        if method == "tools/list":
            return self._json(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "resultType": "complete",
                        "tools": TOOLS,
                        "ttlMs": 60000,
                        "cacheScope": "public",
                    },
                },
            )

        if method == "tools/call":
            return self._call(request_id, params)

        return self._error(404, -32601, f"method not found: {method}", request_id)

    def _check_mirrored(self, name, arguments):
        """Every `x-mcp-header` parameter that is present must arrive as a header."""
        tool = next((candidate for candidate in TOOLS if candidate["name"] == name), None)
        if not tool:
            return None

        for key, schema in tool["inputSchema"].get("properties", {}).items():
            header_name = schema.get("x-mcp-header")
            if not header_name or key not in arguments:
                continue
            sent = decode_header(self.headers.get(f"mcp-param-{header_name}".lower()))
            expected = arguments[key]
            if isinstance(expected, bool):
                expected = "true" if expected else "false"
            if sent != str(expected):
                return (
                    f"Mcp-Param-{header_name} is {sent!r} but the body says {expected!r}"
                )
        return None

    def _call(self, request_id, params):
        name = params.get("name")
        arguments = params.get("arguments") or {}

        if name == "get_weather":
            result = weather(arguments)
        elif name == "explode":
            result = explode(arguments)
        elif name == "slow_echo":
            # Progress first, then the answer: the streaming branch.
            return self._stream(
                [
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {"progress": 1, "total": 2},
                    },
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "resultType": "complete",
                            "content": [
                                {"type": "text", "text": arguments.get("text", "")}
                            ],
                            "isError": False,
                        },
                    },
                ]
            )
        else:
            return self._error(404, -32601, f"no tool named {name!r}", request_id)

        result["resultType"] = "complete"
        return self._json(200, {"jsonrpc": "2.0", "id": request_id, "result": result})


if __name__ == "__main__":
    print(f"mire dev MCP server on :{PORT}/mcp, protocol {PROTOCOL_VERSION}", flush=True)
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()  # noqa: S104
