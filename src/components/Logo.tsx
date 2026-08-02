export function Logo({ size = 22 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 512 512" aria-hidden="true" focusable="false">
      <rect width="512" height="512" rx="104" fill="currentColor" />
      <g fill="none" stroke="#f5f3ee" strokeLinecap="round">
        <path strokeWidth="30" d="M256 156v200" />
        <path strokeWidth="26" d="M196 190v132" />
        <path strokeWidth="26" d="M316 190v132" />
        <path strokeWidth="22" d="M146 226v60" />
        <path strokeWidth="22" d="M366 226v60" />
        <path strokeWidth="18" d="M106 262v-12" />
        <path strokeWidth="18" d="M406 262v-12" />
      </g>
    </svg>
  );
}
