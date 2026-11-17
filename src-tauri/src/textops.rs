//! Rule-based edits to a finished transcript: hesitation removal, spoken
//! commands ("nowa linia", "wyślij"), snippets and per-app formatting.
//!
//! Everything here is plain text work that runs in microseconds; no model is
//! involved. Order matters and is fixed by [`polish`]: fillers go first so a
//! command is recognised even after an "yyy", snippets are expanded before
//! commands so a snippet may contain the words of a command, and the app
//! profile is applied last to the text that will actually be pasted.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FillerLevel {
    Off,
    /// Hesitation sounds only: "yyy", "eee", "hmm", "um".
    #[default]
    Light,
    /// Also a word said twice in a row ("to to") and the stock filler
    /// phrases of speech ("w sensie", "no więc", "you know").
    Strong,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// What the user says ("mój mail").
    pub trigger: String,
    /// What is pasted instead ("jan@example.com"). May span lines.
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppProfile {
    /// Process name the profile applies to, without ".exe" ("code",
    /// "windowsterminal", "slack"). Compared case-insensitively.
    pub app: String,
    /// Chats rarely end a message with a period.
    pub no_trailing_period: bool,
    /// Terminals and search boxes do not want a capital first letter.
    pub lowercase_start: bool,
    /// Press Enter after pasting, to send a chat message or run a command.
    pub press_enter: bool,
}

/// What the delivery should do after pasting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polished {
    pub text: String,
    pub press_enter: bool,
}

pub struct Options<'a> {
    pub fillers: FillerLevel,
    pub commands: bool,
    pub snippets: &'a [Snippet],
    pub profile: Option<&'a AppProfile>,
}

pub fn polish(text: &str, options: &Options<'_>) -> Polished {
    let mut text = remove_fillers(text, options.fillers);
    text = expand_snippets(&text, options.snippets);
    let mut press_enter = false;
    if options.commands {
        let (commanded, send) = apply_commands(&text);
        text = commanded;
        press_enter = send;
    }
    if let Some(profile) = options.profile {
        text = apply_profile(&text, profile);
        press_enter |= profile.press_enter;
    }
    // Nothing left to send: never press Enter on an empty field.
    Polished { press_enter: press_enter && !text.trim().is_empty(), text }
}

/// Process name as profiles compare it: lower case, no directory, no ".exe".
pub fn app_key(name: &str) -> String {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name).trim().to_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_owned()
}

pub fn profile_for<'a>(profiles: &'a [AppProfile], app: Option<&str>) -> Option<&'a AppProfile> {
    let app = app_key(app?);
    profiles.iter().find(|profile| !profile.app.trim().is_empty() && app_key(&profile.app) == app)
}

/// Trims and bounds what Settings sends: empty rows are dropped, and the
/// lists stay small enough that matching every take costs nothing.
pub fn validate(snippets: &mut Vec<Snippet>, profiles: &mut Vec<AppProfile>) -> Result<(), String> {
    snippets.retain(|snippet| !snippet.trigger.trim().is_empty() && !snippet.text.is_empty());
    for snippet in snippets.iter_mut() {
        snippet.trigger = snippet.trigger.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    profiles.retain(|profile| !profile.app.trim().is_empty());
    for profile in profiles.iter_mut() {
        profile.app = app_key(&profile.app);
    }
    if snippets.len() > 200 || profiles.len() > 100 {
        return Err("Too many snippets or app profiles.".into());
    }
    if snippets.iter().any(|snippet| snippet.trigger.chars().count() > 80 || snippet.text.chars().count() > 5000) {
        return Err("A snippet is too long.".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fillers
// ---------------------------------------------------------------------------

/// Sounds of hesitation, never words: safe to drop at any level.
static HESITATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:y{2,}m*|y+m+|e{2,}[hm]*|eh+m*|em{2,}|hm+|m{3,}|u+h+m*|u+m+|a+h{2,})$")
        .expect("static hesitation regex is valid")
});

/// Stock filler phrases removed at the strong level, each only when it
/// stands between commas or at a sentence start, where it carries no meaning.
static FILLER_PHRASES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[.!?]\s+|,\s*)(?:no więc|no to|w sensie|znaczy się|znaczy|jakby|you know|i mean|kind of|sort of|basically|well)\s*,\s*")
        .expect("static filler regex is valid")
});

