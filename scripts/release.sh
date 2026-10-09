#!/usr/bin/env bash
# Show the current / suggested version, or cut a release.
#
#   scripts/release.sh                 show versions and the suggested next one
#   scripts/release.sh 0.2.0           release v0.2.0 (asks before pushing)
#   scripts/release.sh 0.2.0 -y        same, without asking
#   scripts/release.sh 0.2.0 -n        dry run: run every check, change nothing
#
# A release bumps Cargo.toml/Cargo.lock, commits, tags vX.Y.Z and pushes the
# commit and the tag; the tag triggers .github/workflows/release.yml.
set -euo pipefail
cd "$(dirname "$0")/.."

current() { sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1; }
latest_tag() { git tag --list 'v[0-9]*' | sort -V | tail -1; }

suggest() {
  local base=${1%%-*} major minor patch
  IFS=. read -r major minor patch <<<"$base"
  echo "patch  $major.$minor.$((patch + 1))"
  echo "minor  $major.$((minor + 1)).0"
  echo "major  $((major + 1)).0.0"
}

show() {
  local cur tag range=() subjects n kind
  cur=$(current); tag=$(latest_tag)
  echo "Cargo.toml version : $cur"
  echo "Latest tag         : ${tag:-<none>}"
  [[ -n $tag ]] && range=("$tag..HEAD")
  subjects=$(git log --format=%s "${range[@]}")
  n=$(grep -c . <<<"$subjects" || true)
  echo "Commits since tag  : $n"
  [[ $n -gt 0 ]] && sed 's/^/  - /' <<<"$subjects"
  echo
  echo "Candidates (from ${tag:-Cargo.toml}):"
  suggest "${tag#v}" | sed "s/^/  /"
  # Heuristic: a commit starting with "Add"/"feat" means new functionality.
  if grep -qiE '^(add|feat)' <<<"$subjects"; then kind=minor; else kind=patch; fi
  echo
  echo "Suggested next     : $(suggest "${tag#v}" | awk -v k=$kind '$1==k{print $2}')  ($kind: ${n} commit(s), $([[ $kind = minor ]] && echo 'new functionality found' || echo 'fixes only'))"
}

release() {
  local ver=$1 yes=$2 dry=$3 tag cur last
  [[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "bad version '$ver' (want X.Y.Z or X.Y.Z-rc1)" >&2; exit 1; }
  tag="v$ver"; cur=$(current); last=$(latest_tag)
  [[ $(git branch --show-current) == main ]] || { echo "not on main" >&2; exit 1; }
  [[ -z $(git status --porcelain) ]] || { echo "working tree is not clean" >&2; exit 1; }
  git fetch -q origin main --tags
  [[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || { echo "main is not in sync with origin/main (pull or push first)" >&2; exit 1; }
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null && { echo "tag $tag already exists" >&2; exit 1; }
  if [[ -n $last && $(printf '%s\n%s\n' "${last#v}" "$ver" | sort -V | tail -1) != "$ver" ]]; then
    echo "$ver is not newer than $last" >&2; exit 1
  fi
  echo "Release $tag  (Cargo.toml $cur -> $ver, previous tag ${last:-none})"
  if [[ $dry == 1 ]]; then echo "dry run: checks passed, nothing changed"; exit 0; fi
  if [[ $yes != 1 ]]; then
    read -r -p "Bump, commit, tag and push to origin? [y/N] " a
    [[ $a == y || $a == Y ]] || { echo aborted; exit 1; }
  fi
  if [[ $cur != "$ver" ]]; then
    perl -0pi -e 's/^version = "[^"]*"/version = "'"$ver"'"/m' Cargo.toml
    cargo check --locked >/dev/null 2>&1 || cargo check >/dev/null   # refreshes Cargo.lock
    git add Cargo.toml Cargo.lock
    git commit -q -m "Release $tag"
  fi
  git tag -a "$tag" -m "$tag"
  git push origin main "$tag"
  echo "Pushed $tag. Watch the build: gh run watch  |  https://github.com/mrhihi/claude-sessions/actions"
}

ver="" yes=0 dry=0
for a in "$@"; do
  case $a in
    -y) yes=1 ;; -n) dry=1 ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) ver=${a#v} ;;
  esac
done
if [[ -z $ver ]]; then show; else release "$ver" $yes $dry; fi
