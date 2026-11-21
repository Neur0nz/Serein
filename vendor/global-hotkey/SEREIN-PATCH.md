# X11 shortcut registration

Source is the published [global-hotkey 0.8.0 crate](https://crates.io/crates/global-hotkey/0.8.0)
from tauri-apps/global-hotkey at `2a620bf3852008b568f6d36c2baedcc3dd0822f2`.
The registry archive SHA-256 is
`8c386b0a4a70cb2d39fffd74480f985b6f0bfbcb934b6a6b6b7e630e448f242e`.
The upstream license texts and original manifest are included. Standalone examples
and their development dependencies are omitted from the build manifest.

The X11 worker waits for registration commands with a 50 ms timeout. A caller
waiting for a registration response can wake the worker immediately. When no
command arrives, the timeout still wakes it to check for X11 key events. The
same platform APIs and per-shortcut results apply.

Remove the patch when an upstream release serves X11 worker commands without
waiting for the next input poll.
