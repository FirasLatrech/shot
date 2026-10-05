# Shot

A native macOS screenshot tool in Rust, inspired by CleanShot X. It lives in the menu bar and uses AppKit directly through [`objc2`](https://github.com/madsmtm/objc2), with no web views and no Electron.

## Install

1. Download `Shot-x.y.z.dmg` from [Releases](https://github.com/FirasLatrech/shot/releases/latest).
2. Open it and drag **Shot** into **Applications**.
3. The first time, right-click Shot → **Open** (the build isn't notarized by Apple yet). Or run:
   `xattr -dr com.apple.quarantine /Applications/Shot.app`
4. Allow **Screen Recording** when asked, then reopen Shot.

## Build & run

```sh
./scripts/bundle.sh                 # builds target/release/Shot.app
open target/release/Shot.app
```

On first launch macOS asks for **Screen Recording** permission (System Settings → Privacy & Security → Screen Recording). Until it's granted, captures show only your wallpaper. `bundle.sh` signs with the first code-signing identity in your keychain, so the permission survives rebuilds; with ad-hoc signing (`SHOT_SIGN_IDENTITY=-`) you'll need to grant it again after each build.

The default shortcuts (⇧⌘3 / ⇧⌘4 / ⇧⌘5) collide with macOS's own screenshot shortcuts. Turn those off in System Settings → Keyboard → Keyboard Shortcuts → Screenshots, or pick other shortcuts in Shot → Settings → Shortcuts.

Command line (handy for scripts): `Shot area` triggers an action at launch (also `all-in-one`, `window`, `fullscreen`, `previous`, `self-timer`, `text`, `history`, `settings`). `Shot open <file>` opens an image or `.shot` project in the editor, and `Shot capture-to <file.png>` silently captures every display and exits. From a bundle, run them as `open -n Shot.app --args …`.

## What's in phase 1

| Area | Features |
|---|---|
| **Capture** | Area, window (hover to pick, with or without shadow, optional background), fullscreen (display under the cursor), previous area, self-timer, multi-display. Freeze-screen mode with crosshair and an 8× magnifier that shows the pixel color. Shift locks a square; Space switches between area and window mode; arrow keys nudge the selection. |
| **All-In-One** | Remembers the last selection. Resize handles, exact W×H fields, aspect-ratio lock, and one-click Capture / Copy / Save / Pin / Annotate / Text. |
| **Quick Access Overlay** | Thumbnail card in a configurable corner. Copy, save, annotate, pin, OCR. Drag & drop into any app, double-click to annotate, swipe to dismiss, auto-close, right-click menu, restore recently closed, hide all overlays at once, adjustable size. |
| **Annotate** | Tools: crop (aspect ratios, edge snapping), rectangle, filled rectangle, ellipse, line, arrow (standard / thin / double / curved with a draggable bend), text (7 styles, multi-line), pencil (auto-smoothed), highlighter, counter, blur (secure / smooth), randomized pixelate, spotlight. Select/move/delete/nudge, edit text again, color well with screen eyedropper plus saved palette, undo/redo, rotate, flip, combine images by drag-drop or ⌘V, pinch zoom, "Drag me", share sheet, editable `.shot` project files. |
| **Background tool** | 20 backdrops plus custom images, padding, corner radius, shadow, 9-way alignment, social aspect ratios, Auto Balance, saved presets. |
| **OCR** | On-device Vision text recognition (multi-language) plus QR/barcode reading, copied to the clipboard. |
| **Floating pins** | Always on top; drag to move, scroll/pinch to resize, ⌥-scroll for opacity, arrow keys to nudge (⇧ moves 10 px), Lock Mode for click-through. |
| **History** | Every capture is kept for 1 day / 1 week / 1 month. Filterable grid; restore, annotate, pin, copy, drag out, delete. |
| **Settings** | Save folder, PNG/JPEG, after-capture actions, sounds, capture options, overlay position/size/auto-close, recordable global shortcuts. |

## Not built yet (later phases)

- **Phase 2: Recording.** MP4/GIF screen recording with microphone and system audio, click and keystroke overlays, camera overlay. This needs ScreenCaptureKit and AVFoundation.
- **Phase 3: Video editor.** Smart zooms, cursor smoothing, motion blur, platform export presets.
- **Phase 4: Scrolling capture**, and Cloud (it needs a backend service: links, passwords, expiry, teams).
- Highlighter "smart text size detection", and Dark/Light toolbar theming polish.

## Layout

```
crates/shot-core   platform-independent and unit-tested: annotation model, renderer (tiny-skia),
                   effects, background tool, project format, history, settings
crates/app         the macOS app: menu bar, hotkeys, capture, selection UI, overlay, editor,
                   pins, OCR, history and settings windows
```

The core renders the same `Document` for the live editor and for exports, so what you see is what you get. `cargo run -p shot-core --example showcase out.png` renders every tool into one image.

```sh
cargo test --workspace      # core + OCR tests
cargo clippy --workspace --all-targets
```

## Privacy

Everything stays on your Mac. Captures, history and settings live in `~/Library/Application Support/Shot`, and text recognition runs on-device through Apple's Vision framework. Shot makes no network requests.

## Known limitations

- Screen capture uses `CGWindowListCreateImage`, which Apple has deprecated in favor of ScreenCaptureKit. It still works on macOS 13–26; moving to ScreenCaptureKit is planned together with screen recording.
- Shot's overlay windows are excluded from screen captures by design. Set `SHOT_DEV_VISIBLE=1` to include them when working on the UI.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Bug reports and PRs are welcome.

## License

[MIT](LICENSE) © 2026 Firas Latrach
