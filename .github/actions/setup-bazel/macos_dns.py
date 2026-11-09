"""Preserve the Ethernet settings changed by the Tailscale macOS action."""

import json
import os
import subprocess
import sys


# networksetup returns one line per entry, or a sentence when DHCP supplies it.
SETTINGS = {"dnsservers": "DNS Servers", "searchdomains": "Search Domains"}


def read_settings(service):
    settings = {}
    for option, label in SETTINGS.items():
        value = subprocess.check_output(
            ["networksetup", "-get" + option, service],
            text=True,
            env={**os.environ, "LC_ALL": "C"},
        ).strip()
        if not value:
            raise ValueError(f"networksetup returned no result for {option}")
        automatic = f"There aren't any {label} set on {service}."
        settings[option] = [] if value == automatic else value.splitlines()
    return settings


def restore_settings(service, settings):
    for option in SETTINGS:
        subprocess.run(
            ["networksetup", "-set" + option, service, *(settings[option] or ["Empty"])],
            check=True,
        )


if __name__ == "__main__":
    if sys.argv[1:] == ["read"]:
        print("settings=" + json.dumps(read_settings("Ethernet")))
    elif sys.argv[1:] == ["restore"]:
        restore_settings("Ethernet", json.loads(os.environ["ETHERNET_DNS"]))
    else:
        sys.exit("expected read or restore")
