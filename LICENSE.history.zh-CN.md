# 许可证历史

[English](./LICENSE.history) | **中文**

本文件说明仓库的许可证历史，许可证文本以 [LICENSE](./LICENSE) 为准。

## 现行许可证：Elastic License 2.0（ELv2）

自提交 `65487d6`（2026-10-26）起，本仓库采用 Elastic License 2.0 授权，全文见 [LICENSE](./LICENSE)。许可人为 Hsukqi Lee / Mythicove <team@tsinbei.com>。

## 历史许可证：AGPL-3.0

提交 `afd7f59` 及之前的所有提交以 GNU Affero General Public License v3.0 发布。本仓库没有发布过 release tag，因此不存在以 AGPL 发布的正式版本。换证前的提交在 git 历史中仍按 AGPL-3.0 授权，对这些版本的授权不可撤销。

## 换证原因

见 [ADR-0004](https://docs.gov.skin/architecture/adr-0004-license/)（2026-10-24 修订）。项目要阻止第三方把它作为商业托管服务运营。AGPL 的 Copyleft 同时抬高了启动器、插件和自托管部署的接入门槛，这也不是项目想要的。ELv2 与这个意图一致：禁止把软件作为托管服务提供给第三方，禁止绕过 license key，禁止去除版权声明；其余使用全部允许，包括自托管、企业内部使用、二次开发和客户端集成。

项目只有一名作者，没有外部贡献者，换证不需要其他人同意，并且只对换证提交之后发布的版本生效。

## 对你的影响

| 情形 | 结果 |
|---|---|
| 在 2026-10-26 之前 fork 了本仓库 | 你的快照保持 AGPL-3.0，可以继续按 AGPL 使用和分发；之后从上游同步的代码适用 ELv2。 |
| 自托管、修改、二次开发或集成到客户端 | ELv2 允许。 |
| 想作为托管服务提供给第三方 | ELv2 不允许，联系 <team@tsinbei.com> 获取商业授权。 |

## 参考

- [LICENSE](./LICENSE)：现行许可证全文（ELv2）
- [ADR-0004](https://docs.gov.skin/architecture/adr-0004-license/)：决策记录
- ELv2 官方文本：<https://www.elastic.co/licensing/elastic-license>
- AGPL-3.0 文本：<https://www.gnu.org/licenses/agpl-3.0.html>
