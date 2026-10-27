# Mythic Compass

**English** | [简体中文](./README.zh-CN.md)

Minecraft launcher prototype (Tauri 2 + React) plugged into the Mythicove ecosystem: Yggdrasil login, skinview3d skin preview, browsing and downloading mods/modpacks (`.mrpack`) from `mythic-hoard`. Aimed at private groups and small communities, not a public-market competitor.

## Status (prototype)

Implemented:

- Yggdrasil username/password login (against `mythic-mirror`), persistent sessions
- skinview3d 3D skin preview
- Project/version listing from `mythic-hoard`, `.mrpack` downloads
- Delegates the actual game launch to an external launcher (Prism Launcher by default) via `--import`

**Not implemented (the gap to a real "launcher")**: native Minecraft launching (Java detection, piston-meta version manifests, asset downloads, authlib-injector injection, instance management), download progress/checksums, i18n, auto-update. Currently a single-page prototype; see `docs/plans/status.md`.

## Run

```bash
pnpm install
pnpm tauri dev      # development
pnpm tauri build    # packaging
```

The API base URL defaults to `http://localhost:8080` (unified API domain) and can be overridden in the UI or `localStorage`.

## License

[Elastic License 2.0 (ELv2)](./LICENSE)

This repository was previously developed under AGPL-3.0 (no releases tagged) and relicensed to ELv2 on 2026-10-26; see [LICENSE.history](./LICENSE.history) for details.
