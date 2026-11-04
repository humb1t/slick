#!/usr/bin/env bash
# One screenshot of the demo workspace (src/demo.rs), taken off screen: a
# headless Hyprland output, Ghostty on it running `slick` with SLICK_DEMO=1,
# grim on the window once the script says `ready`.
#
#   scripts/demo-shot.sh <out.png> "<SLICK_DEMO_SCRIPT>" [theme] [cols x rows px]
#
# Build first: cargo build --release --features demo
# The headless output stays for the next shot; `hyprctl output remove SLICKDEMO` drops it.
set -euo pipefail
out=$1
steps=${2:-ready}
theme=${3:-Gruvbox Dark}
size=${4:-1600x940}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${SLICK_BIN:-$root/target/release/slick}
mon=SLICKDEMO
w=${size%x*}; h=${size#*x}

if ! hyprctl monitors all -j | grep -q "\"$mon\""; then
  hyprctl output create headless "$mon" >/dev/null
  sleep 0.5
fi
# Logical size w x h, drawn at 2x for a crisp picture.
hyprctl dispatch "function() hl.monitor({ output = \"$mon\", mode = \"$((w * 2))x$((h * 2))@60\", position = \"auto-right\", scale = 2 }) end" >/dev/null
# Its windows go to that output whatever is focused, opaque and fullscreen
# (once per Hyprland session: the rule stays until a reload).
if [ ! -f "${XDG_RUNTIME_DIR:-/tmp}/slick-shot-rule.$(hyprctl instances -j | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["instance"])')" ]; then
  hyprctl dispatch 'function() hl.window_rule({ match = { class = "^slick\\.shot\\..*" }, monitor = "SLICKDEMO", fullscreen = true, opacity = "1 1", tag = "-default-opacity", no_anim = true }) end' >/dev/null
  touch "${XDG_RUNTIME_DIR:-/tmp}/slick-shot-rule.$(hyprctl instances -j | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["instance"])')"
fi
sleep 0.3

demo=$(mktemp -d /tmp/slick-demo-XXXX)
trap 'pkill -f "SLICK_DEMO_DIR=$demo/state" || true; rm -rf "$demo"' EXIT
class="slick.shot.p$$"
cat > "$demo/run.sh" <<RUN
#!/bin/sh
exec ghostty --config-default-files=false --gtk-single-instance=false --class=$class \\
  --theme="$theme" --font-family="Maple Mono NF" --font-size=${FONT_SIZE:-12} \\
  --window-padding-x=14 --window-padding-y=12 --window-decoration=false --cursor-style-blink=false \\
  --confirm-close-surface=false --resize-overlay=never \\
  -e env SLICK_DEMO=1 SLICK_DEMO_DIR=$demo/state SLICK_DEMO_SCRIPT="$steps" LANG=${SHOT_LANG:-en_US.UTF-8} $bin
RUN
chmod +x "$demo/run.sh"
hyprctl dispatch "hl.dsp.exec_cmd(\"$demo/run.sh\")" >/dev/null

for _ in $(seq 1 300); do
  [ -f "$demo/state/ready" ] && break
  sleep 0.1
done
if [ ! -f "$demo/state/ready" ]; then
  echo "no ready after 30s" >&2
  exit 1
fi
sleep ${SETTLE:-0.8}
hyprctl monitors all -j > "$demo/mons"
id=$(hyprctl clients -j | python3 -c "
import json,sys
c=[c for c in json.load(sys.stdin) if c['class']=='$class'][0]
m=[m for m in json.load(open('$demo/mons')) if m['name']=='$mon'][0]
assert c['monitor'] == m['id'] and c['workspace']['id'] == m['activeWorkspace']['id'], 'window not on screen in $mon'
print(c['stableId'])")
[ -n "$id" ]
# The window itself, without the compositor's cursor.
grim -T "$id" "$out"
echo "$out"
