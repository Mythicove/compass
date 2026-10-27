import { useEffect, useRef } from "react";
import { SkinViewer } from "skinview3d";

// skinUrl 为 null 时不加载纹理，skinview3d 会渲染默认模型（无皮肤的玩家也应有预览）。
// 传入的是 Rust 侧转好的 data: URL，webview 因此不需要直连后端（CSP 不放行外站图片）。
export function SkinPreview({ skinUrl }: { skinUrl: string | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const viewer = new SkinViewer({ canvas, width: 260, height: 360 });
    viewer.autoRotate = true;
    if (skinUrl) viewer.loadSkin(skinUrl).catch(() => {});

    return () => viewer.dispose();
  }, [skinUrl]);

  return <canvas ref={canvasRef} />;
}
