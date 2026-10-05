# Contributing to Shot

Thanks for helping out! Shot is a native macOS app written in Rust, so you'll need macOS 13+ and a recent stable Rust toolchain.

## Getting started

```sh
cargo test --workspace                 # unit tests (core + OCR)
cargo run -p shot                      # run the app from the terminal
./scripts/bundle.sh && open target/release/Shot.app
```

Running from the terminal uses the terminal's Screen Recording permission. The bundled app asks for its own. `scripts/bundle.sh` signs with the first code-signing identity in your keychain, so the permission survives rebuilds. Set `SHOT_SIGN_IDENTITY=-` for ad-hoc signing.

## Where things live

| Path | What |
|---|---|
| `crates/shot-core` | Platform-independent and fully unit-tested: annotation model, renderer, effects, Background tool, project format, history, settings, logo. **Put new logic here whenever it doesn't need AppKit.** |
| `crates/app/src/ui.rs` | The small AppKit toolkit everything uses: `View` + `ViewDelegate`, closure `Target`s, windows, menus, bars. |
| `crates/app/src/*.rs` | One module per feature: `selection`, `overlay`, `editor`, `background_panel`, `pin`, `ocr`, `history_window`, `settings_window`. |

A few rules keep the AppKit side safe:

- Views hold their delegate **weakly**; the owning window controller lives in `App` (or a module-level `thread_local`).
- Never drop a window or control from inside its own event handler. Defer with `app::after(0.0, …)` instead.
- Copy values out of a `RefCell` before calling anything that might re-enter (actions, menus, modal panels).
- Controls hold their targets weakly, so keep every `Target` in a `Targets` list owned by the window.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Keep changes small and focused, and match the surrounding style. If you change how something looks, include a screenshot in the PR. `Shot capture-to out.png` is handy for that.