/// Removes the stock phrases; a sentence that now starts with the next word
/// gets its capital back ("W sensie, jutro" becomes "Jutro").
fn drop_filler_phrases(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for found in FILLER_PHRASES.captures_iter(text) {
        let whole = found.get(0).expect("group 0 always matches");
        let lead = found.get(1).map_or("", |lead| lead.as_str());
        out.push_str(&text[at..whole.start()]);
        out.push_str(lead);
        at = whole.end();
        if !lead.starts_with(',') && let Some(next) = text[at..].chars().next() {
            out.extend(next.to_uppercase());
            at += next.len_utf8();
        }
    }
    out.push_str(&text[at..]);
    out
}

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}']+").expect("static word regex is valid"));

pub fn remove_fillers(text: &str, level: FillerLevel) -> String {
    if level == FillerLevel::Off {
        return text.to_owned();
    }
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut previous: Option<(usize, usize, String)> = None;
    for word in WORD.find_iter(text) {
        let lower = word.as_str().to_lowercase();
        if HESITATION.is_match(&lower) {
            // The word and the commas or ellipsis it carries.
            let tail = text[word.end()..].chars().take_while(|c| matches!(c, ',' | '…')).map(char::len_utf8).sum::<usize>();
            cuts.push((word.start(), word.end() + tail));
            continue;
        }
        if level == FillerLevel::Strong
            && let Some((_, end, last)) = &previous
            && *last == lower
            && text[*end..word.start()].chars().all(|c| c == ' ' || c == ',')
            && !INTENSIFIERS.contains(&lower.as_str())
        {
            // "to to": drop the second one and what joins it.
            cuts.push((*end, word.end()));
        }
        previous = Some((word.start(), word.end(), lower));
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in cuts {
        if start >= at {
            out.push_str(&text[at..start]);
            at = end;
        }
    }
    out.push_str(&text[at..]);
    if level == FillerLevel::Strong {
        out = drop_filler_phrases(&out);
    }
    tidy(&out)
}

const INTENSIFIERS: &[&str] = &["bardzo", "very", "dużo", "długo", "daleko", "no", "tak", "nie", "yes", "ha", "hej", "bye"];

/// Repairs what removing words leaves behind: doubled spaces and commas,
/// a comma that now opens the text or a sentence, a space before a mark.
fn tidy(text: &str) -> String {
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").expect("valid"));
    static COMMAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",(\s*,)+").expect("valid"));
    static OPENING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[.!?]\s+|\n)\s*,\s*").expect("valid"));
    static BEFORE_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+([,.;:!?])").expect("valid"));
    static COMMA_STOP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*([.!?])").expect("valid"));
    let text = COMMAS.replace_all(text, ",");
    let text = OPENING.replace_all(&text, "$1");
    let text = BEFORE_MARK.replace_all(&text, "$1");
    let text = COMMA_STOP.replace_all(&text, "$1");
    let text = SPACES.replace_all(&text, " ");
    let text = text.trim().trim_start_matches([',', ';', '.', '…']).trim_start();
    // Nothing but marks left: the take was only hesitation.
    if !text.chars().any(char::is_alphanumeric) {
        return String::new();
    }
    recapitalize(text)
}

// ---------------------------------------------------------------------------
// Snippets
// ---------------------------------------------------------------------------

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ą' => 'a', 'ć' => 'c', 'ę' => 'e', 'ł' => 'l', 'ń' => 'n', 'ó' => 'o', 'ś' => 's', 'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

