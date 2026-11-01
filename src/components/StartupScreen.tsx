import { useEffect, useState, type ReactNode } from "react";
import { useI18n } from "../lib/i18n";
import { Logo } from "./Logo";
import "./StartupScreen.css";

const prefersReducedMotion = () => typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export function StartupScreen({ loading, children }: { loading: boolean; children: ReactNode }) {
  // A synthetic development fixture for capture; removed from production builds.
  loading = loading || (import.meta.env.DEV && new URLSearchParams(window.location.search).get("demoStartup") === "loading");
  const { t } = useI18n();
  const [intro, setIntro] = useState(() => !prefersReducedMotion());
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!intro) return;
    const timer = window.setTimeout(() => setIntro(false), 820);
    const media = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    const changed = () => { if (media?.matches) setIntro(false); };
    media?.addEventListener("change", changed);
    return () => { window.clearTimeout(timer); media?.removeEventListener("change", changed); };
  }, [intro]);
  useEffect(() => {
    if (!loading) return;
    const timer = window.setTimeout(() => setSlow(true), 12_000);
    return () => window.clearTimeout(timer);
  }, [loading]);
  if (!intro && !loading) return children;
  return <section className="startup-screen" role="status" aria-label={t(loading ? "startup.starting" : "startup.intro")}>
    <div className="startup-screen__identity">
      <Logo size={90} motion={intro ? "intro" : loading ? "loading" : "none"} />
      <h1>loquara</h1>
      <p>{t(loading ? "startup.starting" : "startup.intro")}</p>
    </div>
    <div className="startup-screen__action">
      {!loading ? <button type="button" onClick={() => setIntro(false)}>{t("startup.skip")}</button> : slow ? <><p>{t("startup.slow")}</p><button type="button" onClick={() => window.location.reload()}>{t("startup.retry")}</button></> : null}
    </div>
  </section>;
}
