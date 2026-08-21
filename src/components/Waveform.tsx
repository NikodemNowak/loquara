import { useEffect, useRef, useState } from "react";

import { cssColor, drawPeaks, prepareCanvas, type Peaks } from "../lib/waveform";

const HEIGHT = 20;

/**
 * The amplitude envelope of one recording, as captured.
 *
 * Renders nothing when there is no envelope — recordings made before Loquara
 * stored one, and captures still in progress. An invented shape would suggest
 * the app knows something about the audio that it does not.
 */
export function Waveform({ peaks }: { peaks: Peaks | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [peaks]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !peaks?.length) return;
    const prepared = prepareCanvas(canvas, HEIGHT);
    if (!prepared) return;
    const color = cssColor("--faint", "#7d8792");
    drawPeaks(prepared.context, peaks, prepared.width, prepared.height, {
      played: color,
      pending: color,
      barWidth: 2,
      gap: 1.5,
    });
  }, [peaks, width]);

  if (!peaks?.length) return null;

  return <canvas ref={canvasRef} className="wave wave--static" aria-hidden="true" />;
}
