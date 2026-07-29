# Mów Windows Dictation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Zbudować i uruchomić lokalną aplikację Windows do dyktowania z Parakeet, zasobnikiem, globalnym skrótem, auto-wklejaniem, historią audio i dopracowanym jasnym/ciemnym UI.

**Architecture:** Tauri 2 uruchamia dwa okna React: główne i nakładkę. Rust zapewnia integrację Windows, audio i magazyn danych, a długowieczny worker Python ładuje Parakeet do GPU i komunikuje się protokołem JSONL.

**Tech Stack:** Rust 1.95, Tauri 2, React 19, TypeScript, Vite, Vitest, cpal, hound, rusqlite, tauri-plugin-global-shortcut, Python 3.13, PyTorch CUDA, Transformers.

---

## Struktura plików

- `package.json`, `vite.config.ts`, `tsconfig*.json`: narzędzia frontendu.
- `src/app/`: powłoka, routing, główne strony i tokeny motywów.
- `src/features/dictation/`: maszyna stanów UI, nakładka i sterowanie.
- `src/features/history/`: historia, szczegóły i akcje odzyskiwania.
- `src/features/settings/`: skróty, urządzenia, model i wygląd.
- `src/lib/`: kontrakty Tauri i wspólne narzędzia.
- `src-tauri/src/audio.rs`: przechwytywanie mikrofonu i zapis WAV.
- `src-tauri/src/dictation.rs`: koordynator przepływu.
- `src-tauri/src/storage.rs`: SQLite i konfiguracja.
- `src-tauri/src/transcription.rs`: worker JSONL.
- `src-tauri/src/platform.rs`: tray, skróty, okna i wklejanie.
- `engine/parakeet_worker.py`: lokalna inferencja NVIDIA.
- `tests/`: testy integracyjne i kontrakt workera.

### Task 1: Szkielet aplikacji i kontrakty domeny

**Files:**
- Create: `package.json`
- Create: `vite.config.ts`
- Create: `src/lib/types.ts`
- Create: `src/features/dictation/machine.ts`
- Test: `src/features/dictation/machine.test.ts`
- Create: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/domain.rs`
- Test: `src-tauri/src/domain.rs`

- [ ] **Step 1: Napisz testy RED dla stanów `idle → recording → processing → pasting → idle` oraz przejść `failed/retry/cancel`.**
- [ ] **Step 2: Uruchom `pnpm test -- machine.test.ts` i `cargo test domain --manifest-path src-tauri/Cargo.toml`; oczekuj błędów braku implementacji.**
- [ ] **Step 3: Zaimplementuj minimalne reduktory TypeScript i Rust z jednoznacznymi zdarzeniami.**
- [ ] **Step 4: Uruchom oba zestawy testów; oczekuj kompletu PASS bez ostrzeżeń.**
- [ ] **Step 5: Dodaj konfigurację Tauri/Vite i podstawowe skrypty `dev`, `build`, `test`, `tauri`.**
- [ ] **Step 6: Zacommituj `feat: scaffold dictation state model`.**

### Task 2: Silnik Parakeet i protokół JSONL

**Files:**
- Create: `engine/parakeet_worker.py`
- Create: `engine/protocol.py`
- Test: `engine/test_protocol.py`
- Create: `src-tauri/src/transcription.rs`
- Test: `src-tauri/src/transcription.rs`

- [ ] **Step 1: Napisz testy RED protokołu dla `ping`, `load`, `transcribe`, błędnej wiadomości i błędu brakującego WAV.**
- [ ] **Step 2: Uruchom `python -m unittest engine.test_protocol -v`; oczekuj FAIL z brakującymi symbolami.**
- [ ] **Step 3: Zaimplementuj JSONL z jednym obiektem odpowiedzi na jedną linię wejścia i ustrukturyzowanymi błędami.**
- [ ] **Step 4: Dodaj adapter `ParakeetEngine`, który leniwie ładuje `nvidia/parakeet-tdt-0.6b-v3`, wybiera CUDA, czyta WAV i zwraca czysty tekst.**
- [ ] **Step 5: W Rust napisz testy RED parsowania odpowiedzi i mapowania błędów, potem minimalnego klienta workera; uruchom `cargo test transcription`.**
- [ ] **Step 6: Dodaj `scripts/setup-engine.ps1`, który instaluje przypięte zależności i wykonuje `ping` bez pobierania modelu.**
- [ ] **Step 7: Zacommituj `feat: add local parakeet transcription worker`.**

### Task 3: Audio, SQLite i integracja Windows

**Files:**
- Create: `src-tauri/src/audio.rs`
- Create: `src-tauri/src/storage.rs`
- Create: `src-tauri/src/platform.rs`
- Create: `src-tauri/src/dictation.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: moduły Rust obok implementacji

