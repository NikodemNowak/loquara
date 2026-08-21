const NVIDIA_PATH =
  "M8.948 8.798v-1.43a6.7 6.7 0 0 1 .424-.018c3.922-.124 6.493 3.374 6.493 3.374s-2.774 3.851-5.75 3.851c-.398 0-.787-.062-1.158-.185v-4.346c1.528.185 1.837.857 2.747 2.385l2.04-1.714s-1.492-1.952-4-1.952a6.016 6.016 0 0 0-.796.035m0-4.735v2.138l.424-.027c5.45-.185 9.01 4.47 9.01 4.47s-4.08 4.964-8.33 4.964c-.37 0-.733-.035-1.095-.097v1.325c.3.035.61.062.91.062 3.957 0 6.82-2.023 9.593-4.408.459.371 2.34 1.263 2.73 1.652-2.633 2.208-8.772 3.984-12.253 3.984-.335 0-.653-.018-.971-.053v1.864H24V4.063zm0 10.326v1.131c-3.657-.654-4.673-4.46-4.673-4.46s1.758-1.944 4.673-2.262v1.237H8.94c-1.528-.186-2.73 1.245-2.73 1.245s.68 2.412 2.739 3.11M2.456 10.9s2.164-3.197 6.5-3.533V6.201C4.153 6.59 0 10.653 0 10.653s2.35 6.802 8.948 7.42v-1.237c-4.84-.6-6.492-5.936-6.492-5.936z";

const OPENAI_PATH =
  "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997z";

/**
 * Cohere's brandmark, from its official vector.
 *
 * The mark is three coloured shapes rather than one silhouette, so unlike the
 * others it keeps its brand colours: flattened to a single ink the shapes read
 * as an unrecognisable blob.
 */
const COHERE_SHAPES: Array<{ fill: string; d: string }> = [
  { fill: "#39594D", d: "M24.3,44.7c2,0,6-0.1,11.6-2.4c6.5-2.7,19.3-7.5,28.6-12.5c6.5-3.5,9.3-8.1,9.3-14.3C73.8,7,66.9,0,58.3,0 h-36C10,0,0,10,0,22.3S9.4,44.7,24.3,44.7z" },
  { fill: "#D18EE2", d: "M30.4,60c0-6,3.6-11.5,9.2-13.8l11.3-4.7C62.4,36.8,75,45.2,75,57.6C75,67.2,67.2,75,57.6,75l-12.3,0 C37.1,75,30.4,68.3,30.4,60z" },
  { fill: "#FF7759", d: "M12.9,47.6L12.9,47.6C5.8,47.6,0,53.4,0,60.5v1.7C0,69.2,5.8,75,12.9,75h0c7.1,0,12.9-5.8,12.9-12.9v-1.7 C25.7,53.4,20,47.6,12.9,47.6z" },
];

/** Providers whose official mark Loquara ships as a real vector. */
const PATHS: Record<string, string> = {
  NVIDIA: NVIDIA_PATH,
  OpenAI: OPENAI_PATH,
};

/**
 * The mark for a model's provider.
 *
 * Providers with a real vector get their own logo. Everyone else gets a
 * monogram set in the interface's own type — deliberately Loquara's label for
 * them rather than an approximation of a mark it does not have. An invented
 * logo is worse than an honest initial: it misrepresents someone's brand and
 * it never quite looks right next to the genuine ones.
 */
export function BrandLogo({ provider }: { provider: string }) {
  if (provider === "Cohere") {
    return (
      <svg viewBox="0 0 75 75" aria-hidden="true" focusable="false" className="brand-logo--colour">
        {COHERE_SHAPES.map((shape) => (
          <path key={shape.fill} d={shape.d} fill={shape.fill} fillRule="evenodd" clipRule="evenodd" />
        ))}
      </svg>
    );
  }
  const path = PATHS[provider];
  if (path) {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d={path} />
      </svg>
    );
  }
  return (
    <span className="provider-mark__monogram" aria-hidden="true">
      {provider.slice(0, 1).toUpperCase()}
    </span>
  );
}
