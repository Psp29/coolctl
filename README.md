# coolctl

A daemon and GTK4/libadwaita-free GUI for driving the small LCD panel built into DeepCool AIO
liquid coolers (starting with the LM360, USB VID:PID `3633:0026`). Replaces/extends the
Python `deepcool-lm` AUR script with a Rust daemon and a proper editor for what gets shown on
the panel.

Not scoped to the LM360 specifically in naming, even though it's the only hardware currently
supported — `coolctl` is meant to grow to other DeepCool AIO panels sharing the same protocol
shape.

## What it does

- **Monitor mode**: live CPU temp/usage/clock and GPU temp/usage/clock rendered to the panel,
  refreshed continuously. Sensor selection handles the common AMD quirks (`coretemp` →
  `k10temp` → `zenpower` fallback, discrete-vs-integrated GPU disambiguation via `fan1_input`).
- **Media mode**: crop and send a static image, an animated GIF (including animated WebP served
  under a `.gif` filename — Giphy/Discord do this), or a trimmed segment of a video to the
  panel. The GUI's Media Editor has a single live pan/zoom crop viewport that plays video/GIF
  content directly while you crop it, with a progress popup (real per-frame progress and a
  Cancel button) for exporting.
- **Orientation**: fixed 90° steps (0/90/180/270), shared live between the daemon and both GUI
  tabs — content is pre-rotated to compensate for however the pump is physically mounted in the
  case.
- **Brightness** up/down, and full persistence — mode, orientation, and media content all
  survive a daemon restart.

## Architecture

A Cargo workspace, three crates:

- `coolctl-core` — shared types: the `Request`/`Response` IPC protocol, `Orientation`, panel
  dimensions, and the RGB565 buffer rotation used for orientation.
- `coolctld` — the daemon. Talks to the panel over USB (`rusb`, bulk transfer, no HID reports),
  renders Monitor/Solid/Media frames, and serves a Unix socket (`/run/coolctl.sock`, one JSON
  request per connection — `SetMedia` is the one exception, a JSON header line followed by raw
  RGB565 frame bytes). State persists to `/etc/coolctld/config.toml`; media content persists
  separately to `/etc/coolctld/media/` since it can be large.
- `coolctl-gui` — the GTK4 GUI client. Talks to `coolctld` over the same Unix socket, never
  touches the USB device directly.

## Requirements

Arch/CachyOS package names (this project develops and ships against Arch's GStreamer/GTK4
packaging):

- Runtime: `gtk4`, `gstreamer`, `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`,
  `libusb`. Optionally `gst-libav` for broader video codec coverage.
- Build: `rust`, `cargo`.

The daemon needs to open the USB device directly (no udev rule / non-root access has been set
up yet), so `coolctld` runs as `root` via systemd.

## Build & install

The primary path is the included `PKGBUILD`, which builds straight from this working tree (no
separate source download):

```sh
makepkg -si
```

This installs `coolctld` and `coolctl-gui` to `/usr/bin/`, a systemd unit to
`/usr/lib/systemd/system/coolctl.service`, and a `.desktop` entry. Then enable and start the
daemon:

```sh
sudo systemctl enable --now coolctl.service
```

For plain development builds without packaging:

```sh
cargo build --release --workspace
```

## Using the GUI

Launch `coolctl-gui` (or find "coolctl" in your application launcher). Two tabs:

- **Media Editor**: pick "Images" or "GIF / Video", Open a file, pan/zoom the crop viewport to
  frame it, then Export & Send. Video needs an In/Out range of at least 5 seconds, set while it
  plays.
- **Monitor**: switch the panel to live Monitor mode, adjust brightness, check daemon status.

The orientation selector above the tabs is shared — it affects both the live panel and the
Media Editor's crop aspect ratio immediately.

## Development notes

- `cargo check --workspace` / `cargo build --release --workspace` — no crate-specific quirks
  beyond the version pins already in each `Cargo.toml` (notably `gtk4`/`cairo-rs` need to
  resolve to compatible versions — check `cargo check` output for duplicate copies of a crate
  if you touch those pins).
- `coolctl-gui/examples/` holds offline verification scripts (`crop_spike.rs`,
  `verify_export.rs`, `verify_video.rs`) used to check rendering/export/video-extraction
  correctness against saved PNGs before testing on real hardware — run with `cargo run
  --example <name> -p coolctl-gui`. They expect fixture files in a scratch directory referenced
  at the top of each file; regenerate those per the comments there if needed.
- No license has been chosen yet.