/// Replaces each spoken trigger with its stored text. A trigger matches as
/// whole words, ignoring case, Polish diacritics and the punctuation the
/// recogniser puts around it; a whole take that is only the trigger
/// becomes only the snippet, without a stray period.
pub fn expand_snippets(text: &str, snippets: &[Snippet]) -> String {
    let mut out = text.to_owned();
    // Longest first, so "mój mail służbowy" wins over "mój mail".
    let mut ordered: Vec<&Snippet> = snippets.iter().filter(|snippet| !snippet.trigger.trim().is_empty()).collect();
    ordered.sort_by_key(|snippet| std::cmp::Reverse(snippet.trigger.chars().count()));
    for snippet in ordered {
        let words: Vec<String> = snippet.trigger.split_whitespace().map(|word| regex::escape(&fold(word))).collect();
        if words.is_empty() {
            continue;
        }
        // Match on a folded copy, then splice into the original by byte
        // offsets: folding keeps character counts, so map through chars.
        let pattern = format!(r"(?i)(^|[^\p{{L}}\p{{N}}])({})([.,!?;:]?)(?:[^\p{{L}}\p{{N}}]|$)", words.join(r"[\s,.-]+"));
        let Ok(regex) = Regex::new(&pattern) else { continue };
        loop {
            let folded = fold(&out);
            let Some(found) = regex.captures(&folded) else { break };
            let whole = found.get(2).expect("group 2 always matches");
            let mark = found.get(3).map_or(0, |mark| mark.end() - mark.start());
            let (start, end) = (char_offset(&folded, &out, whole.start()), char_offset(&folded, &out, whole.end() + mark));
            let alone = out[..start].trim().is_empty() && out[end..].trim().is_empty();
            let kept_mark = if alone { "" } else { &out[char_offset(&folded, &out, whole.end())..end] };
            let kept_mark = if kept_mark == "." && !out[end..].trim().is_empty() { "" } else { kept_mark };
            out = format!("{}{}{}{}", &out[..start], snippet.text, kept_mark, &out[end..]);
        }
    }
    out
}

/// Byte offset in `original` of the character at `folded_offset` in `folded`.
fn char_offset(folded: &str, original: &str, folded_offset: usize) -> usize {
    let index = folded[..folded_offset].chars().count();
    original.char_indices().nth(index).map_or(original.len(), |(offset, _)| offset)
}

// ---------------------------------------------------------------------------
// Spoken commands
// ---------------------------------------------------------------------------

static NEW_PARAGRAPH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[,.;:]?\s*\b(?:nowy akapit|new paragraph)\b[.,!?;:]*\s*").expect("valid")
});
static NEW_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[,.;:]?\s*\b(?:nowa linia|nowa linijka|nowy wiersz|następna linia|new line|next line)\b[.,!?;:]*\s*").expect("valid")
});
/// "Skasuj to" removes the sentence said just before it.
static SCRATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|(?:[.!?…]|\n)\s*)?[^.!?…\n]*?[,.]?\s*\b(?:skasuj to|usuń to|wymaż to|scratch that|delete that)\b[.,!?]*\s*").expect("valid")
});
/// A send command only counts at the very end of the take.
static SEND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[,.;:]?\s*\b(?:wyślij|wyślij to|wyślij wiadomość|enter|send it|send|press enter)[.,!?]*\s*$").expect("valid")
});

/// Whether the take ends with a send command ("wyślij", "send it").
pub fn ends_with_send(text: &str) -> bool {
    SEND.is_match(text)
}

