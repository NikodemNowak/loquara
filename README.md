# Mów

Mów to lokalna aplikacja Windows do szybkiego dyktowania. Nagrywa mikrofon po
globalnym skrócie, transkrybuje wypowiedź modelem NVIDIA Parakeet na karcie
graficznej i może automatycznie wkleić tekst do aktywnej aplikacji.

## Najważniejsze możliwości

- globalny skrót `Ctrl+Space` uruchamia i kończy nagrywanie, `Esc` je anuluje;
- mała nakładka pokazuje nagrywanie, przetwarzanie, wklejanie i błąd;
- zasobnik systemowy pozwala rozpocząć nagranie, wkleić ostatni tekst i otworzyć
  główne okno;
- historia zachowuje nagrania i tekst, a słownik oraz tryby poprawiają wynik;
- jasny, ciemny i systemowy motyw;
- opcjonalny autostart oraz automatyczne wklejanie.

## Prywatność i architektura

Interfejs React działa w Tauri. Rust przechwytuje dźwięk, zapisuje SQLite i
integruje skróty, zasobnik oraz schowek Windows. Długowieczny worker Python
ładuje `nvidia/parakeet-tdt-0.6b-v3` w dokładnej rewizji
`7c35754d166cca382ad1e53e68b01e7c575f3a1d` przez Transformers/PyTorch CUDA.
Po jednorazowym pobraniu modelu transkrypcja nie wysyła nagrań do usługi
chmurowej.

## Wymagania

- Windows 10/11 x64;
- Node.js, `pnpm`, Rust zgodny z `rust-version` i narzędzia MSVC (dla builda);
- Python 3.10+ (testowane z 3.13);
- PyTorch z CUDA zainstalowany osobno, zgodny ze sterownikiem NVIDIA;
- co najmniej 6 GB wolnego miejsca na model w cache Hugging Face oraz dodatkowe
  miejsce na artefakty Rust i instalator (zmierzony snapshot ma ok. 5,02 GB).

`engine/requirements.txt` celowo nie zawiera PyTorch, aby nie zastąpić
zoptymalizowanego wydania CUDA.

## Instalacja silnika i modelu

W PowerShell:

```powershell
.\scripts\setup-engine.ps1
.\scripts\download-model.ps1
```

`setup-engine.ps1` instaluje dokładne wersje lekkich zależności, sprawdza CUDA,
nazwę GPU i ping workera. `download-model.ps1` pobiera dokładną rewizję do
standardowego cache Hugging Face, jest idempotentny i raportuje ścieżkę oraz
liczbę bajtów. Nie przenosi modelu poza cache. Jeśli Python nie jest w `PATH`,
ustaw `MOW_PYTHON` na pełną ścieżkę do `python.exe`.

## Uruchamianie i budowanie

```powershell
pnpm install
.\scripts\run-dev.ps1
pnpm tauri build
```

Opcje `run-dev.ps1`: `-SetupEngine`, `-DownloadModel` i `-Built`. Binarka
release powstaje w `src-tauri\target\release\mow.exe`, a instalator NSIS w
`src-tauri\target\release\bundle\nsis`. Po instalacji aplikacja znajduje pliki
workera w zasobach bundle niezależnie od katalogu roboczego.

Pełna weryfikacja uruchomieniowa:

```powershell
.\tests\smoke.ps1 -ExePath .\src-tauri\target\release\mow.exe
```

Smoke sprawdza ping, dokładną rewizję modelu, uruchamia wskazaną binarkę,
oczekuje inicjalizacji danych i kończy wyłącznie proces, który sam uruchomił.
Przełącznik `-KeepRunning` pozostawia go aktywnym.

## Dane lokalne

- ustawienia, historia i nagrania: `%APPDATA%\pl.mow.desktop`;
- model: `%USERPROFILE%\.cache\huggingface\hub` albo ścieżki wynikające z
  `HF_HOME` / `HF_HUB_CACHE`;
- dane modelu i nagrania nie są dołączane do repozytorium.

## Rozwiązywanie problemów

- **Brak mikrofonu:** w Ustawieniach Windows włącz dostęp do mikrofonu dla
  aplikacji klasycznych, wybierz właściwe urządzenie w Mów i zamknij programy
  używające go wyłącznie.
- **Nie znaleziono Pythona:** sprawdź `python --version` lub ustaw
  `MOW_PYTHON=C:\pełna\ścieżka\python.exe`. Aplikacja próbuje kolejno
  `MOW_PYTHON`, `python` i launcher `py -3.13`, bez powłoki.
- **CUDA niedostępna:** uruchom
  `python -c "import torch; print(torch.cuda.is_available(), torch.version.cuda)"`.
  Nie instaluj CPU-only PyTorch przez `requirements.txt`.
- **Model niegotowy:** ponów `.\scripts\download-model.ps1`; skrypt używa cache
  i pobierze tylko brakujące artefakty.
- **Skrót nie działa:** inny program może przechwytywać `Ctrl+Space`; zmień
  skrót w Ustawieniach Mów.
- **Okno znika:** zamknięcie głównego okna chowa je do zasobnika. Dwuklik/klik
  ikony Mów albo pozycja „Open Mów” przywraca okno.
