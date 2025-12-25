# smser

`smser` is a tool and server to send and receive SMS messages using a Huawei E3372 USB modem on Linux. It is designed to run on a Raspberry Pi and includes features like a REST API, Prometheus metrics, and rate limiting. It is mostly implemented using Gemini CLI under human supervision.

[![Rust](https://github.com/ruediger/smser/actions/workflows/rust.yml/badge.svg)](https://github.com/ruediger/smser/actions/workflows/rust.yml)

## Warning

There is no authentication if you run this in server mode. Anyone with access to the server can read and send SMS!

Also the code is mostly written by AI (Gemini CLI using Gemini 2.5 and 3.0 models).

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

#### Configuration & Logging

*   **Logging**: Controlled via `RUST_LOG` environment variable.
    ```bash
    RUST_LOG=info smser serve
    ```
*   **Rate Limits**: Currently hardcoded to 100/hour and 1000/day.

## Monitoring

The `/metrics` endpoint exports the following Prometheus metrics:
*   `smser_sms_sent_total`: Total SMS sent.
*   `smser_sms_country_total`: Total SMS sent by destination country code.
*   `smser_http_requests_total`: HTTP request counts by endpoint.
*   `smser_hourly_usage` / `smser_daily_usage`: Current usage against limits.

## License

MIT
