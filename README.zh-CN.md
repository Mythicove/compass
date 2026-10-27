# Mythic Compass

[English](./README.md) | **简体中文**

Minecraft 启动器原型（Tauri 2 + React），接入 Mythicove 生态：Yggdrasil 登录、skinview3d 皮肤预览、从 `mythic-hoard` 浏览并下载 mod/整合包（`.mrpack`）。定位私群/小社区场景，不是公共市场竞争者。

## 现状（原型）

已实现：

- Yggdrasil 账号密码登录（对接 `mythic-mirror`），会话持久化
- skinview3d 3D 皮肤预览
- 从 `mythic-hoard` 拉取项目/版本列表，下载 `.mrpack`
- 委托外部启动器（默认 Prism Launcher）`--import` 接管实际启动

**未实现（与"启动器"之名的差距）**：原生 Minecraft 启动（Java 检测、piston-meta 版本清单、asset 下载、authlib-injector 注入、实例管理）、下载进度/校验、i18n、自动更新。当前是单页原型，详见 `docs/plans/status.md`。

## 运行

```bash
pnpm install
pnpm tauri dev      # 开发
pnpm tauri build    # 打包
```

API 地址默认 `http://localhost:8080`（统一 API 域名），可在界面/`localStorage` 覆盖。

## 许可证

[Elastic License 2.0 (ELv2)](./LICENSE)

本仓库曾以 AGPL-3.0 开发（未发布过 tag），2026-10-26 起换证为 ELv2；历史与影响见 [LICENSE.history](./LICENSE.history)。
