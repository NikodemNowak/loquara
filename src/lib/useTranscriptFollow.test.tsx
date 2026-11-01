import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";

import { useTranscriptFollow } from "./useTranscriptFollow";

function Draft({ recordingId, revision, visible = true }: { recordingId: string | null; revision: number; visible?: boolean }) {
  const { transcriptRef, onTranscriptScroll } = useTranscriptFollow(recordingId, revision);
  return visible ? <div ref={transcriptRef} onScroll={onTranscriptScroll} data-testid="draft">Draft revision {revision}</div> : null;
}

describe("transcript tail following", () => {
  test("follows growing revisions, preserves scroll-up, resumes near the tail and resets a new take", () => {
    const view = render(<Draft recordingId="first" revision={0} />);
    const element = screen.getByTestId("draft");
    let scrollHeight = 300;
    Object.defineProperties(element, {
      scrollHeight: { get: () => scrollHeight },
      clientHeight: { value: 100 },
      scrollTop: { value: 0, writable: true },
    });
    view.rerender(<Draft recordingId="first" revision={1} />);
    expect(element.scrollTop).toBe(200);
    scrollHeight = 400;
    view.rerender(<Draft recordingId="first" revision={2} />);
    expect(element.scrollTop).toBe(300);

    element.scrollTop = 50;
    fireEvent.scroll(element);
    scrollHeight = 500;
    view.rerender(<Draft recordingId="first" revision={3} />);
    expect(element.scrollTop).toBe(50);

    element.scrollTop = 380;
    fireEvent.scroll(element);
    scrollHeight = 600;
    view.rerender(<Draft recordingId="first" revision={4} />);
    expect(element.scrollTop).toBe(500);

    element.scrollTop = 30;
    fireEvent.scroll(element);
    scrollHeight = 350;
    view.rerender(<Draft recordingId="second" revision={0} />);
    expect(element.scrollTop).toBe(250);
    view.rerender(<Draft recordingId={null} revision={0} />);
    expect(element.scrollTop).toBe(0);
  });

  test("does not reset deliberate scroll-up when an unrelated render keeps the same take and revision", () => {
    const view = render(<Draft recordingId="take" revision={1} />);
    const element = screen.getByTestId("draft");
    Object.defineProperties(element, { scrollHeight: { value: 500 }, clientHeight: { value: 100 } });
    element.scrollTop = 40;
    fireEvent.scroll(element);
    view.rerender(<Draft recordingId="take" revision={1} />);
    expect(element.scrollTop).toBe(40);
  });

  test("follows a transcript newly attached after an overlay expansion without a new revision", () => {
    const view = render(<Draft recordingId="take" revision={2} visible={false} />);
    const originalHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
    const originalClient = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    Object.defineProperties(HTMLElement.prototype, {
      scrollHeight: { configurable: true, get: () => 500 },
      clientHeight: { configurable: true, get: () => 100 },
    });
    try {
      view.rerender(<Draft recordingId="take" revision={2} />);
      expect(screen.getByTestId("draft").scrollTop).toBe(400);
    } finally {
      if (originalHeight) Object.defineProperty(HTMLElement.prototype, "scrollHeight", originalHeight);
      else Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
      if (originalClient) Object.defineProperty(HTMLElement.prototype, "clientHeight", originalClient);
      else Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
    }
  });
});
