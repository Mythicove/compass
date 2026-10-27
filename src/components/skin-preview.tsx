import { useEffect, useMemo, useRef, useState } from "react";
import { SkinViewer } from "skinview3d";

// WebGL 不可用时（部分 Linux 的 WebKitGTK、远程桌面、被禁用的硬件加速）退化为平面贴图：
// 有信息总比一块空白画布好，而且这是启动器里唯一依赖 WebGL 的地方。
function webglAvailable(): boolean {
  try {
    const canvas = document.createElement("canvas");
    return !!(canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  } catch {
    return false;
  }
}

// skinUrl 为 null 时不加载纹理，skinview3d 会渲染默认模型（无皮肤的玩家也应有预览）。
// 传入的是 Rust 侧转好的 data: URL，webview 因此不需要直连后端（CSP 不放行外站图片）。
export function SkinPreview({ skinUrl }: { skinUrl: string | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const webgl = useMemo(webglAvailable, []);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!webgl) return;
    const canvas = canvasRef.current;
    if (!canvas) return;

    let viewer: SkinViewer;
    try {
      viewer = new SkinViewer({ canvas, width: 260, height: 360 });
    } catch {
      // 上下文创建失败（驱动/黑名单）：走 2D 回退，不要留一块死画布
      setFailed(true);
      return;
    }
    viewer.autoRotate = true;
    if (skinUrl) viewer.loadSkin(skinUrl).catch(() => {});

    return () => viewer.dispose();
  }, [skinUrl, webgl]);

  if (!webgl || failed) {
    // 没有 WebGL 就没有 3D 预览；有皮肤时退化为平面贴图，没有皮肤时什么都不画
    // （一块空白画布比"没有预览"更容易被误认为坏了）
    return skinUrl ? (
      <img
        src={skinUrl}
        alt="skin texture"
        width={128}
        height={128}
        style={{ imageRendering: "pixelated" }}
      />
    ) : null;
  }
  return <canvas ref={canvasRef} />;
}
