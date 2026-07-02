"""CLI commands for the tracedecay memory provider (`hermes tracedecay ...`)."""
import subprocess
import sys

from . import tools


def register_cli(subparser):
    """Build the ``hermes tracedecay`` argparse subcommand tree."""
    subs = subparser.add_subparsers(dest="tracedecay_command")
    subs.add_parser(
        "status",
        help="Show tracedecay status for the current project (default subcommand)",
    )
    subs.add_parser(
        "doctor",
        help="Check the tracedecay installation and Hermes integration",
    )
    curate = subs.add_parser(
        "curate",
        help="Similarity-dedup curation of the profile memory store (dry-run by default)",
    )
    curate.add_argument(
        "--apply", action="store_true", help="Apply the proposed deletions"
    )
    curate.add_argument(
        "--llm",
        action="store_true",
        help="Include the LLM-review request payload in the report",
    )


def _run(argv):
    try:
        completed = subprocess.run([tools.TRACEDECAY_BIN, *argv], check=False)
    except OSError as exc:
        print(f"tracedecay binary not runnable ({tools.TRACEDECAY_BIN}): {exc}")
        return 1
    return completed.returncode


def tracedecay_command(args):
    """Route ``hermes tracedecay`` subcommands to the tracedecay binary."""
    sub = getattr(args, "tracedecay_command", None)
    if sub == "doctor":
        code = _run(["doctor", "--agent", "hermes"])
    elif sub == "curate":
        argv = ["memory", "curate", "--path", tools.hermes_home_dir()]
        if getattr(args, "apply", False):
            argv.append("--apply")
        if getattr(args, "llm", False):
            argv.append("--llm")
        code = _run(argv)
    else:
        code = _run(["status"])
    if code:
        sys.exit(code)
