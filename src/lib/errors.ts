export function normalizeError(error: unknown, fallback = "Nieznany błąd."): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (error && typeof error === "object") {
    const direct = (error as { message?: unknown }).message;
    if (typeof direct === "string" && direct.trim()) return direct;
    const nested = (error as { error?: { message?: unknown } }).error?.message;
    if (typeof nested === "string" && nested.trim()) return nested;
    try {
      const serialized = JSON.stringify(error);
      if (serialized && serialized !== "{}") return serialized;
    } catch {
      return fallback;
    }
  }
  if (error !== null && error !== undefined) {
    const primitive = String(error);
    if (primitive && primitive !== "[object Object]") return primitive;
  }
  return fallback;
}
