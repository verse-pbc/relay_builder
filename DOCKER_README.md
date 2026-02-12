# Docker Build Instructions

## Quick Start

### Option 1: Pull from GHCR

```bash
docker pull ghcr.io/verse-pbc/relay_builder:latest
docker run -p 8080:8080 -v ./data:/data ghcr.io/verse-pbc/relay_builder:latest
```

### Option 2: Use the build script

```bash
./docker-build.sh
docker run -p 8080:8080 -v ./data:/data relay-builder
```

### Option 3: Use docker-compose

```bash
docker-compose up
```

### Option 4: Build manually

```bash
docker build -t relay-builder .
docker run -p 8080:8080 -v ./data:/data relay-builder
```

## Configuration

All configuration is done via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `RELAY_PORT` | `8080` | Listen port |
| `RELAY_BIND` | `0.0.0.0` | Bind address |
| `RELAY_DATA_DIR` | `./data` | LMDB database directory |
| `RELAY_URL` | `ws://localhost:8080` | Relay URL (for NIP-11) |
| `RELAY_NAME` | `Nostr Relay` | Relay name (NIP-11) |
| `RELAY_DESCRIPTION` | `A relay built with relay_builder` | Description (NIP-11) |
| `RELAY_CONTACT` | `` | Contact info (NIP-11) |
| `RELAY_ICON` | `` | Icon URL (NIP-11) |
| `MAX_CONNECTIONS` | `1000` | Max concurrent WebSocket connections |
| `MAX_SUBSCRIPTIONS` | `100` | Max subscriptions per connection |
| `MAX_SUBSCRIPTION_LIMIT` | `5000` | Max events per subscription query |
| `MAX_CONNECTION_DURATION` | `3600` | Connection time limit in seconds (0=unlimited) |
| `IDLE_TIMEOUT` | `300` | Idle timeout in seconds (0=unlimited) |
| `RATE_LIMIT_PER_SECOND` | `10` | Per-connection events/second (0=disabled) |
| `RATE_LIMIT_GLOBAL` | `0` | Global events/second (0=disabled) |
| `RUST_LOG` | `info` | Log level |

Example with custom configuration:

```bash
docker run -p 9090:9090 \
  -v ./data:/data \
  -e RELAY_PORT=9090 \
  -e RELAY_NAME="My Relay" \
  -e RELAY_DESCRIPTION="A custom Nostr relay" \
  -e RELAY_CONTACT="admin@example.com" \
  -e MAX_CONNECTIONS=500 \
  -e RATE_LIMIT_PER_SECOND=5 \
  -e RUST_LOG=debug \
  relay-builder
```

## Endpoints

- `ws://localhost:8080` - WebSocket (Nostr protocol)
- `http://localhost:8080` - HTML info page (browser)
- `http://localhost:8080` with `Accept: application/nostr+json` - NIP-11 relay info
- `http://localhost:8080/health` - Health check endpoint

## Features

The relay binary includes:

- **NIP-42** authentication support (clients can AUTH)
- **NIP-40** event expiration (automatic handling of expired events)
- **NIP-70** protected events (prevents unauthorized republishing)
- **Rate limiting** (per-connection and global)
- **Graceful shutdown** (SIGTERM/SIGINT)
- **Health endpoint** (`/health`)
- **Connection counting**
- **NIP-11** relay information document