/// Applies the spoken commands. Returns the text and whether the take ended
/// with a send command.
pub fn apply_commands(text: &str) -> (String, bool) {
    let mut out = text.to_owned();
    let send = SEND.is_match(&out);
    if send {
        out = SEND.replace(&out, "").into_owned();
        // The sentence before the command keeps its period.
        if !out.trim_end().ends_with(['.', '!', '?', '…']) && !out.trim().is_empty() {
            out = format!("{}.", out.trim_end());
        }
    }
    while let Some(found) = SCRATCH.find(&out) {
        let removed = found.as_str();
        // Keep the end of the previous sentence that the match swallowed.
        let lead: String = removed.chars().take_while(|c| matches!(c, '.' | '!' | '?' | '…' | '\n')).collect();
        let separator = if lead.is_empty() || found.end() == out.len() { lead } else { format!("{lead} ") };
        out = format!("{}{}{}", &out[..found.start()], separator, &out[found.end()..]);
    }
    out = NEW_PARAGRAPH.replace_all(&out, ".\n\n").into_owned();
    out = NEW_LINE.replace_all(&out, ".\n").into_owned();
    (finish_lines(&out), send)
}

/// After commands: a break directly after a sentence mark does not need the
/// period the command added; breaks at the very start or end are dropped;
/// every line starts with a capital.
fn finish_lines(text: &str) -> String {
    static DOUBLE_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([.!?…:])\.(\n)").expect("valid"));
    static LONE_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\n)\.(\n|$)").expect("valid"));
    static LINE_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]*\n[ \t]*").expect("valid"));
    let text = LINE_SPACES.replace_all(text, "\n");
    let text = DOUBLE_MARK.replace_all(&text, "$1$2");
    let text = LONE_MARK.replace_all(&text, "$1$2");
    let text = text.trim_matches(|c: char| c == '\n' || c == ' ').trim_start_matches('.');
    recapitalize(text.trim_start())
}

