import { useEffect, useRef } from "react";
import { SkinViewer } from "skinview3d";

export function SkinPreview({ skinUrl }: { skinUrl: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const viewer = new SkinViewer({ canvas, width: 260, height: 360 });
    viewer.autoRotate = true;
    viewer.loadSkin(skinUrl).catch(() => {});

    return () => viewer.dispose();
  }, [skinUrl]);

  return <canvas ref={canvasRef} />;
}
