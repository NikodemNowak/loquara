# Loquara

Loquara to lokalna, otwartoźródłowa aplikacja do dyktowania na Windows, macOS i Linux. Nagrywa mikrofon po globalnym skrócie, transkrybuje wypowiedź modelem NVIDIA Parakeet na karcie graficznej i może automatycznie wkleić tekst do aktywnej aplikacji.

## Najważniejsze możliwości

- globalny skrót `Ctrl+Space` uruchamia i kończy nagrywanie, `Esc` je anuluje;
- mała nakładka pokazuje nagrywanie, przetwarzanie, wklejanie i błąd — tylko podczas dyktowania;
- zasobnik systemowy pozwala rozpocząć nagranie, wkleić ostatni tekst i otworzyć główne okno;
- historia zachowuje nagrania i tekst, a słownik oraz tryby poprawiają wynik;
- jasny, ciemny i systemowy motyw;
- opcjonalny autostart oraz automatyczne wklejanie;
- **wszystko działa lokalnie** — audio nie opuszcza komputera.

## Prywatność i architektura

Interfejs React działa w Tauri. Rust przechwytuje dźwięk, zapisuje SQLite i integruje skróty, zasobnik oraz schowek. Długowieczny worker Python ładuje `nvidia/parakeet-tdt-0.6b-v3` w dokładnej rewizji `7c35754d166cca382ad1e53e68b01e7c575f3a1d` przez Transformers/PyTorch CUDA. Po jednorazowym pobraniu modelu transkrypcja nie wysyła nagrań do usługi chmurowej.

## Wymagania

- Windows 10/11 x64, macOS lub Linux (x64);
- Node.js, `pnpm`, Rust zgodny z `rust-version` (dla builda);
- Python 3.10+ (testowane z 3.13);
- PyTorch z CUDA zainstalowany osobno, zgodny ze sterownikiem NVIDIA;
- co najmniej 6 GB wolnego miejsca na model w cache Hugging Face.

`engine/requirements.txt` celowo nie zawiera PyTorch, aby nie zastąpić zoptymalizowanego wydania CUDA.

## Instalacja silnika i modelu

W PowerShell (lub bash na macOS/Linux):

```powershell
.\scripts\setup-engine.ps1
.\scripts\download-model.ps1
```

`setup-engine.ps1` instaluje dokładne wersje lekkich zależności, sprawdza CUDA, nazwę GPU i ping workera. `download-model.ps1` pobiera dokładną rewizję do standardowego cache Hugging Face, jest idempotentny i raportuje ścieżkę oraz liczbę bajtów. Jeśli Python nie jest w `PATH`, ustaw `MOW_PYTHON` na pełną ścieżkę do `python.exe`.

## Uruchamianie i budowanie

```powershell
pnpm install
.\scripts\run-dev.ps1
pnpm tauri build
```

Opcje `run-dev.ps1`: `-SetupEngine`, `-DownloadModel` i `-Built`. Binarka release powstaje w `src-tauri\target\release\loquara.exe`, a instalator NSIS w `src-tauri\target\release\bundle\nsis`.

Pełna weryfikacja uruchomieniowa:

```powershell
.\tests\smoke.ps1 -ExePath .\src-tauri\target\release\loquara.exe
```

## Dane lokalne

- ustawienia, historia i nagrania: `%APPDATA%\io.loquara.desktop`;
- model: `%USERPROFILE%\.cache\huggingface\hub` albo ścieżki wynikające z `HF_HOME` / `HF_HUB_CACHE`;
- dane modelu i nagrania nie są dołączane do repozytorium.

## Licencja

MIT — szczegóły w pliku [LICENSE](LICENSE).
