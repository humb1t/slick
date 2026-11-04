// The demo workspace's pictures (SLICK_DEMO=1, `--features demo`): faces,
// workspace and app marks, two shared images and the custom emoji. All drawn,
// nothing real is pictured. The faces and marks are omaslack's (lib/Demo.js),
// rasterized here so slick, which only shows bitmaps, can take them.
//
//   node assets/demo/make.mjs ../omaslack/lib/Demo.js
//
// Needs rsvg-convert and ImageMagick (`magick`). Writes the PNGs and GIFs
// next to this file; src/demo.rs embeds them.

import { readFileSync, writeFileSync, mkdtempSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { join, dirname } from "node:path"
import { tmpdir } from "node:os"
import vm from "node:vm"

const out = dirname(new URL(import.meta.url).pathname)
const src = readFileSync(process.argv[2] || "../omaslack/lib/Demo.js", "utf8").replace(/^\.pragma.*$/m, "")
const ctx = {}
vm.createContext(ctx)
vm.runInContext(src + "\nthis.acme = acme; this.side = side; this.pics = pics; this.face = face;", ctx)

const tmp = mkdtempSync(join(tmpdir(), "slick-demo-"))
function svgOf(uri) {
  return decodeURIComponent(uri.replace(/^data:image\/svg\+xml;utf8,/, ""))
}
function png(name, svg, w, h) {
  const f = join(tmp, name + ".svg")
  writeFileSync(f, svg)
  execFileSync("rsvg-convert", ["-w", String(w), "-h", String(h), "-o", join(out, name + ".png"), f])
}

// Faces, by handle.
for (const u of [...ctx.acme.users, ...ctx.side.users]) {
  const pic = u.profile.image_72
  if (!u.is_bot && pic && pic.startsWith("data:")) png("face-" + u.name, svgOf(pic), 192, 192)
}
// Marks and images.
for (const k of ["acme", "side", "shipyard", "watchtower"]) png("mark-" + k, svgOf(ctx.pics[k]), 132, 132)
png("dashboard", svgOf(ctx.pics.dashboard), 960, 600)
png("sunset", svgOf(ctx.pics.sunset), 960, 600)
// Slackbot: a speech bubble on teal.
png("mark-slackbot", '<svg xmlns="http://www.w3.org/2000/svg" width="132" height="132"><rect width="132" height="132" fill="#1d9b8f"/>'
  + '<path d="M34 36h64a10 10 0 0 1 10 10v34a10 10 0 0 1-10 10H62l-18 16v-16H34a10 10 0 0 1-10-10V46a10 10 0 0 1 10-10z" fill="#fff"/>'
  + '<circle cx="50" cy="63" r="6" fill="#1d9b8f"/><circle cx="82" cy="63" r="6" fill="#1d9b8f"/></svg>', 132, 132)

// Custom emoji. :lgtm: a green stamp; :party-blob: a blob that bounces through the hues.
png("emoji-lgtm", '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect x="2" y="12" width="60" height="40" rx="10" fill="#2eb67d"/>'
  + '<text x="32" y="39" font-family="DejaVu Sans, sans-serif" font-weight="700" font-size="19" fill="#fff" text-anchor="middle">LGTM</text></svg>', 64, 64)
const hues = ["#ff5f6d", "#ff9f43", "#ffd93d", "#6bcB77", "#4d96ff", "#9b5de5", "#f15bb5", "#ff5f6d"]
const frames = hues.map((c, i) => {
  const up = [0, -4, -7, -8, -7, -4, 0, 2][i]
  const name = "blob" + i
  png(name, '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">'
    + `<g transform="translate(0 ${up})"><path d="M32 12c14 0 24 9 24 22 0 12-9 20-24 20S8 46 8 34c0-13 10-22 24-22z" fill="${c}"/>`
    + '<ellipse cx="24" cy="32" rx="3.4" ry="4.4" fill="#231a2b"/><ellipse cx="40" cy="32" rx="3.4" ry="4.4" fill="#231a2b"/>'
    + '<path d="M25 41c4 4 10 4 14 0" fill="none" stroke="#231a2b" stroke-width="3" stroke-linecap="round"/>'
    + '<ellipse cx="22" cy="22" rx="5" ry="3" fill="#fff" opacity=".45"/></g></svg>', 64, 64)
  return join(out, name + ".png")
})
execFileSync("magick", ["-delay", "8", "-loop", "0", "-dispose", "background", ...frames, join(out, "emoji-party-blob.gif")])
execFileSync("rm", frames)
console.log("written to", out)
