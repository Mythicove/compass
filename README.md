# Mythic Compass

**English** | [简体中文](./README.zh-CN.md)

Minecraft launcher for the Mythicove ecosystem (Tauri 2 + React frontend, Rust core): sign in to `mythic-mirror` with a per-player launcher password, preview skins with skinview3d, browse mods from `mythic-hoard`. Aimed at private groups and small communities, not a public-market competitor.

## Status

Development follows a phased plan — [docs/plans/2026-10-27-compass-launcher.md](https://github.com/Mythicove/docs/blob/main/plans/2026-10-27-compass-launcher.md) (P0 → P6, targeting HMCL/PCL-class capability on Windows, macOS and Linux).

Implemented so far (P0 — skeleton and repo hygiene):

- **All backend HTTP lives in the Rust core** (`src-tauri/src/api.rs`). The webview never touches the network, so the app does not depend on Mirror's CORS configuration and the CSP allows no remote `connect-src`/`img-src` — skins are fetched in Rust and handed to the frontend as `data:` URLs.
- **Named commands only.** The previous `launch(program, args)` (arbitrary command execution) and `download_file(url, dest)` (arbitrary URL to arbitrary path) are gone; there is no generic proxy command either.
- **Tokens never reach the webview.** Commands return a token-free account view; `accounts.json` stays in the app data directory (moves to the OS keychain in P4).
- Sign-in uses a **player name plus that player's launcher password** ([ADR-0012](https://github.com/Mythicove/docs/blob/main/src/content/docs/architecture/adr-0012-credential-separation.md)) — not the site master password.
- Persistent settings (`compass.json`) with input sanitizing and atomic writes, a documented data-directory layout (assets/libraries/versions shared globally, instances isolated), light/dark/system theme, zh-CN/en interface.
- CI on three platforms (`cargo fmt --check`, `clippy -D warnings`, `cargo test`, typecheck, frontend build) plus on-demand packaging into draft releases.

**Not implemented: launching Minecraft itself.** The former "hand the `.mrpack` to Prism Launcher" path was deleted rather than kept next to the new one — one launch path, not two. What is still missing, by phase: native launch (piston-meta manifests, assets/libraries with checksums and resume, Java detection, launch-argument construction, authlib-injector injection, process management) is **P1**; instances and mod/resourcepack management **P2**; mod loaders and modpack installation **P3**; multi-account and keychain storage **P4**; crash diagnostics, Java auto-download and self-update **P5**; signed three-platform packaging and release **P6**.

## Run

```bash
pnpm install
pnpm typecheck      # tsc --noEmit
pnpm tauri dev      # development
pnpm tauri build    # packaging
```

Server addresses default to `http://localhost:8080` and are editable under Settings (persisted to `compass.json` in the app data directory — Settings shows the full layout).

## Layout

```
src-tauri/src/
  paths.rs      data-directory layout (global cache vs. per-instance)
  config.rs     compass.json: defaults, sanitizing, atomic write
  account.rs    signed-in account (tokens stay here, never cross into the webview)
  api.rs        typed Mirror/Hoard HTTP client (the only place that talks to the network)
  error.rs      errors serialize as { code, message }; the UI localizes by code
  commands.rs   named Tauri commands (thin layer)
  state.rs      shared state
src/
  i18n/         zh-CN + en message tables
  lib/ipc.ts    typed invoke wrappers matching the Rust commands
  views/        home / instances / mods / settings
```

## License

[Elastic License 2.0 (ELv2)](./LICENSE)

This repository was previously developed under AGPL-3.0 (no releases tagged) and relicensed to ELv2 on 2026-10-26; see [LICENSE.history](./LICENSE.history) for details.
