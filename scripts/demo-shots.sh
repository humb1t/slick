#!/usr/bin/env bash
# Every screenshot of the README and the landing, from the demo workspace
# (src/demo.rs), taken off screen by scripts/demo-shot.sh.
#
#   scripts/demo-shots.sh            # all of them
#   scripts/demo-shots.sh main react # some
#
# PNGs go to docs/screenshots, WebPs (for the landing) to landing/public/screens.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build --release --features demo
shot=scripts/demo-shot.sh
png=docs/screenshots
web=landing/public/screens
mkdir -p "$png" "$web"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Opens #engineering; the steps after it run once it's on screen.
eng="sleep:2000 ctrl+k type:engineering sleep:500 enter sleep:1800"
# #engineering with the review's thread open, scrolled up to the dashboard.
main="$eng up up up up up up sleep:300 t sleep:1000 wheel-up:60,30 wheel-up:60,30 wheel-up:60,30 sleep:1200 ready"

declare -A steps=(
  [main]="$main"
  [search]="sleep:2000 ctrl+k type:deploy sleep:2500 ready"
  [activity]="sleep:2000 ctrl+shift+m sleep:2500 ready"
  [threads]="sleep:2000 ctrl+shift+t sleep:2500 ready"
  [react]="$eng up up up sleep:300 r sleep:400 type:part sleep:1500 ready"
  [compose]="$eng wheel-up:60,30 wheel-up:60,30 sleep:400 type:Rolled_back_the_gateway,_p99_is_back_to_180ms_:par sleep:1500 ready"
  [profile]="sleep:2000 ctrl+k type:priya sleep:500 enter sleep:1200 alt+i sleep:2000 ready"
  [workspaces]="sleep:2500 click:5,0 sleep:1200 ready"
)
names=("$@")
[ ${#names[@]} -gt 0 ] || names=(main search activity threads react compose profile workspaces)

webp() { magick "$1" -resize "${3:-75%}" -quality 84 -define webp:method=6 "$2"; }

for n in "${names[@]}"; do
  $shot "$png/$n.png" "${steps[$n]}" >/dev/null
  # The features gallery at 1600px.
  webp "$png/$n.png" "$web/$n.webp" 50%
  # The hero: the main window without the thread pane, readable at its size.
  if [ "$n" = main ]; then
    magick "$png/main.png" -crop 2128x1880+0+0 +repage "$tmp/hero.png"
    webp "$tmp/hero.png" "$web/hero.webp" 75%
  fi
  echo "$n"
done
