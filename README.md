# smser

`smser` is a tool and server to send and receive SMS messages using a Huawei E3372 USB modem on Linux. It is designed to run on a Raspberry Pi and includes features like a REST API, Prometheus metrics, and rate limiting. It is mostly implemented using Gemini CLI and Claude Code under human supervision.

GitHub Repo: https://github.com/ruediger/smser

[![Rust](https://github.com/ruediger/smser/actions/workflows/rust.yml/badge.svg)](https://github.com/ruediger/smser/actions/workflows/rust.yml)

## Warning

There is no authentication if you run this in server mode. Anyone with access to the server can read and send SMS!

The Huawei E3372 USB modem uses a web based interface without TLS support. All communication with it is in plain text, but should stay within the local host/modem.

Also the code is mostly written by AI (Gemini CLI using Gemini 2.5 and 3.0 models, as well as Claude Code 4.5).

## Features

*   **CLI Tool**: Send and receive SMS directly from the command line.
*   **REST API Server**: Expose SMS capabilities via a web API.
*   **Prometheus Metrics**: Export metrics for monitoring (sent SMS counts, rate limits, HTTP requests).
*   **Rate Limiting**: Configurable hourly and daily limits to prevent spam or over-usage.
*   **Status Page**: Simple HTML dashboard to view modem status and usage.
*   **Cross-Platform**: Easy cross-compilation for ARM64 (Raspberry Pi).

## Hardware Requirements

*   Huawei E3372 USB LTE Modem (HiLink mode).
*   Tested on Linux hosts (e.g., Raspberry Pi 4/5) running Debian/Raspbian.

## Installation

### Building from Source

Ensure you have Rust installed (via [rustup](https://rustup.rs/)).

```bash
cargo build --release
./target/release/smser --help
```

### Cross-Compilation for Raspberry Pi (ARM64)

To build for a Raspberry Pi 5 (running 64-bit OS) from an x86_64 host:

1.  Add the target:
    ```bash
    rustup target add aarch64-unknown-linux-gnu
    ```
2.  Build:
    ```bash
    cargo build --release --target aarch64-unknown-linux-gnu
    ```
    *Note: The project uses `ring` for cryptography to avoid GLIBC version mismatches often found with other libraries when cross-compiling.*

3.  Deploy to the Pi:
    ```bash
    scp target/aarch64-unknown-linux-gnu/release/smser user@your-pi:/usr/local/bin/
    ```

### Cargo Features

The project has optional features that can be enabled or disabled at build time:

| Feature | Default | Description |
|---------|---------|-------------|
| `modem` | Yes | Direct communication with Huawei E3372 modem |
| `server` | Yes | Web server with REST API (requires `modem`) |
| `alertmanager` | Yes | Prometheus AlertManager webhook handler (requires `server`) |

**Build variants:**
```bash
cargo build --release                      # Full build (modem + server + alertmanager)
cargo build --release --no-default-features  # Client-only build
```

**Client-only mode:** When built without the `modem` feature, the binary can only communicate with a remote smser server. This is useful for machines that don't have the modem attached. The `--remote-url` argument becomes required.

```bash
# Build client-only binary
cargo build --release --no-default-features

# Usage (--remote-url is required)
./smser --remote-url http://smser-server:8080 send --to +441234567890 --message "Hello!"
./smser --remote-url http://smser-server:8080 receive
```

## Usage

### Command Line Interface

**Send an SMS:**
```bash
smser send --to +441234567890 --message "Hello from smser!"
```

**Receive SMS:**
```bash
smser receive --count 5
```

**Remote Mode (talk to another smser server):**
```bash
smser --remote-url http://smser-server:8080 receive --count 5
smser --remote-url http://smser-server:8080 send --to +441234567890 --message "Hello!"
```

**Start the Server:**
```bash
smser serve --port 8080
```

### Server Mode

When running in server mode (`smser serve`), the following endpoints are available:

*   **`POST /send-sms`**: Send a message.
    *   Body: `{"to": "+123...", "message": "Content"}`
*   **`GET /get-sms`**: Retrieve messages.
    *   Params: `count` (default 20), `box_type` (default LocalInbox).
*   **`GET /metrics`**: Prometheus metrics endpoint.
*   **`GET /status`**: HTML status dashboard.
*   **`POST /alertmanager`**: Prometheus Alert Manager [webhook handler](https://prometheus.io/docs/alerting/latest/configuration/#webhook_config).
    *   Accepts standard Alert Manager JSON.
    *   Formats and sends alerts as SMS to the number configured via `--alert-to`.

#### Configuration & Logging

*   **Logging**: Controlled via `RUST_LOG` environment variable.
    ```bash
    RUST_LOG=info smser serve --alert-to +441234567890 --hourly-limit 50 --daily-limit 500
    ```
*   **Rate Limits**: Configurable via `--hourly-limit` (default 100) and `--daily-limit` (default 1000).

#### TLS Configuration

The server supports TLS for secure HTTPS connections:

```bash
smser serve --port 443 --tls-cert /path/to/cert.pem --tls-key /path/to/key.pem
```

When TLS is enabled, you can optionally start an HTTP redirect server on a separate port:

```bash
smser serve --port 443 --tls-cert cert.pem --tls-key key.pem --http-redirect-port 80
```

This will redirect all HTTP requests on port 80 to HTTPS on port 443.

To ensure redirects go to the correct hostname (matching your TLS certificate), use `--redirect-host`:

```bash
smser serve --port 443 --tls-cert cert.pem --tls-key key.pem \
  --http-redirect-port 80 --redirect-host myserver.example.com
```

#### Per-Client Rate Limiting

You can configure individual rate limits for specific clients. This is useful when different scripts or services need their own quotas.

```bash
# Server with per-client limits
smser serve --hourly-limit 100 --daily-limit 1000 \
  --client-limit networkfailover:5:20 \
  --client-limit monitoring:10:50

# Client identifies itself when sending
smser send --to +441234567890 --message "Network failed over" --client networkfailover
```

Via the API, include the `client` field in the request:
```json
{"to": "+441234567890", "message": "Hello", "client": "networkfailover"}
```

**Notes:**
- Named clients count against both their own limit AND the global limit
- Unknown clients (no `--client`) use only global limits
- AlertManager webhook automatically uses client name "alertmanager"

## Monitoring

The `/metrics` endpoint exports the following Prometheus metrics:
*   `smser_sms_sent_total`: Total SMS sent.
*   `smser_sms_stored`: Number of SMS messages stored on the SIM.
*   `smser_sms_country_total`: Total SMS sent by destination country code.
*   `smser_http_requests_total`: HTTP request counts by endpoint.
*   `smser_hourly_usage` / `smser_daily_usage`: Current global usage.
*   `smser_hourly_limit` / `smser_daily_limit`: Configured global limits.
*   `smser_client_hourly_usage{client="X"}` / `smser_client_daily_usage{client="X"}`: Per-client usage.
*   `smser_client_hourly_limit{client="X"}` / `smser_client_daily_limit{client="X"}`: Per-client limits.

## License

MIT OR Apache-2.0
