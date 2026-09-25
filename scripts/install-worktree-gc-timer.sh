#!/usr/bin/env bash
# Install (or remove) the hourly user job that runs scripts/worktree-gc.py.
#
# usage: scripts/install-worktree-gc-timer.sh [--repo PATH] [--uninstall]
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/install-worktree-gc-timer.sh [--repo PATH] [--uninstall]

Installs scripts/worktree-gc.py as a user-level artifact and schedules it
hourly (systemd user timer on Linux, launchd agent on macOS) against the
primary checkout of --repo (default: this script's repository):

  worktree-gc.py --repo <primary> --delete --reclaim-builds
                 --stale-age-hours 12 --idle-hours 24 --quiet

Each run logs its actions and one summary line (journalctl --user -u
tracedecay-worktree-gc, or ~/Library/Logs/tracedecay-worktree-gc.log).
Re-run after pulling to install the current script. --uninstall stops the
job and removes the units and the installed script.
EOF
}

die() {
    echo "install-worktree-gc-timer: $*" >&2
    exit 1
}

repo=""
uninstall=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo)
            [[ $# -ge 2 ]] || die "missing value for --repo"
            repo="$2"
            shift 2
            ;;
        --uninstall)
            uninstall=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

script_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_script="$script_root/scripts/worktree-gc.py"
installed="${XDG_DATA_HOME:-$HOME/.local/share}/tracedecay/worktree-gc.py"
name="tracedecay-worktree-gc"
label="dev.tracedecay.worktree-gc"

if ((uninstall == 0)); then
    [[ -f "$source_script" ]] || die "missing $source_script"
    python3="$(command -v python3)" || die "python3 not found"
    primary="$(git -C "${repo:-$script_root}" worktree list --porcelain | sed -n '1s/^worktree //p')"
    [[ "$primary" == /* ]] || die "cannot resolve the primary checkout of ${repo:-$script_root}"
    gc_args=(--repo "$primary" --delete --reclaim-builds --stale-age-hours 12 --idle-hours 24 --quiet)
    mkdir -p "$(dirname -- "$installed")"
    install -m 0755 "$source_script" "$installed"
fi

case "$(uname -s)" in
    Linux)
        unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        if ((uninstall == 1)); then
            systemctl --user disable --now "$name.timer" 2>/dev/null || true
            rm -f -- "$unit_dir/$name.service" "$unit_dir/$name.timer" "$installed"
            systemctl --user daemon-reload
            echo "removed $name.timer"
            exit 0
        fi
        mkdir -p "$unit_dir"
        exec_start="\"$python3\" \"$installed\""
        for arg in "${gc_args[@]}"; do
            exec_start+=" \"$arg\""
        done
        cat >"$unit_dir/$name.service" <<EOF
[Unit]
Description=TraceDecay worktree GC for $primary

[Service]
Type=oneshot
Environment="PATH=$PATH"
ExecStart=$exec_start
Nice=19
IOSchedulingClass=idle
MemoryMax=1G
EOF
        cat >"$unit_dir/$name.timer" <<EOF
[Unit]
Description=Hourly TraceDecay worktree GC for $primary

[Timer]
OnCalendar=hourly
RandomizedDelaySec=300
Persistent=true

[Install]
WantedBy=timers.target
EOF
        systemctl --user daemon-reload
        systemctl --user enable --now "$name.timer"
        systemctl --user list-timers "$name.timer" --no-pager
        ;;
    Darwin)
        plist="$HOME/Library/LaunchAgents/$label.plist"
        launchctl bootout "gui/$(id -u)" "$plist" 2>/dev/null || true
        if ((uninstall == 1)); then
            rm -f -- "$plist" "$installed"
            echo "removed $label"
            exit 0
        fi
        xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }
        log="$HOME/Library/Logs/$name.log"
        mkdir -p "$(dirname -- "$plist")" "$(dirname -- "$log")"
        {
            printf '<?xml version="1.0" encoding="UTF-8"?>\n'
            printf '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
            printf '<plist version="1.0"><dict>\n'
            printf '  <key>Label</key><string>%s</string>\n' "$label"
            printf '  <key>ProgramArguments</key><array>\n'
            for arg in "$python3" "$installed" "${gc_args[@]}"; do
                printf '    <string>%s</string>\n' "$(xml "$arg")"
            done
            printf '  </array>\n'
            printf '  <key>EnvironmentVariables</key><dict><key>PATH</key><string>%s</string></dict>\n' "$(xml "$PATH")"
            printf '  <key>StartInterval</key><integer>3600</integer>\n'
            printf '  <key>ProcessType</key><string>Background</string>\n'
            printf '  <key>LowPriorityIO</key><true/>\n'
            printf '  <key>Nice</key><integer>19</integer>\n'
            printf '  <key>StandardOutPath</key><string>%s</string>\n' "$(xml "$log")"
            printf '  <key>StandardErrorPath</key><string>%s</string>\n' "$(xml "$log")"
            printf '</dict></plist>\n'
        } >"$plist"
        plutil -lint "$plist" >/dev/null
        launchctl bootstrap "gui/$(id -u)" "$plist"
        echo "installed $label (every 3600s, log $log)"
        ;;
    *)
        die "unsupported OS $(uname -s); only systemd (Linux) and launchd (macOS) are supported"
        ;;
esac
