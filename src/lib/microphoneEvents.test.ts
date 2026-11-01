import { expect, test, vi } from "vitest";
import { getAdapter } from "./tauri";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

test("native adapter forwards the typed device event and releases its listener", async () => {
  const dispose = vi.fn();
  vi.mocked(listen).mockResolvedValue(dispose);
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  try {
    const received = vi.fn();
    const unlisten = await getAdapter().onInputDevicesChanged(received);
    expect(listen).toHaveBeenCalledWith("audio://devices-changed", expect.any(Function));
    const deliver = vi.mocked(listen).mock.calls[0][1];
    const payload = { generation: 4, devices: [{ id: "wasapi:opaque", name: "USB", isDefault: true, available: true }] };
    deliver({ event: "audio://devices-changed", id: 1, payload });
    expect(received).toHaveBeenCalledWith(payload);
    unlisten();
    expect(dispose).toHaveBeenCalledOnce();
  } finally { Reflect.deleteProperty(window, "__TAURI_INTERNALS__"); }
});
