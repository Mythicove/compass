import { useI18n } from "../i18n";

// P0 只给出实例视图的位置与空态：实例的创建/启动随 P1（原生启动）落地，
// 内容管理（mods/资源包/存档）随 P2 落地。这里不放假的按钮。
export function InstancesView() {
  const { t } = useI18n();
  return (
    <section className="card">
      <h2>{t("home.instances")}</h2>
      <p className="muted">{t("home.noInstances")}</p>
    </section>
  );
}
