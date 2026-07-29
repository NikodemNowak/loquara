# Mów — projekt aplikacji do dyktowania na Windows

## Cel

Mów ma zastąpić płatne aplikacje Wispr Flow i Superwhisper w codziennym dyktowaniu na Windows 11. Pierwszy wynik musi działać lokalnie, reagować natychmiast na globalny skrót, zapisywać nagranie przed transkrypcją, automatycznie wklejać gotowy tekst do poprzednio aktywnej aplikacji i umożliwiać odzyskanie lub ponowienie każdego nieudanego nagrania.

Opis użytkownika jest zatwierdzonym briefem. Zgodnie z jego instrukcją proces nie zatrzymuje się na dodatkowe pytania ani osobną akceptację makiet.

## Wnioski z researchu

Wispr Flow wyróżnia się czyszczeniem wypełniaczy, rozumieniem samokorekty, kontekstem aplikacji, słownikiem, długimi sesjami, historią i odzyskiwaniem nieudanych transkrypcji. Superwhisper wyróżnia się trybami, wyborem modeli lokalnych i chmurowych, historią audio, konfigurowalnymi skrótami oraz auto-wklejaniem. Na Windows część funkcji Superwhisper nadal jest niepełna, więc Mów ma przewagę przez konsekwentne wsparcie Windows od pierwszej wersji.

NVIDIA Parakeet TDT 0.6B v3 obsługuje polski, automatyczną interpunkcję i kapitalizację, ma licencję CC BY 4.0 i jest wystarczająco mały dla lokalnego, szybkiego działania. Cohere Transcribe 2B jest interesującą alternatywą Apache 2.0, ale jest cięższy. Domyślnym silnikiem tej wersji będzie Parakeet, a interfejs silnika pozostanie wymienny.

## Zakres produktu

### Dyktowanie

- Globalny skrót domyślny `Ctrl+Space`, zmieniany w ustawieniach.
- Dwa tryby skrótu: toggle oraz push-to-talk.
- `Esc` anuluje aktywne nagranie.
- Małe okno nakładki pokazuje stany: gotowy, słucham, przepisuję, wklejam, błąd.
- Nagranie jest najpierw bezpiecznie zapisane jako WAV, dopiero potem wysyłane do silnika.
- Po sukcesie tekst trafia do schowka i jest automatycznie wklejany do poprzednio aktywnego pola tekstowego.
- Po błędzie nagranie pozostaje w historii i można ponowić transkrypcję.
- Menu zasobnika pozwala zacząć/zatrzymać nagrywanie, wkleić ostatni tekst, otworzyć aplikację i ją zakończyć.

### Jakość tekstu

- Domyślna transkrypcja lokalna NVIDIA Parakeet.
- Automatyczne wykrywanie polskiego i angielskiego przez model.
- Lekki deterministyczny post-processing: białe znaki, spacje przed interpunkcją i reguły słownika.
- Słownik użytkownika wspiera zamiany `usłyszano → zapisuj jako`.
- Tryby `Czysty`, `Wiadomość` i `Kod` określają poziom post-processingu bez obowiązkowej chmury.

### Historia i odzyskiwanie

- Każdy wpis zawiera czas, długość, status, transkrypcję, model i ścieżkę audio.
- Akcje: kopiuj, wklej, odtwórz, ponów, usuń.
- Wyszukiwanie tekstowe i filtry statusu.
- Domyślnie audio jest przechowywane przez 30 dni; można wybrać 1 dzień, 7 dni, 30 dni lub bezterminowo.

### Ustawienia

- Mikrofon, skrót, tryb toggle/push-to-talk, auto-wklejanie i dźwięki.
- Motyw systemowy, jasny lub ciemny.
- Uruchamianie z systemem.
- Stan modelu: niezainstalowany, pobieranie, gotowy, błąd.
- Lokalizacja danych i przycisk testu mikrofonu.

## Kierunek wizualny

Interfejs łączy strukturę ciemnej makiety z czytelnością jasnej. Nie używa gradientów, neonów, szkła, fioletu, maskotek ani dekoracyjnych „AI” elementów.

- Typografia: `Segoe UI Variable`, zwarta skala 12–28 px.
- Promień: 6–10 px, bez wielkich kapsuł.
- Kolor akcentu: kobalt w stanie neutralnym, cynober wyłącznie podczas nagrywania i przy błędzie.
- Nawigacja: wąski panel po lewej; treść jest płaską listą i sekcjami, nie zbiorem nadmuchanych kart.
- Nakładka: 312×56 px, wyśrodkowana nad paskiem zadań, always-on-top i bez przejmowania fokusu.
- Motyw jasny: papierowe `#F5F3EE`, biel i atrament.
- Motyw ciemny: `#17181A`, `#222428` i ciepła biel.

Makiety źródłowe są zapisane w `docs/designs/`.

## Architektura

### Aplikacja

Tauri 2 + React + TypeScript zapewniają lekkie okno i szybkie iterowanie nad wyglądem. Rust obsługuje zasobnik, skróty globalne, nagrywanie, schowek, wklejanie, pliki i proces silnika. React odpowiada wyłącznie za stan widoku i interakcje.

### Moduły Rust

- `audio`: urządzenia `cpal`, zapis WAV i poziomy fali.
- `dictation`: maszyna stanów i koordynacja nagranie → transkrypcja → wklejenie.
- `transcription`: klient procesu roboczego i wymienny interfejs silnika.
- `storage`: konfiguracja i historia w SQLite.
- `platform`: skróty, zasobnik, okna, schowek i symulacja `Ctrl+V`.

### Silnik modelu

Proces `engine/parakeet_worker.py` komunikuje się liniami JSON przez stdin/stdout. Ładuje model raz do VRAM, przyjmuje ścieżkę WAV i zwraca tekst lub ustrukturyzowany błąd. Oddzielenie procesu chroni UI przed awarią biblioteki ML i umożliwia późniejsze dodanie Cohere lub Whisper.

### Dane

SQLite przechowuje metadane. Pliki audio pozostają w katalogu danych aplikacji. Ustawienia nie zawierają sekretów. Żadne audio nie opuszcza komputera.

## Przepływ i obsługa błędów

1. Skrót zapamiętuje aktywne okno i rozpoczyna nagrywanie.
2. Nakładka przechodzi do `recording`, a poziomy audio są emitowane do UI.
3. Zatrzymanie atomowo zamyka WAV i tworzy wpis `processing`.
4. Worker transkrybuje plik. Timeout lub awaria procesu daje status `failed`, zachowując audio.
5. Po sukcesie wynik jest normalizowany, zapisany, skopiowany i wklejony.
6. Jeśli wklejenie się nie uda, wpis pozostaje `completed`, a nakładka pokazuje akcję ponownego wklejenia.
7. Ponów używa istniejącego pliku WAV, bez ponownego nagrywania.

## Kryteria akceptacji

- Aplikacja uruchamia się na Windows 11 i pozostaje w zasobniku po zamknięciu okna.
- `Ctrl+Space` rozpoczyna i kończy nagrywanie w dowolnej aplikacji.
- Nakładka pokazuje poprawny stan bez zabierania fokusu.
- Udana transkrypcja pojawia się w historii i jest automatycznie wklejana.
- Nieudana transkrypcja zachowuje audio i ma działające `Ponów`.
- Motywy jasny, ciemny i systemowy są spójne.
- Aplikacja nie wykonuje żądań sieciowych podczas dyktowania po pobraniu modelu.
- Testy jednostkowe obejmują maszynę stanów, post-processing, historię i protokół workera.