/// Capitalises the first letter of the text and of each line.
fn recapitalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    for c in text.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            start = false;
            continue;
        }
        if c == '\n' {
            start = true;
        } else if !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | '„' | '-' | '•') {
            start = false;
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// App profiles
// ---------------------------------------------------------------------------

pub fn apply_profile(text: &str, profile: &AppProfile) -> String {
    let mut out = text.to_owned();
    if profile.no_trailing_period && out.ends_with('.') && !out.ends_with("..") {
        out.pop();
    }
    if profile.lowercase_start {
        let mut chars = out.chars();
        if let Some(first) = chars.next() {
            // "GitHub" or "API" stay as they are: only a plain capital that
            // sentence casing added is lowered.
            let second = chars.clone().next();
            if first.is_uppercase() && second.is_none_or(|c| !c.is_uppercase()) {
                out = first.to_lowercase().chain(chars).collect();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hesitations_go_but_words_stay() {
        assert_eq!(remove_fillers("Yyy, no to jutro eee spotkanie.", FillerLevel::Light), "No to jutro spotkanie.");
        assert_eq!(remove_fillers("Um, I think hmm we should go.", FillerLevel::Light), "I think we should go.");
        // Real words that look like sounds survive.
        assert_eq!(remove_fillers("Ten hem był krzywy, a my mamy mem.", FillerLevel::Light), "Ten hem był krzywy, a my mamy mem.");
        assert_eq!(remove_fillers("Yyy.", FillerLevel::Light), "");
        assert_eq!(remove_fillers("Yyy, tekst", FillerLevel::Off), "Yyy, tekst");
    }

    #[test]
    fn strong_level_drops_repeats_and_stock_phrases() {
        assert_eq!(remove_fillers("Zrób to to jutro, że że działa.", FillerLevel::Strong), "Zrób to jutro, że działa.");
        assert_eq!(remove_fillers("To jest bardzo bardzo ważne.", FillerLevel::Strong), "To jest bardzo bardzo ważne.");
        assert_eq!(remove_fillers("No więc, idziemy. W sensie, jutro.", FillerLevel::Strong), "Idziemy. Jutro.");
        assert_eq!(remove_fillers("Zrób to to jutro.", FillerLevel::Light), "Zrób to to jutro.");
    }

    #[test]
    fn line_and_paragraph_commands_break_the_text() {
        assert_eq!(apply_commands("Cześć. Nowa linia. Jak się masz?"), ("Cześć.\nJak się masz?".into(), false));
        assert_eq!(apply_commands("Lista zakupów nowa linia mleko nowa linia chleb"), ("Lista zakupów.\nMleko.\nChleb".into(), false));
        assert_eq!(apply_commands("Pierwszy punkt. Nowy akapit. Drugi punkt."), ("Pierwszy punkt.\n\nDrugi punkt.".into(), false));
        assert_eq!(apply_commands("First line, new line, second line."), ("First line.\nSecond line.".into(), false));
        // Inflected, ordinary use is not a command.
        assert_eq!(apply_commands("Otworzyli nową linię metra."), ("Otworzyli nową linię metra.".into(), false));
    }

    #[test]
    fn send_counts_only_at_the_end() {
        assert_eq!(apply_commands("Będę za pięć minut. Wyślij."), ("Będę za pięć minut.".into(), true));
        assert_eq!(apply_commands("Będę za pięć minut wyślij"), ("Będę za pięć minut.".into(), true));
        assert_eq!(apply_commands("Wyślij mi ten plik jutro."), ("Wyślij mi ten plik jutro.".into(), false));
        assert_eq!(apply_commands("See you soon, send it."), ("See you soon.".into(), true));
    }

    #[test]
    fn scratch_that_removes_the_sentence_before_it() {
        assert_eq!(apply_commands("Spotkajmy się o piątej. Albo o szóstej, skasuj to. O siódmej."), ("Spotkajmy się o piątej. O siódmej.".into(), false));
        assert_eq!(apply_commands("Głupie zdanie. Skasuj to."), ("".into(), false));
        assert_eq!(apply_commands("Dobre zdanie. Złe zdanie, skasuj to."), ("Dobre zdanie.".into(), false));
    }

    #[test]
    fn snippets_expand_in_place_and_alone() {
        let snippets = [
            Snippet { trigger: "mój mail".into(), text: "jan@example.com".into() },
            Snippet { trigger: "mój mail służbowy".into(), text: "jan@firma.pl".into() },
            Snippet { trigger: "podpis".into(), text: "Pozdrawiam,\nJan".into() },
        ];
        assert_eq!(expand_snippets("Mój mail.", &snippets), "jan@example.com");
        assert_eq!(expand_snippets("Wyślij to na mój mail, dzięki.", &snippets), "Wyślij to na jan@example.com, dzięki.");
        assert_eq!(expand_snippets("Pisz na moj mail sluzbowy.", &snippets), "Pisz na jan@firma.pl.");
        assert_eq!(expand_snippets("Podpis", &snippets), "Pozdrawiam,\nJan");
        assert_eq!(expand_snippets("Podpisany dokument.", &snippets), "Podpisany dokument.");
    }

    #[test]
    fn profiles_follow_the_target_app() {
        let profiles = [
            AppProfile { app: "Slack".into(), no_trailing_period: true, press_enter: true, ..Default::default() },
            AppProfile { app: "WindowsTerminal.exe".into(), lowercase_start: true, no_trailing_period: true, ..Default::default() },
        ];
        assert_eq!(profile_for(&profiles, Some("C:\\Apps\\slack.exe")).unwrap().app, "Slack");
        assert!(profile_for(&profiles, Some("code")).is_none());
        assert!(profile_for(&profiles, None).is_none());
        let terminal = profile_for(&profiles, Some("windowsterminal")).unwrap();
        assert_eq!(apply_profile("Git status.", terminal), "git status");
        assert_eq!(apply_profile("API działa.", terminal), "API działa");
        let options = Options { fillers: FillerLevel::Light, commands: true, snippets: &[], profile: profile_for(&profiles, Some("slack")) };
        assert_eq!(polish("Yyy, będę za chwilę.", &options), Polished { text: "Będę za chwilę".into(), press_enter: true });
        assert_eq!(polish("Yyy.", &options), Polished { text: String::new(), press_enter: false });
    }
}
