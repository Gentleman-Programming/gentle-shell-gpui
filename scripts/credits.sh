#!/bin/sh
# Regenerates CREDITS.md, which Configuration › About embeds, from the whole
# git history: every commit author once, bots left out, sorted by name.
# `.mailmap` merges one person's names and addresses and maps them to their
# GitHub noreply address, which is where the @login comes from.
#
# Run from a full clone (a shallow one would drop people): scripts/credits.sh
set -eu

cd "$(git rev-parse --show-toplevel)"
if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
    echo "credits: shallow clone; run 'git fetch --unshallow' first" >&2
    exit 1
fi

people=$(
    git log --use-mailmap --format='%aN%x09%aE' HEAD |
        awk -F '\t' '
            $1 ~ /\[bot\]$/ || $2 ~ /\[bot\]@/ { next }
            {
                login = ""
                if ($2 ~ /@users\.noreply\.github\.com$/) {
                    login = substr($2, 1, index($2, "@") - 1)
                    sub(/^[0-9]+\+/, "", login)
                }
                key = login != "" ? tolower(login) : tolower($1)
                if (key in seen) next
                seen[key] = 1
                print (login != "" ? "- " $1 " (@" login ")" : "- " $1)
            }' |
        LC_ALL=C sort -f
)
if [ -z "$people" ]; then
    echo "credits: no authors found" >&2
    exit 1
fi

cat >CREDITS.md <<EOF
# Credits

Everyone who has authored a commit in this repository. Configuration › About shows this list in the app, built in.

Generated from the git history by \`scripts/credits.sh\` (bots left out; \`.mailmap\` merges a person's names and addresses and links them to their GitHub account). Don't edit it by hand: add a \`.mailmap\` line if needed, run the script from a full clone and commit the result.

## Contributors

$people
EOF
