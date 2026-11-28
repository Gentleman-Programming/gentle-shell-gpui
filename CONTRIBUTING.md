# Contributing

Many people work on this repository at once. These rules keep two people from doing the same issue and make it clear which pull requests are ready. Read [`AGENTS.md`](AGENTS.md) before your first PR: it says where code goes and the design rules.

## Before you start an issue

- **Check it isn't assigned.** Someone under "Assignees" (and the `in progress` label) is already working on it.
- **Comment `/take`** on the issue: a bot assigns it to you and labels it `in progress`. From then on it's yours. If it's already taken, the bot says by whom.
- **Can't continue?** Comment `/drop` and the issue is free again.
- **No progress in 14 days** (no PR, no comment) frees the issue for someone else; a maintainer releases it.
- **Something without an issue?** Open one first and describe what you propose, especially for a new feature or a design change, so we agree on it before you spend time.

## Opening your pull request

- **Open it as a Draft** while you're still working. Draft means "in progress, don't review yet".
- **Link the issue** with `Closes #123` in the description: the issue shows there's a PR, and it closes when the PR merges.
- **Mark it "Ready for review"** only when:
  - [ ] `cargo fmt` is applied.
  - [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` passes.
  - [ ] `cargo test` passes.
  - [ ] You tried it for real, in an isolated copy ([`docs/development.md`](docs/development.md#isolated-testing)).
  - [ ] Visual changes have screenshots in the PR.
  - [ ] The PR says which systems you tried it on (macOS, Windows, Linux).

## What a PR's state means

|State|Meaning|
|---|---|
|Draft|In progress; not reviewed yet.|
|Ready for review|Ready for anyone to review: a maintainer or another contributor.|
|Changes requested|There are comments to address. Reply in the PR when they're done so we look again.|
|Needs testing on Windows / Linux|The code is fine; someone with that system has to try it. If you have it, your help counts.|
|Approved and `/solve`|Someone on the solvers list approved it and wrote `/solve`: a bot merges it into `main`.|

## Reviewing a pull request

Anyone can review a PR that's Ready for review, not only maintainers:

1. **Try it for real:** a release build (`cargo build --release`) in an isolated copy ([`docs/development.md`](docs/development.md#isolated-testing)), never on the app you use.
2. **Go through what the PR says it does**, and run `cargo test` on your system.
3. **Write what you found** in the PR: the system you used (macOS, Windows, Linux), what works and what doesn't.
4. **Decide:**
   - Something has to change: **Files changed → Review changes → Request changes**, and say what.
   - Small things for later: say them in your comment, and open an issue if they're worth it.
   - It's ready: **Files changed → Review changes → Approve**.

### Merging with `/solve`

People on the solvers list ([`.github/solvers.txt`](.github/solvers.txt)) merge what they review, without write access to the repository: after approving, they comment `/solve` and a bot merges the PR into `main` on the owner's behalf, right away. It merges only when:

- whoever writes `/solve` is on the list and isn't the PR's author;
- their latest review approves the PR's current commit (if the author pushes again, review the new commits and approve again);
- it isn't a Draft, every check is green and it merges cleanly;
- it doesn't touch sensitive paths (CI, `Cargo.*`, `build.rs`, `packaging/`, the updater, `setup/`, `pi-rpc/`, scripts): those wait for a Code Owner.

Otherwise the bot answers with what's missing; fix it and write `/solve` again. If you're not on the list, your review still counts: someone on it takes it from there.

Only the repository owner edits the list, from **Actions → Solvers → Run workflow** (add or remove, and the login). It can't change in a pull request: a PR that touches `.github/solvers.txt` is closed automatically.

### For maintainers: where to look

GitHub keeps each PR's state itself, so the pull request list filters by it:

|To see…|Filter|
|---|---|
|Ready and waiting for review|`is:pr is:open draft:false review:none`|
|Changes requested (the author's turn)|`is:pr is:open review:changes_requested`|
|Approved (ready to merge)|`is:pr is:open review:approved`|
|In progress|`is:pr is:open draft:true`|
|Waiting for someone to test on Windows or Linux|`is:pr is:open label:"needs testing: windows"` (or `linux`)|

The testing labels are the only state GitHub doesn't track: a maintainer sets them. A bot can't label fork PRs without `pull_request_target`, which [`docs/security.md`](docs/security.md) rules out.

## Project rules

- **One PR, one topic.** A bug found along the way goes in its own PR ([ADR 0003](docs/adr/0003-behavior-preserving-refactors.md)).
- **Changes to `.github/`, `Cargo.*`, `build.rs`, `packaging/` or the updater** need a Code Owner's review ([`docs/security.md`](docs/security.md)).
- **Never test on the app you're using:** use an isolated copy so your data stays safe ([`AGENTS.md`](AGENTS.md#testing-without-breaking-anything)).
