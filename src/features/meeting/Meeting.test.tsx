import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { App } from "../../app/App";
import { HistoryPage } from "../history/HistoryPage";
import { adapterStub, idleMeeting, meetingRecording } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";
import type { MeetingStatus, Recording } from "../../lib/types";
import { formatClock } from "./MeetingBar";
import { groupTurns } from "./MeetingTranscript";

describe("spotkania", () => {
  test("pierwsze nagranie wymaga potwierdzenia informacji o nagrywaniu innych", async () => {
    const user = userEvent.setup();
    const startMeeting = vi.fn(async (): Promise<MeetingStatus> => ({ ...idleMeeting, phase: "recording", meetingId: "m", startedAt: Date.now(), consentAcknowledged: true }));
    const acknowledgeMeetingConsent = vi.fn(async () => ({ ...idleMeeting, consentAcknowledged: true }));
    renderWithI18n(<App adapter={adapterStub({
      getMeetingStatus: async () => ({ ...idleMeeting, consentAcknowledged: false }),
      startMeeting,
      acknowledgeMeetingConsent,
    })} />);

    await user.click(await screen.findByRole("button", { name: "Nagraj spotkanie" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Nagrywasz także innych");
    expect(startMeeting).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole("button", { name: "Rozumiem, nagrywaj" }));
    await waitFor(() => expect(startMeeting).toHaveBeenCalledTimes(1));
    expect(acknowledgeMeetingConsent).toHaveBeenCalledTimes(1);
    const bar = await screen.findByRole("status");
    expect(bar).toHaveTextContent("Nagrywanie spotkania");
    expect(bar).toHaveTextContent("słuchawek");
    expect(screen.queryByRole("button", { name: "Nagraj spotkanie" })).not.toBeInTheDocument();
  });

  test("pasek nagrywania zostaje widoczny i zatrzymuje spotkanie", async () => {
    const user = userEvent.setup();
    let emit!: (status: MeetingStatus) => void;
    const stopMeeting = vi.fn(async (): Promise<MeetingStatus> => ({ ...idleMeeting, phase: "processing", meetingId: "m", progress: 0 }));
    renderWithI18n(<App adapter={adapterStub({
      onMeetingStatus: async (listener) => { emit = listener; return () => undefined; },
      stopMeeting,
    })} />);
    await screen.findByRole("button", { name: "Nagraj spotkanie" });

    await act(async () => emit({ ...idleMeeting, phase: "recording", meetingId: "m", startedAt: Date.now() - 65_000, othersSilent: true }));
    const bar = screen.getByRole("status");
    expect(bar).toHaveTextContent("01:05");
    expect(bar).toHaveTextContent("Nagrywanie ekranu i dźwięku systemowego");

    await user.click(within(bar).getByRole("button", { name: "Zakończ i przepisz" }));
    expect(stopMeeting).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole("status")).toHaveTextContent("Przepisuję spotkanie… 0%");
  });

  test("historia pokazuje spotkanie jako rozmowę z podpisanymi stronami", () => {
    renderWithI18n(
      <HistoryPage adapter={adapterStub()} recordings={[meetingRecording]} onRefresh={async () => undefined} onToast={() => undefined} />,
    );

    const inspector = screen.getByRole("complementary", { name: "Szczegóły nagrania" });
    const turns = within(inspector).getAllByRole("listitem");
    expect(turns).toHaveLength(2);
    expect(turns[0]).toHaveTextContent("Ja00:01Zaczynamy?");
    expect(turns[1]).toHaveTextContent("Pozostali01:02Tak, slychac cie dobrze.");
    expect(within(inspector).queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.getByText("Spotkanie")).toBeInTheDocument();
  });

  test("przerwane spotkanie można przepisać ponownie z zachowanych ścieżek", async () => {
    const user = userEvent.setup();
    const failed: Recording = {
      ...meetingRecording,
      status: "failed",
      text: null,
      audioPath: null,
      error: "The meeting recording was interrupted. Retry to transcribe what was saved.",
      meeting: { segments: [], tracks: { me: "me.wav", others: "others.wav", meOffsetMs: 0, othersOffsetMs: 12 } },
    };
    const onRetryMeeting = vi.fn(async () => undefined);
    const retryTranscription = vi.fn();
    renderWithI18n(
      <HistoryPage adapter={adapterStub({ retryTranscription })} recordings={[failed]} onRefresh={async () => undefined} onToast={() => undefined} onRetryMeeting={onRetryMeeting} />,
    );

    expect(screen.getAllByText(/Nagrywanie spotkania zostało przerwane/).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "Przepisz ponownie" }));
    expect(onRetryMeeting).toHaveBeenCalledWith("meeting-1");
    expect(retryTranscription).not.toHaveBeenCalled();
  });

  test("kolejne wypowiedzi jednej strony łączą się w akapit", () => {
    expect(groupTurns([
      { speaker: "me", startMs: 0, endMs: 1, text: "A" },
      { speaker: "me", startMs: 2, endMs: 3, text: "B" },
      { speaker: "others", startMs: 4, endMs: 5, text: "C" },
    ])).toEqual([
      { speaker: "me", startMs: 0, endMs: 3, text: "A B" },
      { speaker: "others", startMs: 4, endMs: 5, text: "C" },
    ]);
    expect(formatClock(3_725_000)).toBe("1:02:05");
  });
});
