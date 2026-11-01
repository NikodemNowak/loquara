import { useCallback, useLayoutEffect, useRef, type UIEventHandler } from "react";

/** Follow draft revisions until the reader deliberately moves away from the tail. */
export function useTranscriptFollow(recordingId: string | null, revision: number | undefined) {
  const transcriptRef = useRef<HTMLDivElement>(null);
  const previousTake = useRef<string | null>(null);
  const previousRevision = useRef<number | undefined>(undefined);
  const previousElement = useRef<HTMLDivElement | null>(null);
  const following = useRef(true);
  const onTranscriptScroll: UIEventHandler<HTMLDivElement> = useCallback((event) => {
    const element = event.currentTarget;
    following.current = element.scrollHeight - element.clientHeight - element.scrollTop <= 24;
  }, []);

  useLayoutEffect(() => {
    const element = transcriptRef.current;
    if (!element) return;
    const attached = previousElement.current !== element;
    if (!attached && previousTake.current === recordingId && previousRevision.current === revision) return;
    previousElement.current = element;
    previousRevision.current = revision;
    if (attached || previousTake.current !== recordingId) {
      previousTake.current = recordingId;
      following.current = true;
      if (!recordingId) element.scrollTop = 0;
    }
    if (recordingId && following.current) {
      element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
    }
  });

  return { transcriptRef, onTranscriptScroll };
}
