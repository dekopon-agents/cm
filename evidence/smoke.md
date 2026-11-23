# Quota smoke

Run on 2026-11-23 UTC with Rust 1.98.1. Each command used a fresh scratch campaign and a scripted `gh` on PATH.

| Branch | cm source SHA | Command | Result |
| --- | --- | --- | --- |
| quota/conditional-reads | `a2924d881a1ffab13b49de8796a0fa904bb36665` | `/tmp/cm-quota-smoke.sh /tmp/cm-conditional-verified-bin conditional` | Pass |
| quota/quota-sample | `1481825afba1a495cf2535a4c7aa4f5a6d733ca8` | `/tmp/cm-quota-smoke.sh /workspace/cm/target/debug/cm sample` | Pass |

The stacked run exercised both changes together. GitHub REST traffic was simulated. GitHub and proxy quota accounting after deployment still needs the coordinator's two-sync check.

## Conditional reads

```text
01-smoke/example/build: launched → pr-open
launch and both sync runs exited 0
pulls.json matched byte-for-byte after the second sync
second sync accepted a 304 with deliberately invalid body bytes
200 repos/fixture/example/pulls/7
200 repos/fixture/example/commits/c7a2e0e5ef5e643632852d17d1504cecef2892d6/check-runs?per_page=100&page=1
200 repos/fixture/example/commits/c7a2e0e5ef5e643632852d17d1504cecef2892d6/status?per_page=100&page=1
304 repos/fixture/example/pulls/7 If-None-Match: W/"fixture-pr"
Last run: 2026-11-23T01:30Z
```

## Quota sample

```text
01-smoke/example/build: launched → pr-open
launch and both sync runs exited 0
pulls.json matched byte-for-byte after the second sync
second sync accepted a 304 with deliberately invalid body bytes
200 rate_limit
200 repos/fixture/example/pulls/7
200 repos/fixture/example/commits/0163c4fd566c63b572d620d24288d208f947f488/check-runs?per_page=100&page=1
200 repos/fixture/example/commits/0163c4fd566c63b572d620d24288d208f947f488/status?per_page=100&page=1
200 rate_limit
304 repos/fixture/example/pulls/7 If-None-Match: W/"fixture-pr"
200 rate_limit
Last run: 2026-11-23T01:30Z
GitHub REST core: 17/5000 used (reset 1792227600)
```

## Reproduce

Build the checked-out revision with `cargo build --locked`. Save the script below as `/tmp/cm-quota-smoke.sh` and run `sh /tmp/cm-quota-smoke.sh "$PWD/target/debug/cm" sample`. Use `conditional` as the last argument when building the conditional-only branch.

