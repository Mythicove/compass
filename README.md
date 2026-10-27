# Mythic Compass

**English** | [简体中文](./README.zh-CN.md)

Minecraft launcher for the Mythicove ecosystem (Tauri 2 + React frontend, Rust core): sign in to `mythic-mirror` with a per-player launcher password, preview skins with skinview3d, browse mods from `mythic-hoard`. Aimed at private groups and small communities, not a public-market competitor.

## Status

Development follows a phased plan — [Compass launcher development plan](https://docs.gov.skin/plans/compass-launcher/) (P0 → P6, targeting HMCL/PCL-class capability on Windows, macOS and Linux). Progress is tracked on the [status page](https://docs.gov.skin/plans/status/).

**It does not launch Minecraft yet.** The Rust core for native launching is in place and tested against real version data; what is missing is the command/UI wiring around it (create instance → install → launch → logs).

Implemented so far:

- **P0 — skeleton and repo hygiene**
  - All backend HTTP lives in the Rust core (`src-tauri/src/api.rs`). The webview never touches the network, so the app does not depend on Mirror's CORS configuration and the CSP allows no remote `connect-src`/`img-src` — skins are fetched in Rust and handed to the frontend as `data:` URLs.
  - Named commands only. The previous `launch(program, args)` (arbitrary command execution) and `download_file(url, dest)` (arbitrary URL to arbitrary path) are gone; there is no generic proxy command either.
  - Tokens never reach the webview: commands return a token-free account view; `accounts.json` stays in the app data directory (moves to the OS keychain in P4).
  - Sign-in uses a **player name plus that player's launcher password** ([ADR-0012](https://docs.gov.skin/architecture/adr-0012-credential-separation/)) — not the site master password.
  - Persistent settings (`compass.json`) with sanitizing and atomic writes, a documented data-directory layout (assets/libraries/versions shared globally, instances isolated), light/dark/system theme, zh-CN/en interface.
  - CI on three platforms (`cargo fmt --check`, `clippy -D warnings`, `cargo test`, typecheck, frontend build) plus on-demand packaging into draft releases.
- **P1 (in progress) — the native-launch core, all in Rust with contract tests against real data**
  - `version.rs`: version-JSON parsing, platform rule evaluation (`os.name`/`os.arch`/`os.version` regex/`features`, last matching rule wins), Maven coordinate → path, library resolution (legacy `natives`+`extract` vs. modern 4-part coordinates that ride the classpath), launch-argument construction for both `arguments` (1.13+) and `minecraftArguments` (≤1.12), and version inheritance merging.
  - `net.rs`: the single download implementation — concurrency, retries with backoff, sha1 + size verification, `Range` resume, per-host mirror mapping (BMCLAPI-compatible) with fallback to the official source, `.part` files renamed into place only after verification.
  - `assets.rs` / `meta.rs` / `install.rs`: content-addressed asset objects (deduplicated by hash), the `virtual/` layout for pre-1.8, version manifest and inheritance resolution, and an idempotent install that lands everything in the shared cache.
  - `java.rs`: runtime detection by actually running `java -version` (never trusting path names or `JAVA_HOME`), with candidate ordering and an `exact` mode for versions that need Java 8.
  - `launch.rs`: JVM args → main class → game args, Log4Shell mitigation for 1.7–1.18.1, the `logging.client.argument` that is *not* part of `arguments.jvm`, macOS `-XstartOnFirstThread` for legacy versions, natives extraction with zip-slip protection, and log pumping that reads both streams.
  - `instance.rs`: one `instance.json` per instance plus an isolated `.minecraft`, slug validation that blocks traversal.

Not yet implemented, by phase: instance creation/install/launch wiring and progress UI (**rest of P1**), instances and mod/resourcepack management (**P2**), mod loaders and modpacks (**P3**), multi-account and keychain storage (**P4**), crash diagnostics, Java auto-download and self-update (**P5**), signed three-platform packaging and release (**P6**).

The former "hand the `.mrpack` to Prism Launcher" path was deleted rather than kept next to the new one — one launch path, not two.

## Run

```bash
pnpm install
pnpm typecheck      # tsc --noEmit
pnpm tauri dev      # development
pnpm tauri build    # packaging
```

Server addresses default to `http://localhost:8080` and are editable under Settings (persisted to `compass.json` in the app data directory — Settings shows the full layout).

## Tests

```bash
cd src-tauri
cargo test                                   # 81 unit/contract tests, offline
cargo test --test install_smoke -- --ignored # real ~550MB install of 1.21.4 (network)
```

The parser and argument builder are tested against **real version JSON** committed in `src-tauri/tests/fixtures/` (1.7.10, 1.12.2, 1.16.5, 1.20.1, 1.21.4, a real Fabric inherited profile, a trimmed asset index and version manifest). Hand-written samples would turn "the format as I imagine it" into the contract.

## Layout

```
src-tauri/src/
  paths.rs      data-directory layout (global cache vs. per-instance)
  config.rs     compass.json: defaults, sanitizing, atomic write
  account.rs    signed-in account (tokens stay here, never cross into the webview)
  api.rs        typed Mirror/Hoard HTTP client (the only place that talks to our servers)
  version.rs    version JSON: rules, libraries, arguments, inheritance merge
  net.rs        the downloader (concurrency, retry, checksums, resume, mirrors)
  assets.rs     asset index: content-addressed objects, virtual layout for pre-1.8
  meta.rs       version manifest and metadata fetching
  install.rs    install planning and orchestration into the shared cache
  java.rs       Java runtime detection (probed, never assumed)
  launch.rs     command-line construction, natives extraction, process spawn, log pump
  instance.rs   instance model (instance.json + isolated .minecraft)
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