- [ ] **Step 1: Napisz testy RED dla nazw plików audio, finalizacji WAV, CRUD historii, retencji i post-processingu słownika.**
- [ ] **Step 2: Uruchom `cargo test --manifest-path src-tauri/Cargo.toml`; potwierdź oczekiwane FAIL.**
- [ ] **Step 3: Zaimplementuj `cpal` + `hound`, zapisując natywny mono/stereo PCM z metadanymi częstotliwości.**
- [ ] **Step 4: Zaimplementuj migrację SQLite i repozytoria ustawień, historii i słownika.**
- [ ] **Step 5: Zaimplementuj zasobnik, globalne skróty `Ctrl+Space`/`Esc`, zapamiętanie fokusu, schowek i `Ctrl+V`.**
- [ ] **Step 6: Połącz koordynator: zapis → worker → historia → schowek → wklejenie, zawsze zachowując WAV po błędzie.**
- [ ] **Step 7: Uruchom pełne `cargo test`; oczekuj PASS.**
- [ ] **Step 8: Zacommituj `feat: integrate windows audio dictation pipeline`.**

### Task 4: Interfejs główny, nakładka i motywy

**Files:**
- Create: `src/main.tsx`
- Create: `src/app/App.tsx`
- Create: `src/app/theme.css`
- Create: `src/app/app.css`
- Create: `src/features/dictation/RecorderOverlay.tsx`
- Create: `src/features/history/HistoryPage.tsx`
- Create: `src/features/settings/SettingsPage.tsx`
- Create: `src/features/vocabulary/VocabularyPage.tsx`
- Create: `src/features/modes/ModesPage.tsx`
- Test: `src/**/*.test.tsx`

- [ ] **Step 1: Napisz testy RED dostępności i zachowania dla nawigacji, czterech stanów nakładki, retry, kopiowania i przełączania motywu.**
- [ ] **Step 2: Uruchom `pnpm test`; oczekuj FAIL z brakującymi komponentami.**
- [ ] **Step 3: Zaimplementuj tokeny jasnego/ciemnego/systemowego motywu dokładnie według specyfikacji, bez gradientów i szkła.**
- [ ] **Step 4: Zaimplementuj płaską powłokę z boczną nawigacją oraz strony Dzisiaj, Historia, Słownik, Tryby i Ustawienia.**
- [ ] **Step 5: Zaimplementuj nakładkę 312×56 dla `recording`, `processing`, `pasting`, `failed`, z falą audio i licznikiem.**
- [ ] **Step 6: Podłącz komendy i zdarzenia Tauri przez typowany adapter z bezpiecznym trybem demonstracyjnym w przeglądarce.**
- [ ] **Step 7: Uruchom `pnpm test` i `pnpm build`; oczekuj PASS i kompilacji bez błędów.**
- [ ] **Step 8: Zacommituj `feat: build polished mow desktop interface`.**

### Task 5: Instalacja modelu, pakiet Windows i próba końcowa

**Files:**
- Create: `scripts/download-model.ps1`
- Create: `scripts/run-dev.ps1`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `README.md`
- Test: `tests/smoke.ps1`

- [ ] **Step 1: Napisz test RED smoke sprawdzający obecność binarki, uruchomienie procesu, utworzenie zasobnika i odpowiedź `ping` workera.**
- [ ] **Step 2: Skonfiguruj ikony, pojedynczą instancję, start w zasobniku, zasoby workera i instalator NSIS.**
- [ ] **Step 3: Zainstaluj zależności workera i pobierz model do lokalnego cache Hugging Face.**
- [ ] **Step 4: Uruchom `pnpm test`, `cargo test`, `python -m unittest`, `pnpm tauri build` i smoke test; napraw wszystkie błędy przez test RED → GREEN.**
- [ ] **Step 5: Uruchom gotową aplikację, potwierdź proces i zasobnik oraz wykonaj próbne nagranie z ponowieniem błędu.**
- [ ] **Step 6: Zacommituj `build: package and verify mow for windows`.**

## Samokontrola planu

Plan obejmuje wszystkie wymagania briefu: szybkość, skróty, nakładkę statusu, zapis audio, retry, auto-wklejanie, zasobnik, model open source, jasny/ciemny motyw, historię, słownik i tryby. Nazwy stanów są spójne między TypeScript, Rust i testami. Nie ma nierozstrzygniętych placeholderów; implementacja jest podzielona na pięć samodzielnie weryfikowalnych zadań.
