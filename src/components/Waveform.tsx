export function Waveform({ seed, active = false }: { seed: string; active?: boolean }) {
  let hash = 7;
  for (const char of seed) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  const bars = Array.from(
    { length: 18 },
    (_, index) => 4 + (((hash >>> (index % 16)) + index * 7) % 15),
  );
  return (
    <span className={`mini-wave ${active ? "mini-wave--active" : ""}`} aria-hidden="true">
      {bars.map((height, index) => <i key={index} style={{ height }} />)}
    </span>
  );
}
