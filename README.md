# rproxy — Self-Hosted Residential Proxy Service

A production-quality residential proxy service written in Rust.  Two binaries:

| Binary | Role |
|--------|------|
| `rproxy-server` | Proxy gateway — accepts SOCKS5 / HTTP CONNECT from client apps |
| `rproxy-agent` | Exit node — runs on residential machines, tunnels traffic to targets |

## Architecture

```
[Client App]
    │  SOCKS5 :1080  or  HTTP CONNECT :8080
    ▼
[rproxy-server]  ←── picks provider from pool, opens yamux stream
    │  WSS :8888  (provider WebSocket endpoint)
    │  yamux multiplexing — many proxy streams over one WebSocket
    ▼
[rproxy-agent]   ←── runs on the residential machine, accepts yamux streams
    │  direct TCP
    ▼
[Target Website]
```

**Key design decisions:**
- Provider agents connect **outbound** to the server (NAT-friendly, no port forwarding needed)
- Streams are multiplexed via **yamux** over a single persistent WebSocket per provider
- Gateway uses `Mode::Client` (opens streams); agent uses `Mode::Server` (accepts streams)
- Thin VLESS-inspired framing header inside each yamux stream for addressing

## Quick Start

### 1. Build

```bash
cargo build --release
```

### 2. Start the server

```bash
RPROXY_PROVIDER_TOKEN=my-secret-token ./target/release/rproxy-server
```

Listens on:
- `:1080` — SOCKS5 for client apps
- `:8080` — HTTP CONNECT for client apps  
- `:8888` — WebSocket for provider agents

### 3. Start an agent (on a residential machine)

```bash
RPROXY_TOKEN=my-secret-token \
RPROXY_SERVER_URL=ws://your-server:8888 \
RPROXY_GEO=US-CA \
  ./target/release/rproxy-agent
```

The agent connects outbound, authenticates, and waits for proxy requests.

### 4. Use the proxy

```bash
curl --socks5 your-server:1080 https://example.com
# or
curl --proxytunnel --proxy http://your-server:8080 https://example.com
```

## Configuration

### rproxy-server

| Flag | Env | Default | Description |
|------|-----|---------|-------------|
| `--socks5-port` | `RPROXY_SOCKS5_PORT` | `1080` | SOCKS5 listener port |
| `--http-port` | `RPROXY_HTTP_PORT` | `8080` | HTTP CONNECT listener port |
| `--provider-port` | `RPROXY_PROVIDER_PORT` | `8888` | Provider WebSocket port |
| `--provider-token` | `RPROXY_PROVIDER_TOKEN` | `change-me` | Shared secret for auth |

### rproxy-agent

| Flag | Env | Default | Description |
|------|-----|---------|-------------|
| `--server-url` | `RPROXY_SERVER_URL` | `ws://127.0.0.1:8888` | Server WebSocket URL |
| `--token` | `RPROXY_TOKEN` | `change-me` | Auth token |
| `--geo` | `RPROXY_GEO` | — | Geographic label (e.g. `US-CA`) |
| `--label` | `RPROXY_LABEL` | — | Human-readable label |
| `--accept-invalid-certs` | `RPROXY_ACCEPT_INVALID_CERTS` | false | Skip TLS cert validation |

## Wire Protocol

Each yamux stream carries one proxied TCP connection:

```
┌──────────┬────────────────┬──────────┐
│ Ver (1B) │ AddrType (1B)  │ Port (2B)│
│ Addr (variable)            │          │
└──────────┴────────────────┴──────────┘
 ← raw TCP byte stream follows →
```

Address types: `0x01` IPv4, `0x03` Domain, `0x04` IPv6.

Agent replies with `0x00` (success) or `0x01` (error + message) before relaying.

## TLS / WSS

For production, put the server behind a reverse proxy (Nginx/Caddy) with TLS
and point agents at `wss://your-domain:443`. The agent supports WSS natively.

## License

MIT
