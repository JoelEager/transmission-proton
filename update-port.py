"""
Retrieve a port number from the clipboard and write it to Transmission's settings file.

Usage:
python3 update-port.py /path/to/config/directory
"""

import sys
import json
import tkinter as tk
from os.path import join

SETTINGS_FILE = "transmission/settings.json"
PORT_KEY = "peer-port"


def get_clipboard_text():
    root = tk.Tk()
    root.withdraw()  # Hide the main window
    try:
        content = root.clipboard_get()
    except tk.TclError:
        content = ""  # Empty string if clipboard is empty or non-text
    root.destroy()
    return content


def main():
    clipboard_content = get_clipboard_text()
    port = int(clipboard_content)
    assert port

    path = join(sys.argv[1], SETTINGS_FILE)
    print(f"Opening {path}")
    
    with open(path, "r", encoding="utf-8") as file:
        settings = json.load(file)
    
    settings[PORT_KEY] = port
    
    with open(path, "w", encoding="utf-8") as file:
        json.dump(settings, file, indent=4)

    print(f"Successfully updated port to {port}")


if __name__ == "__main__":
    main()
