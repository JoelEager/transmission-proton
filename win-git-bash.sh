#!/usr/bin/env bash
# Set the port and launch Transmission via Windows Git Bash

set -e
local this_dir="$(dirname "${BASH_SOURCE[0]}")"

# Redirect stdout and sterr to tee (which writes to file and stdout)
exec > >(tee -a "$this_dir/log.txt") 2>&1

py "$this_dir/update-port.py" $LOCALAPPDATA
/c/Program\ Files/Transmission/transmission-qt.exe

echo "Transmission done"
