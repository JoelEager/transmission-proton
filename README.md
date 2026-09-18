# Transmission Proton Helper
Simple tool to update Transmission's peer port with the port copied to clipboard and launch Transmission. Intended to automatically configure and launch Transmission via Proton VPN using the "Connect and Go" profile option.

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
      --transmission-path <PATH>         Path to Transmission executable
      --log-file <LOG_FILE>              Path to log file [default: ~/.transmission-proton.log]
      --port <PORT>                      Override port value directly instead of reading clipboard
      --skip-launch                      Skip launching Transmission
  -h, --help                             Print help
  -V, --version                          Print version
```
