# Mythic Compass

**English** | [简体中文](./README.zh-CN.md)

A Minecraft launcher prototype (Tauri 2 + React) plugged into the Mythicove ecosystem: Yggdrasil login, skinview3d skin preview, and browsing plus downloading mods and modpacks (.mrpack) from mythic-hoard. It targets private groups and small communities, not the public market.

## Status (prototype)

Implemented:

- Yggdrasil username and password login against mythic-mirror, with persistent sessions
- skinview3d 3D skin preview
- Project and version listing from mythic-hoard, with .mrpack downloads
- Handing the actual game launch to an external launcher (Prism Launcher by default) via --import

Still missing: native Minecraft launching (Java detection, piston-meta version manifests, asset downloads, authlib-injector injection, instance management), download progress and checksums, i18n and auto-update. It is a single-page prototype today; see the [status page](https://docs.gov.skin/plans/status/).

## Run

```bash
pnpm install
pnpm tauri dev      # development
pnpm tauri build    # packaging
```

The API base URL defaults to http://localhost:8080 (the unified API domain) and can be overridden in the UI or in localStorage.

## License

[Elastic License 2.0 (ELv2)](./LICENSE) — source-available, not OSI open source. The licensing philosophy and intended-use FAQ live on the docs site: [English](https://docs.gov.skin/en/community/licensing/) · [中文](https://docs.gov.skin/community/licensing/).

Using, modifying and redistributing the launcher are all free, including bundling it with your community or monetized server. ELv2 only restricts offering this software itself to third parties as a hosted/managed service, circumventing license keys, and removing notices.

The repository was developed under AGPL-3.0 without any tagged release, and moved to ELv2 on 2026-10-26. See [LICENSE.history](./LICENSE.history) for the history and what it means.
