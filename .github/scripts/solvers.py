#!/usr/bin/env python3
"""The solvers list: who can merge a pull request with `/solve`.

`.github/solvers.txt` holds one GitHub login per line. The Solvers workflow
runs this script from main's copy, never a pull request's:

- `solve`: someone wrote `/solve` on a PR. It merges the PR when the commenter
  is on the list and every check below passes; otherwise it says why.
- `edit`: adds or removes a login, committing to the default branch. Only the
  owners of `/.github/` in CODEOWNERS can.
- `guard`: closes open PRs that change the list; it's edited only by `edit`.

`solve` merges only when:

- the commenter isn't the PR's author, and the PR isn't a Draft;
- the commenter's latest review approves the PR's current commit, so the merge
  is the code they read (`--match-head-commit` pins it);
- every check finished green, and GitHub says it merges cleanly;
- it touches none of the sensitive paths in `pr_risk_report.py` (CI, build
  scripts, dependencies, packaging, the updater...): those need a Code Owner.

Merges and edits to the list use the repository owner's token ($OWNER_TOKEN,
the `SOLVE_TOKEN` secret), as if the owner had done them: the Actions token
can't get past the approvals `main` asks for, and a merge with it wouldn't
start CI on `main`. Comments, reads and closing PRs use the Actions token
($GH_TOKEN), so they show up as the bot.

Input comes from environment variables (see each command), never from argv
built with `${{ }}`, so a comment can't inject anything.

Usage: solvers.py solve|edit|guard
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pr_risk_report import SENSITIVE_PATHS

LIST = ".github/solvers.txt"
OWNED = "/.github/"
VERDICTS = {"APPROVED", "CHANGES_REQUESTED", "DISMISSED"}
GREEN = {"SUCCESS", "NEUTRAL", "SKIPPED"}
FIELDS = "number,author,isDraft,headRefOid,baseRefName,mergeable,reviews,statusCheckRollup,files,changedFiles"


def gh(*args: str, as_owner: bool = False) -> str:
    """Runs `gh` against $GH_REPO and returns stdout; `as_owner` uses $OWNER_TOKEN instead of $GH_TOKEN."""
    env = None
    if as_owner:
        if not os.environ.get("OWNER_TOKEN"):
            raise SystemExit("::error::The SOLVE_TOKEN secret isn't set (docs/security.md#repository-settings-admins).")
        env = {**os.environ, "GH_TOKEN": os.environ["OWNER_TOKEN"]}
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True, env=env).stdout


def logins(text: str) -> list[str]:
    """The logins in a solvers list: no comments, blanks or leading `@`."""
    return [line.strip().lstrip("@") for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")]


def render(text: str, names: list[str]) -> str:
    """`text`'s leading comment block followed by `names`, sorted."""
    header = []
    for line in text.splitlines():
        if not line.lstrip().startswith("#"):
            break
        header.append(line)
    return "\n".join([*header, *sorted(names, key=str.lower)]) + "\n"


def owners(codeowners: str, pattern: str = OWNED) -> list[str]:
    """User logins owning `pattern` in a CODEOWNERS file (the last matching line wins; teams don't count)."""
    found: list[str] = []
    for line in codeowners.splitlines():
        parts = line.split("#", 1)[0].split()
        if parts and parts[0] == pattern:
            found = [p[1:] for p in parts[1:] if p.startswith("@") and "/" not in p]
    return found


def has(names: list[str], login: str) -> bool:
    return login.lower() in (n.lower() for n in names)


def blockers(pr: dict, solver: str, default_branch: str) -> list[str]:
    """Why `solver`'s `/solve` can't merge `pr`; empty when it can."""
    reasons: list[str] = []
    head = pr["headRefOid"]
    if pr["author"]["login"].lower() == solver.lower():
        return ["you can't solve your own PR: someone else on the solvers list reviews it"]
    if pr["isDraft"]:
        reasons.append("it's still a Draft")
    if pr["baseRefName"] != default_branch:
        reasons.append(f"it targets `{pr['baseRefName']}`, not `{default_branch}`")

    verdict = None
    for review in pr["reviews"]:
        login = (review.get("author") or {}).get("login") or ""
        if login.lower() == solver.lower() and review["state"] in VERDICTS:
            verdict = review
    if verdict is None or verdict["state"] != "APPROVED":
        reasons.append("approve it first (Files changed → Review changes → Approve), then write `/solve`")
    elif (verdict.get("commit") or {}).get("oid") != head:
        reasons.append(f"your approval is for an older commit; review the changes up to {head[:7]} and approve again")

    checks = pr["statusCheckRollup"]
    if not checks:
        reasons.append("no checks ran (a first-time contributor's run may need a maintainer's approval)")
    pending, failing = [], []
    for check in checks:
        name = check.get("name") or check.get("context") or "?"
        if check["__typename"] == "CheckRun":
            if check["status"] != "COMPLETED":
                pending.append(name)
            elif check["conclusion"] not in GREEN:
                failing.append(name)
        elif check.get("state") in ("PENDING", "EXPECTED"):
            pending.append(name)
        elif check.get("state") != "SUCCESS":
            failing.append(name)
    if pending:
        reasons.append("checks still running: " + ", ".join(pending) + "; write `/solve` again when they're green")
    if failing:
        reasons.append("checks failing: " + ", ".join(failing))

    if pr["mergeable"] == "CONFLICTING":
        reasons.append(f"it conflicts with `{default_branch}`")
    elif pr["mergeable"] != "MERGEABLE":
        reasons.append("GitHub hasn't worked out whether it merges cleanly yet; write `/solve` again in a minute")

    if pr["changedFiles"] > len(pr["files"]):
        reasons.append(f"it changes {pr['changedFiles']} files, too many to check here; it needs a Code Owner")
    sensitive = sorted({f["path"] for f in pr["files"] if any(rx.search(f["path"]) for rx, _ in SENSITIVE_PATHS)})
    if sensitive:
        reasons.append("it touches sensitive paths, so a Code Owner merges it: " + ", ".join(f"`{p}`" for p in sensitive))
    return reasons