```sh
#!/bin/sh
set -eu
cm=$1
expect_sample=$2
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat >"$scratch/bin/gh" <<'GH'
#!/bin/sh
set -eu
case "$2" in
repos/fixture/example/pulls/7)
    [ "$1" = api ] && [ "$3" = --include ] || exit 90
    if [ -f "$QUOTA_FIXTURE/pull-200" ]; then
        [ "$#" = 5 ] && [ "$4" = -H ] && [ "$5" = 'If-None-Match: W/"fixture-pr"' ] || exit 91
        printf '%s\n' "304 repos/fixture/example/pulls/7 $5" >>"$QUOTA_FIXTURE/requests"
        printf 'HTTP/2.0 304 Not Modified\r\n\r\n\377'
        exit 1
    fi
    [ "$#" = 3 ] || exit 92
    touch "$QUOTA_FIXTURE/pull-200"
    printf '%s\n' '200 repos/fixture/example/pulls/7' >>"$QUOTA_FIXTURE/requests"
    printf 'HTTP/2.0 200 OK\r\nETag: W/"fixture-pr"\r\n\r\n'
    printf '{"state":"open","merged":false,"head":{"sha":"%s"},"base":{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"mergeable_state":"dirty"}\n' "$QUOTA_HEAD"
    ;;
"repos/fixture/example/commits/$QUOTA_HEAD/check-runs?per_page=100&page=1")
    printf '%s\n' "200 $2" >>"$QUOTA_FIXTURE/requests"
    printf 'HTTP/2.0 200 OK\r\nETag: "fixture-runs"\r\n\r\n'
    echo '{"total_count":1,"check_runs":[{"name":"test","status":"completed","conclusion":"success"}]}' ;;
"repos/fixture/example/commits/$QUOTA_HEAD/status?per_page=100&page=1")
    printf '%s\n' "200 $2" >>"$QUOTA_FIXTURE/requests"
    printf 'HTTP/2.0 200 OK\r\n\r\n'
    echo '{"total_count":0,"statuses":[]}' ;;
rate_limit)
    [ "$#" = 2 ] && [ "$1" = api ] || exit 93
    printf '%s\n' '200 rate_limit' >>"$QUOTA_FIXTURE/requests"
    echo '{"resources":{"core":{"used":17,"limit":5000,"reset":1792227600}}}' ;;
*) exit 94 ;;
esac
GH
chmod +x "$scratch/bin/gh"
PATH="$scratch/bin:$PATH"
QUOTA_FIXTURE="$scratch"
export PATH QUOTA_FIXTURE
campaign="$scratch/campaign"
worktree="$scratch/worktree"
git init -q "$worktree"
git -C "$worktree" -c user.name=smoke -c user.email=smoke@example.test commit -q --allow-empty -m fixture
git -C "$worktree" remote add origin https://github.com/fixture/example.git
QUOTA_HEAD=$(git -C "$worktree" rev-parse HEAD)
export QUOTA_HEAD
target="01-smoke/example/build"
url="https://github.com/fixture/example/pull/7"
printf 'Build using fixtures.\n' >"$scratch/kickoff.md"
"$cm" init "$campaign" >/dev/null
git -C "$campaign" init -q
"$cm" -C "$campaign" init "$target" --lane claude-cloud --kickoff-file "$scratch/kickoff.md" >/dev/null
"$cm" -C "$campaign" launch "$target" --coordinator session_smoke --worktree "$worktree" >"$scratch/launch"
"$cm" -C "$campaign" advance "$target" pr-open --pr "$url" --sha "$QUOTA_HEAD"
"$cm" -C "$campaign" sync >"$scratch/sync-first"
cp "$campaign/.cm/pulls.json" "$scratch/pulls-first.json"
"$cm" -C "$campaign" sync >"$scratch/sync-second"
cmp "$scratch/pulls-first.json" "$campaign/.cm/pulls.json"
python3 - "$campaign" "$scratch" "$expect_sample" <<'PY'
import json, os, pathlib, sys
campaign, fixture = map(pathlib.Path, sys.argv[1:3])
sample = sys.argv[3] == "sample"
pull = json.loads((campaign / ".cm/pulls.json").read_text())["https://github.com/fixture/example/pull/7"]
assert pull["etags"]["repos/fixture/example/pulls/7"] == 'W/"fixture-pr"'
assert pull["mergeable_state"] == "dirty"
assert pull["head"] == pull["ci_head"] == os.environ["QUOTA_HEAD"]
requests = (fixture / "requests").read_text().splitlines()
assert requests.count("200 repos/fixture/example/pulls/7") == 1
assert requests.count('304 repos/fixture/example/pulls/7 If-None-Match: W/"fixture-pr"') == 1
assert sum("/commits/" in line for line in requests) == 2
assert requests.count("200 rate_limit") == (3 if sample else 0)
card = (campaign / "CARD.md").read_text().splitlines()
stamp = "GitHub REST core: 17/5000 used (reset 1792227600)"
assert card[0].startswith("Last run: ")
assert card[1] == (stamp if sample else ""), card[:3]
assert ((fixture / "launch").read_text().splitlines().count(stamp) == 1) == sample
print("launch and both sync runs exited 0")
print("pulls.json matched byte-for-byte after the second sync")
print("second sync accepted a 304 with deliberately invalid body bytes")
print("\n".join(requests))
print("\n".join(card[:2]))
PY
```
