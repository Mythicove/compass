# Mythic Compass

[English](./README.md) | **简体中文**

Mythicove 生态的 Minecraft 启动器（Tauri 2 + React 前端，Rust 核心）：用玩家的**启动器独立密码**登录 `mythic-mirror`、skinview3d 皮肤预览、从 `mythic-hoard` 浏览模组。定位私群/小社区场景，不是公共市场竞争者。

## 现状

开发按阶段计划推进：[docs/plans/2026-10-27-compass-launcher.md](https://github.com/Mythicove/docs/blob/main/plans/2026-10-27-compass-launcher.md)（P0 → P6，目标是三平台上达到 HMCL/PCL 级能力）。

已实现（P0 —— 骨架与仓库卫生）：

- **所有后端 HTTP 都在 Rust 核心里**（`src-tauri/src/api.rs`）。webview 完全不碰网络，于是桌面端既不依赖 Mirror 的 CORS 配置，CSP 也不需要放行任何远端 `connect-src`/`img-src`——皮肤由 Rust 取回后以 `data:` URL 交给前端。
- **只有具名命令。** 原先的 `launch(program, args)`（任意命令执行）与 `download_file(url, dest)`（任意 URL 写任意路径）已删除，也不提供通用转发命令。
- **令牌不进 webview。** 命令返回的是不含令牌的账号视图；`accounts.json` 留在数据目录（P4 迁到系统钥匙串）。
- 登录用**玩家名 + 该玩家的启动器独立密码**（[ADR-0012](https://github.com/Mythicove/docs/blob/main/src/content/docs/architecture/adr-0012-credential-separation.md)），不是网站主密码。
- 持久化设置（`compass.json`，带输入收敛与原子写）、明确的数据目录布局（资源/库/版本全局共享，实例互相隔离）、深浅色与跟随系统主题、中英双语界面。
- 三平台 CI（`cargo fmt --check`、`clippy -D warnings`、`cargo test`、typecheck、前端构建）+ 按需打包成 draft release。

**尚未实现：启动 Minecraft 本身。** 原先"把 `.mrpack` 交给 Prism Launcher"的路径是**直接删掉**而不是与新路径并存——只留一条启动路径。按阶段还差：原生启动（piston-meta 版本清单、资源与库的校验/续传下载、Java 检测、启动参数构造、authlib-injector 注入、进程管理）属 **P1**；实例与 mod/资源包管理属 **P2**；mod loader 与整合包安装属 **P3**；多账号与钥匙串存储属 **P4**；崩溃诊断、Java 自动下载与自动更新属 **P5**；三平台签名打包与发布属 **P6**。

## 运行

```bash
pnpm install
pnpm typecheck      # tsc --noEmit
pnpm tauri dev      # 开发
pnpm tauri build    # 打包
```

服务器地址默认 `http://localhost:8080`，可在「设置」里修改（持久化到数据目录的 `compass.json`，设置页会列出完整目录布局）。

## 目录

```
src-tauri/src/
  paths.rs      数据目录布局（全局缓存 vs 实例隔离）
  config.rs     compass.json：默认值、输入收敛、原子写
  account.rs    已登录账号（令牌只留在这里，不跨进 webview）
  api.rs        Mirror/Hoard 的具名 HTTP 客户端（唯一出网的地方）
  error.rs      错误序列化成 { code, message }，前端按 code 本地化
  commands.rs   Tauri 命令（薄层）
  state.rs      共享状态
src/
  i18n/         中英两份文案表
  lib/ipc.ts    与 Rust 命令一一对应的类型化封装
  views/        首页 / 实例 / 模组 / 设置
```

## 许可证

[Elastic License 2.0 (ELv2)](./LICENSE)

本仓库曾以 AGPL-3.0 开发（未发布过 tag），2026-10-26 起换证为 ELv2；历史与影响见 [LICENSE.history](./LICENSE.history)。