def solve() -> int:
    """Env: PR, SOLVER, BODY, DEFAULT_BRANCH. Reads the list from the checked-out default branch."""
    pr_number, solver = os.environ["PR"], os.environ["SOLVER"]
    command = (os.environ["BODY"].splitlines() or [""])[0].split()
    if not command or command[0] != "/solve":
        return 0

    def say(text: str) -> None:
        gh("pr", "comment", pr_number, "--body", text)

    with open(LIST, encoding="utf-8") as f:
        if not has(logins(f.read()), solver):
            say(f"@{solver}, only people on the solvers list (`{LIST}`) can merge with `/solve`. Your review still counts: someone on the list takes it from there.")
            return 0

    pr = json.loads(gh("pr", "view", pr_number, "--json", FIELDS))
    for _ in range(5):  # GitHub computes mergeability lazily.
        if pr["mergeable"] != "UNKNOWN":
            break
        time.sleep(3)
        pr = json.loads(gh("pr", "view", pr_number, "--json", FIELDS))

    reasons = blockers(pr, solver, os.environ["DEFAULT_BRANCH"])
    if reasons:
        say(f"@{solver}, not merging yet:\n\n" + "\n".join(f"- {r}" for r in reasons))
        return 0

    head = pr["headRefOid"]
    if not os.environ.get("OWNER_TOKEN"):
        say(f"@{solver}, everything checks out, but `/solve` isn't set up to merge yet (no `SOLVE_TOKEN`); a maintainer will merge it.")
        return 1
    try:
        # `--admin` gets past the approval `main` asks for, like the owner's own
        # Merge button; this script already checked CI and the approval.
        gh("pr", "merge", pr_number, "--merge", "--admin", "--match-head-commit", head, as_owner=True)
    except subprocess.CalledProcessError as err:
        say(f"@{solver}, GitHub refused the merge: {err.stderr.strip()}")
        return 1
    say(f"Merged {head[:7]}, approved by @{solver} with `/solve`.")
    return 0


def edit() -> int:
    """Env: ACTION (add|remove), LOGIN, ACTOR, DEFAULT_BRANCH."""
    action, actor, branch = os.environ["ACTION"], os.environ["ACTOR"], os.environ["DEFAULT_BRANCH"]
    login = os.environ["LOGIN"].strip().lstrip("@")

    codeowners = base64.b64decode(gh("api", f"repos/{{owner}}/{{repo}}/contents/.github/CODEOWNERS?ref={branch}", "--jq", ".content")).decode()
    allowed = owners(codeowners)
    if not has(allowed, actor):
        print(f"::error::Only the owners of {OWNED} in CODEOWNERS ({', '.join('@' + a for a in allowed) or 'nobody'}) can edit the solvers list.")
        return 1

    current = json.loads(gh("api", f"repos/{{owner}}/{{repo}}/contents/{LIST}?ref={branch}"))
    text = base64.b64decode(current["content"]).decode()
    names = logins(text)
    if action == "add":
        try:
            login = gh("api", f"users/{login}", "--jq", ".login").strip()
        except subprocess.CalledProcessError:
            print(f"::error::GitHub has no user @{login}.")
            return 1
        if has(names, login):
            print(f"@{login} is already on the list.")
            return 0
        names.append(login)
    else:
        if not has(names, login):
            print(f"@{login} isn't on the list.")
            return 0
        names = [n for n in names if n.lower() != login.lower()]

    gh(  # as the owner: a commit straight to the default branch.
        "api", "--method", "PUT", f"repos/{{owner}}/{{repo}}/contents/{LIST}",
        "-f", f"message=chore(solvers): {action} @{login}\n\nRequested by @{actor} with the Solvers workflow.",
        "-f", "content=" + base64.b64encode(render(text, names).encode()).decode(),
        "-f", f"sha={current['sha']}",
        "-f", f"branch={branch}", as_owner=True,
    )  # fmt: skip
    print(f"{'Added' if action == 'add' else 'Removed'} @{login}.")
    return 0


def guard() -> int:
    """Closes every open PR that changes the solvers list."""
    prs = json.loads(gh("pr", "list", "--state", "open", "--limit", "500", "--json", "number,files,changedFiles"))
    for pr in prs:
        paths = [f["path"] for f in pr["files"]]
        if pr["changedFiles"] > len(paths):
            files = gh("api", "--paginate", f"repos/{{owner}}/{{repo}}/pulls/{pr['number']}/files", "--jq", ".[].filename")
            paths = files.split()
        if LIST in paths:
            gh(
                "pr", "close", str(pr["number"]), "--comment",
                f"This PR changes `{LIST}`, which isn't edited in pull requests: the repository owner manages it with the Solvers workflow. Closing it; open a new PR without that file.",
            )  # fmt: skip
            print(f"Closed #{pr['number']}.")
    return 0


def main() -> int:
    commands = {"solve": solve, "edit": edit, "guard": guard}
    if len(sys.argv) != 2 or sys.argv[1] not in commands:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    return commands[sys.argv[1]]()


if __name__ == "__main__":
    sys.exit(main())
