#!/usr/bin/env python3
"""Send one hand-built OTLP/HTTP JSON trace and report what the endpoint did with it.

Ground truth for Pessimal's usage-reporting encoder (docs/plans/2026-09-11-m8-usage-reporting.md).
The Rust encoder's golden fixtures come from whatever shape this script gets accepted, so that the
fixtures and the encoder do not share an author.

OTLP/JSON is *not* plain ProtoJSON. What this probe emits is the spec-correct shape:

  * `traceId` and `spanId` are lowercase **hex**, where ProtoJSON would base64 a `bytes` field.
  * every 64-bit field (`*TimeUnixNano`, `intValue`) is a **decimal string**, not a number.
  * `kind` and `status.code` are integers.
  * a root span **omits** `parentSpanId` rather than sending zeros.

Measured against otelcol-contrib 0.160.0 on 2026-09-11, rather than assumed — the three deviations
behave differently, and only one of them is loud:

  | deviation                          | result                                            |
  |------------------------------------|---------------------------------------------------|
  | base64 ids                         | **400**, with `ID.UnmarshalJSONIter: length mismatch` |
  | int64 as a JSON number             | accepted, and exact — jsoniter reads int64 without float64 rounding, so `…123456789` ns round-tripped intact |
  | all-zero `parentSpanId` on a root  | accepted, normalised to no parent                 |

So the collector is stricter than feared about ids and more forgiving than the spec about numbers.
Emit the spec shape anyway: the leniency is this receiver's, not OTLP's, and a different backend is
under no obligation to repeat it.

What no status code can tell you is whether a *well-formed* span was stored and is queryable. That
is why this script prints the trace id and tells you to go and look.

Usage:

    scripts/otlp-trace-probe.py http://localhost:4318
    OTLP_HEADER_NAME=signoz-ingestion-key OTLP_HEADER_VALUE=... \
        scripts/otlp-trace-probe.py https://ingest.eu.signoz.cloud:443

The credential comes from the environment, never argv, so it stays out of shell history and out of
any process listing. It is never printed.

Stdlib only, Python 3.9 compatible: this has to run on a CI guest with no pip install.
"""

import json
import os
import secrets
import sys
import time
import urllib.error
import urllib.request

# OTel's own enum values. Spelled out because a wrong integer here is also a silent drop.
SPAN_KIND_INTERNAL = 1
SPAN_KIND_CLIENT = 3
STATUS_CODE_OK = 1


def hex_id(n_bytes):
    """A trace (16 bytes) or span (8 bytes) id, as OTLP/JSON wants it: lowercase hex."""
    return secrets.token_bytes(n_bytes).hex()


def attribute(key, value):
    """One KeyValue. int goes out as a decimal string — intValue is an int64."""
    if isinstance(value, bool):
        any_value = {"boolValue": value}
    elif isinstance(value, int):
        any_value = {"intValue": str(value)}
    elif isinstance(value, float):
        any_value = {"doubleValue": value}
    else:
        any_value = {"stringValue": str(value)}
    return {"key": key, "value": any_value}


def span(name, trace_id, span_id, start_ns, end_ns, kind, attributes, parent_span_id=None):
    body = {
        "traceId": trace_id,
        "spanId": span_id,
        "name": name,
        "kind": kind,
        "startTimeUnixNano": str(start_ns),
        "endTimeUnixNano": str(end_ns),
        "attributes": attributes,
        "status": {"code": STATUS_CODE_OK},
    }
    if parent_span_id is not None:
        body["parentSpanId"] = parent_span_id
    return body


