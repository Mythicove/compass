# 许可证历史

[English](./LICENSE.history) | **中文**

> 本文件仅为对外说明，不构成许可证文本；许可以 [LICENSE](./LICENSE) 为准。

## 现行许可证：Elastic License 2.0（ELv2）

自换证提交 `65487d6`（2026-10-26）起，本仓库以 **Elastic License 2.0** 授权，全文见 [LICENSE](./LICENSE)。许可人（Licensor）：Hsukqi Lee / Mythicove <team@tsinbei.com>。

## 历史许可证：AGPL-3.0

换证提交之前的所有提交（截至 `afd7f59`）以 **GNU Affero General Public License v3.0** 发布。本仓库尚未发布过任何 release tag，因此不存在以 AGPL 发布的正式版本；但换证前的提交在 git 历史中仍按 AGPL-3.0 授权，其授权对这些版本不可撤销。

## 换证原因

见 [ADR-0004 许可证策略](https://docs.gov.skin/architecture/adr-0004-license/)（2026-10-24 修订）。要点：

- 项目本意是**反商用托管**（防止第三方把本站作为商业托管服务牟利），而非强制衍生品开源；AGPL 的 Copyleft 反而抬高了启动器/插件/自托管等生态接入门槛。
- ELv2 的三条限制精确命中该意图：禁止作为托管/管理服务提供给第三方、禁止绕过 license key、禁止去除版权声明；其余使用（自托管、企业内部、二次开发、客户端集成）全部放开。
- 本项目为单一作者、无外部贡献者，可单方面换证；换证仅对换证提交之后的版本生效。

## 实际影响

| 情形 | 结果 |
|---|---|
| 在 2026-10-26 前 fork 了本仓库 | 你的 fork 快照保持 AGPL-3.0，可永久按 AGPL 使用与分发；此后从上游同步的新代码按 ELv2。 |
| 自托管、修改、二次开发、客户端集成 | ELv2 允许。 |
| 作为托管/管理服务提供给第三方 | ELv2 不允许；需另行获得商业授权，联系 <team@tsinbei.com>。 |

## 参考

- [LICENSE](./LICENSE) — 现行许可证全文（ELv2）
- [ADR-0004](https://docs.gov.skin/architecture/adr-0004-license/) — 决策记录
- ELv2 官方文本：<https://www.elastic.co/licensing/elastic-license>
- AGPL-3.0 原文：<https://www.gnu.org/licenses/agpl-3.0.html>
