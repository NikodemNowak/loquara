import { useEffect, useId, useRef, useState } from "react";
import "./Logo.css";

export type LogoMotion = "none" | "intro" | "loading" | "working";

const folds = [
  "M5 17.5C5 9.8 9.5 5 17 5H27V11H17C13.4 11 11 13.3 11 17.5V21H5Z",
  "M27 14.5C27 22.2 22.5 27 15 27H5V21H15C18.6 21 21 18.7 21 14.5V11H27Z",
];
const centerlines = ["M8 21V17.5C8 11 12 8 17 8H27", "M24 11V14.5C24 21 20 24 15 24H5"];

export function Logo({ size = 22, className = "", motion = "none" }: { size?: number; className?: string; motion?: LogoMotion }) {
  const id = useId().replace(/:/g, "");
  const ref = useRef<SVGSVGElement>(null);
  const [paused, setPaused] = useState(() => document.hidden);
  useEffect(() => {
    if (motion === "none") return;
    let inView = true;
    const update = () => setPaused(document.hidden || !inView);
    const observer = typeof IntersectionObserver === "function" ? new IntersectionObserver(([entry]) => {
      inView = entry.isIntersecting;
      update();
    }) : null;
    if (ref.current) observer?.observe(ref.current);
    document.addEventListener("visibilitychange", update);
    update();
    return () => { observer?.disconnect(); document.removeEventListener("visibilitychange", update); };
  }, [motion]);

  return <svg ref={ref} width={size} height={size} viewBox="0 0 32 32" className={`loquara-mark loquara-mark--${motion} ${className}`} data-paused={paused || undefined} aria-hidden="true" focusable="false">
    {(motion === "intro" || motion === "loading") && <defs>{folds.map((path, index) => <g key={index}>
      <clipPath id={`${id}-fold-${index}`}><path d={path} /></clipPath>
      <mask id={`${id}-reveal-${index}`} maskUnits="userSpaceOnUse" x="0" y="0" width="32" height="32">
        <path className={`loquara-mark__reveal loquara-mark__reveal--${index}`} d={centerlines[index]} pathLength="70" />
      </mask>
    </g>)}</defs>}
    <g className="loquara-mark__body">{folds.map((path, index) => <path key={index} className={`loquara-mark__fold loquara-mark__fold--${index}`} d={path} />)}</g>
    {motion === "intro" && <g className="loquara-mark__entrance">{folds.map((path, index) => <path key={index} className={`loquara-mark__fold loquara-mark__fold--${index}`} d={path} mask={`url(#${id}-reveal-${index})`} />)}</g>}
    {motion === "loading" && <g className="loquara-mark__travel">{centerlines.map((path, index) => <path key={index} className={`loquara-mark__highlight loquara-mark__highlight--${index}`} d={path} pathLength="70" clipPath={`url(#${id}-fold-${index})`} />)}</g>}
  </svg>;
}
