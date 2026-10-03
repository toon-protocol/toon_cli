# Adding an app to a connector

`toon add` puts a new app behind the connector of a TOON app you already run. The app gets
its own route, `g.toon.<segment>.<app>`, and its own price, and shares the connector's
identity, channels and peerings.

Add an app to an existing connector unless it needs its own identity, prices for the whole
connector, or its own peerings. Then [create a TOON app](creating-a-toon-app.md) instead.

## What an app is

An app is a plain HTTP service. The connector unseals each paid packet and makes the HTTP
request inside it (method, path and body) to the app, then returns the app's answer to the
payer. The app never sees a payment, so any HTTP service can be one. For full examples, see
[`gas-station`](https://github.com/toon-protocol/gas-station), and [`anytoon`](https://github.com/toon-protocol/anytoon).
The minimal app below is the one image this guide shows running under `toon add --image`.

Given as a container image, an app must:

- listen on port **3100** inside the container (`TOON_BLS_PORT` says so too),
- answer `GET /health` with `200` within two minutes of starting,
- keep anything it must not lose under **`/data`** (`TOON_DATA_DIR`), which is a directory in
  the agent node's home that survives restarts.

The app is given no environment beyond `TOON_BLS_PORT` and `TOON_DATA_DIR`. An image that
needs a secret or another variable to start fails with `app_failed`; `toon logs <app>` says why.

`toon` runs it with `docker`, its port published on loopback only: nothing reaches the app
except through its connector.

Any answer is a paid answer. A `404` or a `400` is real work the payer asked for, so the
packet is still fulfilled; status codes are for the payer to read, not a refund.

### A minimal app

```python
# app.py: answers every request with what it was sent
from http.server import BaseHTTPRequestHandler, HTTPServer

class App(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def answer(self):
        size = int(self.headers.get("content-length") or 0)
        body = b'{"path":"%s","received":%d}' % (self.path.encode(), len(self.rfile.read(size)))
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_POST = answer

HTTPServer(("0.0.0.0", 3100), App).serve_forever()
```

```dockerfile
FROM python:3.13-alpine
COPY app.py /app.py
EXPOSE 3100
CMD ["python", "-u", "/app.py"]
```

```sh
docker build -t echo-app .
```

The same app is used by the end-to-end run, in [`docs/end-to-end/app`](../end-to-end/app).

## Walkthrough: add an app

**1. Find the TOON app to add it to.**

```sh
toon status
# TOON app relay (ILP address g.toon.fb0e007c71750599): connector running ...
```

**2. Add it, with a price.** `--price` is what a client pays the connector for one packet
to the app, in base units; it defaults to 0.

```sh
toon add echo --to relay --image echo-app --price 2 --yes --json > add.json
jq '{address, restarted}' add.json
# { "address": "g.toon.fb0e007c71750599.echo", "restarted": true }
```

A connector reads its routes only when it starts, so `add` restarts it, which drops the
packets it holds in flight. While the connector is running, that needs `--yes`; without it
`add` fails with `confirmation_required` and changes nothing. On a stopped agent node no
`--yes` is needed and the app starts with the next `toon up`.

**3. Check it is running and pay it.**

```sh
toon status
toon logs echo
toon send "$(jq -r .address add.json)" --amount 2 --yes
```

A packet to an app behind your own connector needs no `--seal-to`. Others reach it over a
peering toward your connector, with `--seal-to` set to your connector's `/ilp` URL.

## An app you already serve

Give `--url` instead of `--image` and `toon` runs nothing; the connector delivers to that
URL. `status` shows it as served there, since its process is yours to keep running:

```sh
toon add search --to relay --url http://127.0.0.1:8080/ --price 10 --yes
```

Keep such an app on loopback. Anything that can reach its URL directly can use it without
paying.

## Choose the address

By default the route is `g.toon.<segment>.<app>`. `--address` sets another prefix. Keep it
under `g.toon.<segment>`: that is where every address the connector answers to sits.

```sh
toon add search-v2 --to relay --image search:2 --address g.toon.fb0e007c71750599.search.v2 --yes
```

## Change the price

```sh
toon route price g.toon.fb0e007c71750599.echo 5 --yes
toon route list
```

This restarts the connector too, so it needs `--yes` while the connector runs.

## Take an app away

```sh
toon remove echo --yes
```

It removes the app and its route and restarts the connector.

## What it refuses

| Error code | Meaning |
| --- | --- |
| `confirmation_required` | The connector is running and `--yes` was not given; nothing changed |
| `name_taken` | A TOON app or an app of this agent node already has that name, or it is not usable |
| `unknown_name` | `--to` does not name a TOON app of this agent node |
| `one_relay` | The image is the relay's: an agent node runs one relay, the one `init` made |
| `app_failed` | The app did not start or did not answer `/health`; `toon logs <app>` says why |
