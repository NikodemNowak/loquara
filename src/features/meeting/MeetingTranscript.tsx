import { useI18n } from "../../lib/i18n";
import type { MeetingSegment, MeetingTranscript as Transcript } from "../../lib/types";
import { formatClock } from "./MeetingBar";

/** Consecutive turns of one speaker read as one paragraph. */
export function groupTurns(segments: MeetingSegment[]): MeetingSegment[] {
  const turns: MeetingSegment[] = [];
  for (const segment of segments) {
    const last = turns[turns.length - 1];
    if (last && last.speaker === segment.speaker) {
      turns[turns.length - 1] = { ...last, endMs: segment.endMs, text: `${last.text} ${segment.text}` };
    } else {
      turns.push({ ...segment });
    }
  }
  return turns;
}

export function MeetingTranscript({ meeting }: { meeting: Transcript }) {
  const { t } = useI18n();
  const turns = groupTurns(meeting.segments);
  return (
    <div className="meeting-transcript">
      <div className="inspector-transcript__head"><span>{t("meeting.transcript")}</span></div>
      {meeting.warnings?.includes("others_silent") && (
        <p className="inspector-hint">{t("meeting.othersSilentHistory")}</p>
      )}
      {turns.length ? (
        <ol className="meeting-turns">
          {turns.map((turn) => (
            <li key={`${turn.speaker}-${turn.startMs}`} className={`meeting-turn meeting-turn--${turn.speaker}`}>
              <span className="meeting-turn__who">
                {t(turn.speaker === "me" ? "meeting.speaker.me" : "meeting.speaker.others")}
                <time>{formatClock(turn.startMs)}</time>
              </span>
              <p>{turn.text}</p>
            </li>
          ))}
        </ol>
      ) : (
        <p className="inspector-hint">{t("meeting.empty")}</p>
      )}
    </div>
  );
}
