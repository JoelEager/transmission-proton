# Transmission Proton Helper
Simple tool to update [Transmission](https://github.com/transmission/transmission)'s peer port with a value from the clipboard and launch Transmission. Intended to configure and launch Transmission via Proton VPN using the "Connect and Go" profile option. Includes a log file for troubleshooting and history.

Mostly vibe coded using [Google Jules](https://jules.google.com/).

## Build Instructions
### Prerequisites
- [Rust & Cargo](https://www.rust-lang.org/tools/install)

### Building Release Binary
```bash
cargo build --release
```

The compiled executable will be available at `target/release/transmission-proton` (or `transmission-proton.exe` on Windows).

## Usage
Simply run the binary:

```bash
./target/release/transmission-proton
```

It should automatically detect the Transmission configuration path and executable for your platform. If not, the defaults can be overridden with command line options. (Run with `--help` for usage.)
