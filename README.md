# Mythic Compass

定制 Minecraft 启动器，服务 Mythicove 混合站（统一鉴权 + 皮肤 + mod/整合包分发）。定位私群场景（学校 MC 群等），非公开市场。

## 功能规划

- **统一鉴权**：Yggdrasil 外置登录（对接 `mythic-mirror`），正版兼容。
- **皮肤预览**：`skinview3d` 3D 实时预览。
- **mod / 整合包**：从 `mythic-hoard` 拉取，`.mrpack` 导入（packwiz 兼容）。
- **一键启动**：配置 Java、游戏版本、loader（Fabric/Forge/NeoForge/Quilt），一键启动 Minecraft。

## 技术栈

- Tauri 2 + React + TypeScript + Vite
- `skinview3d`（3D）、`openapi-typescript`（对接后端契约）

## 开发

```bash
pnpm install
pnpm tauri dev      # 桌面开发
pnpm tauri build    # 打包
```

## 状态

骨架阶段，核心逻辑待实现。