def build_request(trace_id):
    """One poll-cycle trace: a root with one child, shaped like the real client spans will be.

    Only allowlisted attributes appear, on purpose — this doubles as a worked example of the
    attribute table in the plan. No endpoint, no host name, no error message.
    """
    now_ns = time.time_ns()
    root_id, child_id = hex_id(8), hex_id(8)
    return {
        "resourceSpans": [
            {
                "resource": {
                    "attributes": [
                        attribute("service.name", "pessimal-client"),
                        attribute("service.version", "0.1.0"),
                        # Session-scoped by design: minted per launch, never persisted.
                        attribute("service.instance.id", "0199a0e4-0000-7000-8000-probe00000001"),
                        attribute("pessimal.platform", "ios"),
                        attribute("pessimal.device.class", "phone"),
                        attribute("pessimal.os.version", "18.4"),
                    ]
                },
                "scopeSpans": [
                    {
                        "scope": {"name": "pessimal.usage", "version": "0.1.0"},
                        "spans": [
                            span(
                                "pessimal.client.poll",
                                trace_id,
                                root_id,
                                now_ns,
                                now_ns + 412_000_000,
                                SPAN_KIND_INTERNAL,
                                [
                                    attribute("pessimal.outcome", "ok"),
                                    attribute("pessimal.backend.kind", "signoz"),
                                    attribute("pessimal.hosts.count", 12),
                                    attribute("pessimal.alerts.firing", 1),
                                ],
                            ),
                            span(
                                "pessimal.client.gather",
                                trace_id,
                                child_id,
                                now_ns + 2_000_000,
                                now_ns + 390_000_000,
                                SPAN_KIND_CLIENT,
                                [
                                    attribute("pessimal.http.status_class", 2),
                                    attribute("pessimal.outcome", "ok"),
                                ],
                                parent_span_id=root_id,
                            ),
                        ],
                    }
                ],
            }
        ]
    }


def post(base_url, payload):
    url = base_url.rstrip("/") + "/v1/traces"
    body = json.dumps(payload).encode()
    request = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    name, value = os.environ.get("OTLP_HEADER_NAME"), os.environ.get("OTLP_HEADER_VALUE")
    if name and value:
        request.add_header(name, value)
        print("header:      {} (value not shown)".format(name))
    else:
        print("header:      none")

    print("POST         {} ({} bytes)".format(url, len(body)))
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "replace")
    except urllib.error.URLError as error:
        print("unreachable: {}".format(error.reason), file=sys.stderr)
        return None, None


def main(argv):
    """With --stdin, send a payload someone else built — used to put the Rust encoder's own output
    in front of a real receiver, so the encoder and its fixtures do not share an author."""
    args = [a for a in argv[1:] if a != "--stdin"]
    from_stdin = "--stdin" in argv
    if len(args) != 1:
        print(__doc__, file=sys.stderr)
        return 2

    if from_stdin:
        payload = json.load(sys.stdin)
        ids = {
            s.get("traceId")
            for rs in payload.get("resourceSpans", [])
            for ss in rs.get("scopeSpans", [])
            for s in ss.get("spans", [])
        }
        trace_id = ", ".join(sorted(i for i in ids if i)) or "(none found in payload)"
        print("payload:     from stdin, {} span(s)".format(
            sum(len(ss.get("spans", []))
                for rs in payload.get("resourceSpans", [])
                for ss in rs.get("scopeSpans", []))))
    else:
        trace_id = hex_id(16)
        payload = build_request(trace_id)

    status, body = post(args[0], payload)
    if status is None:
        return 1

    print("status:      {}".format(status))
    print("body:        {}".format(body.strip()[:400] or "(empty)"))
    print("trace id:    {}".format(trace_id))

    # A partial success is reported in the body with a 200, which is the one case where the status
    # alone would mislead.
    rejected = False
    if body.strip():
        try:
            parsed = json.loads(body)
            dropped = parsed.get("partialSuccess", {}).get("rejectedSpans")
            if dropped:
                print("REJECTED:    {} spans — {}".format(dropped, parsed["partialSuccess"]))
                rejected = True
        except ValueError:
            pass

    if status == 200 and not rejected:
        print()
        print("Accepted. That is necessary, not sufficient: a malformed span can be accepted and")
        print("dropped. Confirm the trace id above actually appears in the backend.")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
