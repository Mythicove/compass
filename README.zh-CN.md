# Mythic Compass

[English](./README.md) | **简体中文**

Minecraft 启动器原型（Tauri 2 + React），接入 Mythicove 生态：Yggdrasil 登录、skinview3d 皮肤预览、从 mythic-hoard 浏览并下载 mod 和整合包（.mrpack）。面向私群和小社区场景，不参与公共市场竞争。

## 现状（原型）

已实现：

- Yggdrasil 账号密码登录（对接 mythic-mirror），会话持久化
- skinview3d 3D 皮肤预览
- 从 mythic-hoard 拉取 Mod、整合包与版本列表（下载与安装随开发计划落地）
- 原生启动 Rust 模块（版本解析、资源下载、Java 探测、参数构造、进程管理、实例）已实现并有测试，Tauri 命令接线进行中

尚未实现：Tauri 命令与 UI 接线（下载进度与校验 UI、i18n、自动更新、CI、三平台打包）。阶段计划与验收见 [Compass 启动器开发计划](https://docs.gov.skin/plans/compass-launcher/)。

## 运行

```bash
pnpm install
pnpm tauri dev      # 开发
pnpm tauri build    # 打包
```

API 地址默认 http://localhost:8080（统一 API 域名），可在界面或 localStorage 覆盖。

## 许可证

[Elastic License 2.0 (ELv2)](./LICENSE) —— 源码可得，非 OSI 开源。许可理念与预期用法 FAQ 见文档站：[中文](https://docs.gov.skin/community/licensing/) · [English](https://docs.gov.skin/en/community/licensing/)。

使用、修改、再分发启动器全部免费，包括与社区或盈利性服务器捆绑分发。ELv2 只限制三件事：把软件本身作为托管/管理服务提供给第三方、绕过 license key、移除版权声明。

本仓库曾以 AGPL-3.0 开发，未发布过 tag，2026-10-26 起换证为 ELv2。历史与影响见 [LICENSE.history](./LICENSE.history)。
