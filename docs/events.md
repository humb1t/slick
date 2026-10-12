# Events and the running slick

slick tells other programs what is going on, and takes commands from them,
so you can build your own bar widget, notifier or automation on top of it.
The [Slack plugin for the Omarchy bar](https://github.com/leonavas/omarchy-slack) is built
this way, using nothing private.

There are three ways in:

| what | where | good for |
| --- | --- | --- |
| snapshot | `$XDG_RUNTIME_DIR/slick/state.json`, or `slick status --json` | bars that poll or watch a file |
| event stream | `slick events` (JSON lines) | long-running scripts |
| hooks | `~/.config/slick/hooks/<event>.d/*` | one-shot reactions (a sound, a notification) |

slick runs once per session. Running `slick` again brings the open one to the
front; `slick --new-instance` opens a second one anyway (that one publishes
nothing and takes no commands).

On Windows the snapshot lives in `%LOCALAPPDATA%\slick\run\state.json`, and
hooks are `.exe`, `.bat` or `.cmd` files.

## Snapshot

Rewritten (atomically) whenever something in it changes. Message text never
touches the disk on Linux: `$XDG_RUNTIME_DIR` is a tmpfs.

```json
{
  "v": 1,
  "running": true,
  "pid": 12345,
  "version": "0.1.3",
  "team": "Acme",
  "domain": "acme",
  "user": "Ana",
  "updated": 1760000000,
  "dnd": { "paused": true, "until": 1760003600, "snooze": true },
  "attention": 4,
  "totals": { "mentions": 2, "dms": 2, "groups": 1, "channels": 5, "channels_mentioned": 1, "channels_quiet": 2,
              "direct_messages": 3, "channel_mentions": 1, "reactions": 1 },
  "conversations": [
    {
      "id": "D0123", "kind": "dm", "name": "Bruno Lima", "user": "U0456",
      "unread": true, "mentions": 1, "latest": "1760000000.000100",
      "muted": false, "notify": "default", "quiet": false,
      "messages": [
        { "ts": "1760000000.000100", "user": "U0456", "author": "Bruno Lima",
          "text": "Can you look at the deploy?", "html": "Can you look at the <b>deploy</b>?",
          "thread_ts": null }
      ]
    }
  ],
  "reactions": [
    {
      "conversation": { "id": "C0789", "kind": "channel", "name": "deploys", "user": null },
      "message": { "ts": "1759990000.000200", "user": "U0123", "author": "Ana",
                   "text": "Deploy is out", "thread_ts": null },
      "reaction": "tada",
      "emoji": { "name": "tada", "char": "🎉", "url": null, "file": null },
      "user": { "id": "U0456", "name": "Bruno Lima" },
      "at": 1759999000
    }
  ]
}
```

- `kind` is `dm`, `group` (a group DM), `channel` or `private`.
- `text` is plain text, exactly as people wrote it: `<b>` in a message is
  those three characters. Escape it before showing it as HTML or markup
  (`notify-send` bodies, Qt rich text, a web page). `html` is the same text
  ready for that: escaped, with only the message's own bold (`<b>`), italic
  (`<i>`) and strikethrough (`<s>`) as tags, and the workspace's own emoji as
  `<img src="file://…" alt=":name:">` of their saved pictures (GIFs as they
  are; `:name:` until the picture is downloaded).
- `muted` and `notify` are the conversation's own notification preference in
  Slack: `notify` is `everything`, `mentions_dms`, `nothing` or `default`
  (following the global one). `quiet` is a channel that is muted or set to
  mentions only (or nothing); DMs are never quiet. `totals.channels_quiet`
  counts the quiet ones among `totals.channels`.
- `conversations` holds the unread ones, newest first. `messages` are the
  newest unread ones, up to 5, oldest first. They fill in a moment after the
  conversation appears, and quiet channels (below) without a mention only get
  them for the 8 most recent.
- `attention` is what slick's own tray counts: unread DMs and group DMs plus
  channels that mention you. `totals.channels` counts the other unread channels.
  `totals.direct_messages` counts unread messages in DMs and group DMs, and
  `totals.channel_mentions` the messages that mention you in channels.
- `reactions` are the reactions other people put on your messages, newest
  first, up to 20, as `reaction.added` reports them (with `at`, when it
  arrived). One leaves the list when it is taken back, after 24 hours, or when
  you read its conversation: open in slick, or `conversation.read`. `message`
  has only `ts` when slick hadn't loaded it. `totals.reactions` counts them;
  they don't count in `attention`.
- `emoji` is how to draw a reaction: `char` for a standard emoji (skin tone
  included), or, for one of the workspace's own, its picture's `url` and a
  copy of it in `file` (`emoji/<name>.<ext>` next to the snapshot, animated
  GIFs as they are). `file` is `null` until the copy is downloaded, a moment
  after the reaction first shows up; the snapshot is rewritten then.
- `dnd` is `null` when notifications are on. `snooze: true` is a pause you
  set; `false` is Slack's notification schedule (the night hours).
- The conversation open in slick only counts as unread while slick's window
  doesn't have focus.
- After `slick quit` the file says `"running": false`. After a crash it can
  be left over: `slick status --json` checks that slick really is running.

## Events

Each event is one line of JSON:

```json
{ "v": 1, "event": "message.new", "at": 1760000000, "data": { … } }
```

| event | `data` |
| --- | --- |
| `app.started` | `pid`, `version`, `team` |
| `app.stopped` | `pid` |
| `counts.changed` | `attention`, `totals` (as in the snapshot) |
| `message.new` | `conversation` (`id`, `kind`, `name`, `user`), `message` (as in the snapshot), `mention` |
| `conversation.read` | `conversation` |
| `dnd.changed` | `paused`, and `until` and `snooze` while paused |
| `reaction.added` | `conversation`, `message` (`ts`, and the rest as in the snapshot when slick has it loaded), `reaction` (`thumbsup`, without colons), `emoji` (as in the snapshot), `user` (`id`, `name`: who reacted), `mine`, `by_me` |
| `reaction.removed` | as `reaction.added` |
| `presence.changed` | `user` (`id`, `name`), `active`, `dm` (the DM with them, or `null`), `me` |

`message.new` covers messages from other people in conversations you haven't
read, as slick learns about them: at once, through Slack's real-time
websocket, or within 15 seconds while that is down. It does not
replay what was already unread when slick started. `mention` is true when the
message names you, `@here`, `@channel` or `@everyone`.

`reaction.added` and `reaction.removed` come through the websocket only:
while it is down slick sees that a conversation changed, not which reaction,
and says nothing. `mine` is true when the message is yours, `by_me` when you
reacted.

`presence.changed` covers the people in the sidebar's DMs (up to 40), and you
(`me: true`). The websocket reports them as they change; while it is down,
slick checks them every minute. Only changes are announced: not the first
reading after slick starts, nor someone who just joined or left the sidebar.
`active` is false when they are away.

```sh
# every new DM, as it arrives
slick events | jq -c 'select(.event == "message.new" and .data.conversation.kind == "dm")'
```

`slick events` waits for slick when it isn't running and reconnects when it
restarts, so it can run from your session's autostart.

## Hooks

Every executable file in `~/.config/slick/hooks/<event>.d/` runs for that
event, with the event line on stdin and its name in `$SLICK_EVENT`. They run in
the background, in name order, and nothing waits for them.

```sh
#!/bin/sh
# ~/.config/slick/hooks/message.new.d/notify-mentions
jq -e '.data.mention' >/dev/null || exit 0
notify-send "slick" "You were mentioned"
```

```sh
#!/bin/sh
# ~/.config/slick/hooks/reaction.added.d/notify-reactions
ev=$(cat)
echo "$ev" | jq -e '.data.mine and (.data.by_me | not)' >/dev/null || exit 0
notify-send "slick" "$(echo "$ev" | jq -r '"\(.data.user.name) reacted :\(.data.reaction): in \(.data.conversation.name)"')"
```

```sh
#!/bin/sh
# ~/.config/slick/hooks/message.new.d/dm-sound
jq -e '.data.conversation.kind == "dm"' >/dev/null && pw-play /usr/share/sounds/freedesktop/stereo/message.oga
```

## Commands

```sh
slick open <conversation> [ts] [--thread <ts>] [--background]
slick mark-read <conversation>
slick dnd pause <minutes> | resume | status
slick quit
slick status [--json]
slick events
```

`slick open` shows the conversation, scrolled to the message `ts` (inside its
thread with `--thread`), and brings the window to the front (Hyprland) unless
`--background` is given. With slick closed it opens it, in a new terminal
window (`xdg-terminal-exec`) when not run from one. `slick dnd` works with
slick closed too, using the saved session.

Underneath, these write JSON lines to the socket at
`$XDG_RUNTIME_DIR/slick/slick.sock`, which you can also do directly:

```sh
echo '{"cmd":"open","conv":"C0123","ts":"1760000000.000100"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/slick/slick.sock
```

Commands: `focus`, `open` (`conv`, `ts`, `thread_ts`, `focus`), `mark-read`
(`conv`), `dnd.pause` (`minutes`), `dnd.resume`, `quit`, and `subscribe`, which
turns the connection into the event stream.
