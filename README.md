# transmission-proton

Simple tool to automatically update Transmission's peer port with the port copied to clipboard (e.g. from Proton VPN) and launch Transmission.

## Features

- Reads port number directly from clipboard (or accepts `--port` CLI override)
- Updates Transmission's `settings.json` (`peer-port`)
- Launches Transmission and logs start and finish timestamps to stdout and `log.txt`

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

### Options

```
Usage: transmission-proton [OPTIONS]

Options:
      --config-dir <CONFIG_DIR>          Directory containing transmission/settings.json
      --transmission-path <PATH>        Path to Transmission executable
      --log-file <LOG_FILE>              Path to log file [default: log.txt]
      --port <PORT>                      Override port value directly instead of reading clipboard
      --skip-launch                      Skip launching Transmission
  -h, --help                             Print help
  -V, --version                          Print version
```
