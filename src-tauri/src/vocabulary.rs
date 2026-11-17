//! The user's vocabulary applied to a finished transcript.
//!
//! An entry is the spelling the user wants ("Claude Code", "try again",
//! "Loquara") plus optional "heard as" aliases. Parakeet renders English and
//! brand words through Polish spelling ("kloda", "Clad Cod", "tragen",
//! "szerpa ONX"), so besides exact aliases every run of one to three words is
//! compared with each term by a phonetic key: Polish and English graphemes
//! folded onto one alphabet, then a weighted edit distance where vowels and
//! voicing pairs are cheap and consonants are not. A fuzzy match must also
//! keep the consonant skeleton and the first sound, short terms need an
//! exact key, and words the user says often for their own sake are left
//! alone. Pure string work: microseconds per transcript, no model.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Polish case endings a spoken term can carry ("Claude'a", "GitHuby").
const SUFFIXES: &[&str] = &["owi", "ach", "ami", "ów", "em", "om", "ie", "ze", "a", "e", "u", "y", "i"];
/// Fuzzy matches are only trusted from this many key characters up.
const FUZZY_MIN_KEY: usize = 7;

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// Byte range in the original text.
    pub start: usize,
    pub end: usize,
    pub term: usize,
    pub replacement: String,
    /// Weighted key distance; 0 for exact keys and aliases.
    pub distance: f32,
}

struct Term {
    text: String,
    lower: String,
    key: String,
    skeleton: String,
    alias_keys: Vec<String>,
    words: usize,
    /// "GitHub", "iPhone", "Claude": the term's own casing always wins.
    case_specific: bool,
    /// A word that takes Polish endings ("Codexa"); acronyms, phrases and
    /// words ending in a vowel sound ("Loquara", which becomes "Loquary") do not.
    inflects: bool,
}

pub struct Vocabulary {
    terms: Vec<Term>,
    protected: Arc<HashSet<String>>,
}

#[derive(Clone, Copy)]
struct Word<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

impl Vocabulary {
    pub fn new<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [String])>) -> Self {
        let terms = entries
            .into_iter()
            .filter_map(|(text, aliases)| {
                let text = text.trim();
                let key = phonetic_key(text);
                (!key.is_empty()).then(|| Term {
                    words: words(text).len().max(1),
                    case_specific: text.chars().any(char::is_uppercase),
                    inflects: !text.contains(char::is_whitespace) && text.chars().any(char::is_lowercase) && key.len() >= 4 && !key.ends_with(is_vowel),
                    skeleton: skeleton(&key),
                    alias_keys: aliases.iter().map(|alias| phonetic_key(alias)).filter(|key| !key.is_empty()).collect(),
                    text: text.to_owned(),
                    lower: text.to_lowercase(),
                    key,
                })
            })
            .collect();
        Self { terms, protected: Arc::default() }
    }

    /// The user's everyday words (see [`frequent_words`]): never corrected
    /// by a near match, only by an exact sound or an alias.
    pub fn protect(mut self, words: Arc<HashSet<String>>) -> Self {
        self.protected = words;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        if self.terms.is_empty() {
            return text.to_owned();
        }
        let mut result = String::with_capacity(text.len() + 16);
        let mut cursor = 0;
        for found in self.matches(text) {
            result.push_str(&text[cursor..found.start]);
            result.push_str(&found.replacement);
            cursor = found.end;
        }
        result.push_str(&text[cursor..]);
        result
    }

    /// Accepted, non-overlapping replacements in text order.
    pub fn matches(&self, text: &str) -> Vec<Match> {
        let mut candidates = Vec::new();
        self.scan(text, |candidate| candidates.push(candidate));
        // Best first: exact before fuzzy, then the longer span.
        candidates.sort_by(|a, b| {
            a.distance.total_cmp(&b.distance).then((b.end - b.start).cmp(&(a.end - a.start))).then(a.start.cmp(&b.start))
        });
        let mut taken: Vec<Match> = Vec::new();
        for candidate in candidates {
            if taken.iter().all(|kept| candidate.end <= kept.start || candidate.start >= kept.end) {
                taken.push(candidate);
            }
        }
        taken.sort_by_key(|found| found.start);
        // A span already written the way the user wants claims its place
        // without changing anything.
        taken.retain(|found| text[found.start..found.end] != found.replacement);
        taken
    }

    /// Spans that sound like a term but were not close enough to replace:
    /// `(term, heard)` pairs the user may want to add as aliases.
    pub fn near_misses(&self, text: &str) -> Vec<(usize, String)> {
        let accepted = self.matches(text);
        let mut misses = Vec::new();
        self.spans(text, 3, |span| {
            if accepted.iter().any(|found| span.end > found.start && span.start < found.end) {
                return;
            }
            let short_words = span.words > 1 && span.original.split([' ', '-']).any(|word| word.chars().count() < 3);
            for (index, term) in self.terms.iter().enumerate() {
                if !short_words && self.score(term, span).is_none() && let Some(heard) = self.near_miss(term, span) {
                    misses.push((index, heard));
                }
            }
        });
        misses.sort();
        misses.dedup_by(|a, b| a.0 == b.0 && a.1.to_lowercase() == b.1.to_lowercase());
        misses
    }

    pub fn term(&self, index: usize) -> &str {
        &self.terms[index].text
    }

    /// Every run of up to `max_words` words that could stand for one term.
    fn spans<'s>(&self, text: &'s str, max_words: usize, mut visit: impl FnMut(&Span<'s>)) {
        let words = words(text);
        let keys: Vec<String> = words.iter().map(|word| phonetic_key(word.text)).collect();
        let lowers: Vec<String> = words.iter().map(|word| word.text.to_lowercase()).collect();
        let inflecting = self.terms.iter().any(|term| term.inflects);
        for first in 0..words.len() {
            for count in 1..=max_words.min(words.len() - first) {
                let span = &words[first..first + count];
                // One-letter words ("i", "w", "z") are Polish conjunctions
                // and prepositions, never a syllable of a term.
                if (count > 1 && span.iter().any(|word| word.text.chars().count() < 2))
                    || !joinable(text, span)
                    || span.iter().any(|word| looks_like_code(word.text))
                {
                    break;
                }
                let (start, end, last) = (span[0].start, span[count - 1].end, span[count - 1]);
                let original = &text[start..end];
                let key = collapse(&keys[first..first + count].concat());
                if key.is_empty() {
                    continue;
                }
                let lower = original.to_lowercase();
                let mut stems = Vec::new();
                if inflecting {
                    let last_lower = &lowers[first + count - 1];
                    for suffix in SUFFIXES {
                        let Some(stem) = last_lower.strip_suffix(suffix) else { continue };
                        let stem = stem.trim_end_matches(['\'', '\u{2019}']);
                        if stem.chars().count() >= 3 && last.text.is_char_boundary(last.text.len() - suffix.len()) {
                            let head = keys[first..first + count - 1].concat();
                            let ending = &last.text[last.text.len() - suffix.len()..];
                            stems.push(Stem { ending, key: collapse(&(head + &phonetic_key(stem))), common: common(stem) });
                        }
                    }
                }
                let everyday = |word: &String| common(word) || self.protected.contains(word);
                let ordinary = &lowers[first..first + count];
                visit(&Span {
                    start, end, original, key, stems, words: count,
                    common: count == 1 && common(&lower),
                    loose: if count == 1 { !everyday(&lower) } else { !ordinary.iter().any(|word| common(word)) },
                    lower,
                });
            }
        }
    }

    fn max_words(&self) -> usize {
        self.terms.iter().map(|term| term.words + 1).max().unwrap_or(1).min(4)
    }

    fn scan(&self, text: &str, mut emit: impl FnMut(Match)) {
        self.spans(text, self.max_words(), |span| {
            for (index, term) in self.terms.iter().enumerate() {
                if span.words <= term.words + 1
                    && let Some((distance, ending)) = self.score(term, span)
                {
                    let replacement = render(term, span.original, ending);
                    emit(Match { start: span.start, end: span.end, term: index, replacement, distance });
                }
            }
        });
    }

    /// `Some((distance, ending))` when the span should become `term`.
    fn score<'s>(&self, term: &Term, span: &Span<'s>) -> Option<(f32, &'s str)> {
        if span.lower == term.lower || term.alias_keys.contains(&span.key) {
            return Some((0.0, ""));
        }
        // A common word is never rewritten, not even when it sounds exactly
        // like a term ("kursor" is not "Cursor").
        if span.common {
            return None;
        }
        // Three sounds are too few to trust: "Wyte" is not "Vite" unless it
        // is also spelt almost the same.
        if term.key.len() <= 3 && edit_distance(&fold(&span.lower), &fold(&term.lower)) > 1 {
            return None;
        }
        // "pull requeście" is the phrase already spelt right, with a Polish
        // ending a phrase term cannot carry: leave it as it is.
        if term.words > 1 && span.lower.len() > term.lower.len() && span.lower.starts_with(&term.lower) {
            return None;
        }
        if span.key == term.key {
            // "Excele" sounds like "Excel" too; keeping its ending keeps it
            // whole. "Taurii" is only "Tauri" said slowly.
            let ending = span.stems.iter().find(|stem| {
                let written = span.lower.get(..span.lower.len().saturating_sub(stem.ending.len())).unwrap_or_default();
                !stem.common && stem.key == term.key && term.inflects && !term.lower.ends_with(&stem.ending.to_lowercase())
                    && edit_distance(&fold(written), &fold(&term.lower)) <= 1
            });
            return Some((0.0, ending.map_or("", |stem| stem.ending)));
        }
        // The user's own everyday words, and phrases built from them, are
        // only replaced when they sound exactly like the term.
        let fuzzy = |key: &str| if span.loose { fuzzy(term, key) } else { None };
        let mut best = fuzzy(&span.key).map(|distance| (distance, ""));
        // "kloda" is "Claude" with a Polish ending: keep the ending.
        if term.inflects {
            for stem in &span.stems {
                let found = if term.alias_keys.contains(&stem.key) {
                    Some(0.0)
                } else if stem.common {
                    None
                } else if stem.key == term.key {
                    Some(0.0)
                } else {
                    fuzzy(&stem.key)
                };
                if let Some(distance) = found.map(|distance| distance + 0.25)
                    && best.is_none_or(|(current, _)| distance < current)
                {
                    best = Some((distance, stem.ending));
                }
            }
        }
        best
    }

    /// Close to a term but not replaced: an everyday word that would
    /// otherwise have matched, or a spelling a little too far off. Returns
    /// what an alias should say: the word without its Polish ending, so the
    /// alias then also covers the other forms ("kodeks" for "kodeksa").
    fn near_miss(&self, term: &Term, span: &Span<'_>) -> Option<String> {
        if term.key.len() < 5 || span.key.len() < 5 || span.words > term.words + 1 || span.lower.starts_with(&term.lower) {
            return None;
        }
        let near = |key: &str| {
            key == term.key
                || fuzzy(term, key).is_some()
                || (span.loose
                    && key.len().abs_diff(term.key.len()) <= 2
                    && key.get(..2) == term.key.get(..2)
                    && edit_distance(&skeleton(key), &term.skeleton) <= usize::from(term.key.len() >= 9)
                    && distance(key, &term.key) <= allowance(term.key.len()) + 1.0)
        };
        if let Some(stem) = span.stems.iter().find(|stem| term.inflects && near(&stem.key)) {
            let heard = &span.original[..span.original.len() - stem.ending.len()];
            return Some(heard.trim_end_matches(['\'', '\u{2019}']).to_owned());
        }
        near(&span.key).then(|| span.original.to_owned())
    }
}

struct Span<'s> {
    start: usize,
    end: usize,
    original: &'s str,
    lower: String,
    key: String,
    words: usize,
    /// A single common word: never rewritten.
    common: bool,
    /// No word of the span is an everyday word, so a near (not exact)
    /// match may be trusted.
    loose: bool,
    stems: Vec<Stem<'s>>,
}

/// The span read as an inflected word: its ending, and the key of the rest.
struct Stem<'s> {
    ending: &'s str,
    key: String,
    /// The rest is itself a common word ("kodeks-u"): never rewritten.
    common: bool,
}

static COMMON: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| include_str!("common_words.txt").split_whitespace().collect());

/// Frequent Polish and English words that no term may overwrite.
fn common(word: &str) -> bool {
    COMMON.contains(word)
}

/// Lower case without diacritics, for comparing spellings.
fn fold(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            'ą' => 'a', 'ć' => 'c', 'ę' => 'e', 'ł' => 'l', 'ń' => 'n', 'ó' => 'o', 'ś' => 's', 'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

fn fuzzy(term: &Term, key: &str) -> Option<f32> {
    if term.key.len() < FUZZY_MIN_KEY || key.len().abs_diff(term.key.len()) > 3 {
        return None;
    }
    if first_sound(key) != first_sound(&term.key) || edit_distance(&skeleton(key), &term.skeleton) > usize::from(term.key.len() >= 8) {
        return None;
    }
    let found = distance(key, &term.key);
    (found <= allowance(term.key.len())).then_some(found)
}

/// How far a key may drift from a term of `len` key characters.
fn allowance(len: usize) -> f32 {
    match len {
        0..=6 => 0.0,
        7..=9 => 1.0,
        _ => 1.5,
    }
}

fn render(term: &Term, original: &str, suffix: &str) -> String {
    let mut text = term.text.clone();
    if !suffix.is_empty() {
        // Polish writes an apostrophe after a vowel letter that is not
        // pronounced the Polish way ("Claude'a", but "Codexa"); a silent
        // "e" gives way to an ending that starts soft ("w Chromie").
        let silent_e = term.text.ends_with('e') && !term.key.ends_with('e');
        if silent_e && suffix.starts_with(['i', 'e']) {
            text.pop();
        } else if term.text.ends_with(|c: char| "aeiouy".contains(c.to_ascii_lowercase())) {
            text.push('\'');
        }
        text.push_str(suffix);
    }
    if !term.case_specific && original.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = text.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().chain(chars).collect();
        }
    }
    text
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut found = Vec::new();
    let mut start = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (position, &(index, character)) in chars.iter().enumerate() {
        // An apostrophe or hyphen inside a word keeps it whole ("Claude'a"),
        // and so do the marks of code, paths and addresses ("klod_2").
        let inner = matches!(character, '\'' | '’' | '-' | '_' | '.' | '/' | '\\' | '@' | ':')
            && start.is_some()
            && chars.get(position + 1).is_some_and(|(_, next)| next.is_alphanumeric());
        if character.is_alphanumeric() || inner {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            found.push(Word { start: begin, end: index, text: &text[begin..index] });
        }
    }
    if let Some(begin) = start {
        found.push(Word { start: begin, end: text.len(), text: &text[begin..] });
    }
    found
}

/// Words of one phrase are separated by spaces or a hyphen, never by a
/// sentence mark or a line break.
fn joinable(text: &str, span: &[Word<'_>]) -> bool {
    span.windows(2).all(|pair| {
        let gap = &text[pair[0].end..pair[1].start];
        gap.chars().all(|c| c == ' ' || c == '-') && gap.len() <= 3
    })
}

fn looks_like_code(word: &str) -> bool {
    word.contains(|c: char| c.is_ascii_digit() || matches!(c, '_' | '.' | '/' | '\\' | '@' | ':'))
        || word.chars().zip(word.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase())
}

/// A spelling-independent rendering of how a phrase sounds to a Polish ear.
pub fn phonetic_key(text: &str) -> String {
    let mut key = String::new();
    // "GitHub" is "git" + "hub", not "gi" + "th" + "ub".
    let mut split = String::with_capacity(text.len() + 4);
    let mut previous_lower = false;
    for character in text.chars() {
        if previous_lower && character.is_uppercase() {
            split.push(' ');
        }
        previous_lower = character.is_lowercase();
        split.extend(character.to_lowercase());
    }
    for word in split.split(|c: char| !c.is_alphanumeric()) {
        let mut folded = fold_word(word);
        // English silent "e" ("Claude", "Code", "remote").
        if folded.len() > 3 && folded.ends_with('e') && !folded[..folded.len() - 1].ends_with(is_vowel) {
            folded.pop();
        }
        key.push_str(&folded);
    }
    collapse(&key)
}

/// Repeated letters are one sound: "onnx", "Taurii".
fn collapse(key: &str) -> String {
    let mut collapsed = String::with_capacity(key.len());
    for character in key.chars() {
        if !collapsed.ends_with(character) {
            collapsed.push(character);
        }
    }
    collapsed
}

fn fold_word(word: &str) -> String {
    const RULES: &[(&str, &str)] = &[
        ("sch", "sh"), ("rz", "zh"), ("sz", "sh"), ("cz", "ch"), ("ph", "f"), ("wh", "v"),
        ("ck", "k"), ("qu", "kv"), ("ee", "i"), ("ea", "i"), ("oo", "u"), ("ou", "u"), ("au", "o"), ("ał", "o"),
        ("ai", "ej"), ("ay", "ej"), ("ei", "ej"), ("ey", "ej"), ("ą", "o"), ("ę", "e"), ("ó", "u"),
        ("ł", "u"), ("ń", "n"), ("ć", "ch"), ("ś", "sh"), ("ź", "zh"), ("ż", "zh"), ("q", "k"),
        ("x", "ks"), ("w", "v"), ("y", "i"),
    ];
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    'outer: while let Some(character) = rest.chars().next() {
        if character == 'c' {
            let after: Vec<char> = rest.chars().skip(1).take(2).collect();
            let (replacement, consumed) = match after.as_slice() {
                ['i', next, ..] if is_vowel(*next) => ("ch", 2),
                ['e' | 'i' | 'y', ..] => ("s", 1),
                ['h', ..] => ("ch", 2),
                _ => ("k", 1),
            };
            out.push_str(replacement);
            rest = &rest[consumed..];
            continue;
        }
        for (pattern, replacement) in RULES {
            if let Some(tail) = rest.strip_prefix(pattern) {
                out.push_str(replacement);
                rest = tail;
                continue 'outer;
            }
        }
        if character.is_ascii_alphanumeric() {
            out.push(character);
        }
        rest = &rest[character.len_utf8()..];
    }
    out
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

fn is_soft(c: char) -> bool {
    is_vowel(c) || matches!(c, 'j' | 'h')
}

fn first_sound(key: &str) -> Option<char> {
    key.chars().next().map(|c| if is_vowel(c) { 'a' } else { voiceless(c) })
}

fn voiceless(c: char) -> char {
    match c {
        'b' => 'p',
        'd' => 't',
        'g' => 'k',
        'v' => 'f',
        'z' => 's',
        other => other,
    }
}

/// The consonants of a key, voicing ignored.
fn skeleton(key: &str) -> String {
    key.chars().filter(|c| !is_soft(*c)).map(voiceless).collect()
}

/// Edit distance where vowels, "j" and "h" and voicing pairs count half.
fn distance(left: &str, right: &str) -> f32 {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let indel = |c: char| if is_soft(c) { 0.5 } else { 1.0 };
    let mut previous: Vec<f32> = std::iter::once(0.0)
        .chain(right.iter().scan(0.0, |sum, &c| { *sum += indel(c); Some(*sum) }))
        .collect();
    let mut current = vec![0.0; right.len() + 1];
    for &l in &left {
        current[0] = previous[0] + indel(l);
        for (j, &r) in right.iter().enumerate() {
            let substitution = if l == r {
                0.0
            } else if (is_vowel(l) && is_vowel(r)) || voiceless(l) == voiceless(r) {
                0.5
            } else {
                1.0
            };
            current[j + 1] = (previous[j] + substitution).min(previous[j + 1] + indel(l)).min(current[j] + indel(r));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, l) in left.chars().enumerate() {
        current[0] = i + 1;
        for (j, &r) in right.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(l != r)).min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Words the user says in at least `minimum` separate transcripts: their own
/// vocabulary, which a near-sounding term must not overwrite. A recogniser
/// that keeps mishearing a term the same way puts that word here too; it
/// then shows up among the suggestions, and an alias settles it.
pub fn frequent_words<'a>(texts: impl IntoIterator<Item = &'a str>, minimum: usize) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for text in texts {
        let unique: HashSet<String> = words(text).iter().filter(|word| word.text.chars().count() >= 2).map(|word| word.text.to_lowercase()).collect();
        for word in unique {
            *counts.entry(word).or_default() += 1;
        }
    }
    counts.into_iter().filter(|(_, count)| *count >= minimum).map(|(word, _)| word).collect()
}

/// A fix the user made by hand in a transcript: `heard` became `term`.
/// `heard` is `None` when only the casing changed ("github" to "GitHub").
#[derive(Clone, Debug, PartialEq)]
pub struct Correction {
    pub term: String,
    pub heard: Option<String>,
}

/// The misrecognitions the user fixed between `before` and `after`: short
/// runs of words replaced by something that sounds alike. Rewording ("ten"
/// for "tamten"), added or removed words and fixes to everyday words are not
/// vocabulary and are left out.
pub fn corrections(before: &str, after: &str) -> Vec<Correction> {
    let old = words(before);
    let new = words(after);
    // Casing counts as a change: "cloud code" to "Claude Code" is one fix.
    let fold_word = |word: &Word<'_>| word.text.to_owned();
    let old_keys: Vec<String> = old.iter().map(fold_word).collect();
    let new_keys: Vec<String> = new.iter().map(fold_word).collect();
    // Longest common subsequence of words, then walk it for replaced runs.
    let (n, m) = (old.len(), new.len());
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old_keys[i] == new_keys[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut found = Vec::new();
    let hunk = |old_run: &[Word<'_>], new_run: &[Word<'_>], found: &mut Vec<Correction>| {
        if old_run.is_empty() || new_run.is_empty() || old_run.len() > 4 || new_run.len() > 4 {
            return;
        }
        let heard = old_run.iter().map(|word| word.text).collect::<Vec<_>>().join(" ");
        let term = new_run.iter().map(|word| word.text).collect::<Vec<_>>().join(" ");
        let lower_term = term.to_lowercase();
        // One everyday word fixed into another ("kod" to "kot") is a typo,
        // not a name; a name is written with capitals ("cloud" to "Claude").
        let everyday = |run: &[Word<'_>]| run.iter().all(|word| common(&word.text.to_lowercase()));
        if (everyday(new_run) || everyday(old_run)) && !is_cased(&term) {
            return;
        }
        if fold(&heard.to_lowercase()) == fold(&lower_term) {
            // Same word, new casing: worth keeping only for a name spelt
            // with inner capitals ("GitHub"), not a sentence start.
            if term.split(' ').any(|word| word.chars().skip(1).any(char::is_uppercase)) {
                found.push(Correction { term, heard: None });
            }
            return;
        }
        // "requeste" to "requeście" fixes a Polish ending, not a word.
        let (folded_heard, folded_term) = (fold(&heard.to_lowercase()), fold(&lower_term));
        let shared = folded_heard.chars().zip(folded_term.chars()).take_while(|(a, b)| a == b).count();
        if shared >= 4 && shared + 3 >= folded_heard.chars().count().min(folded_term.chars().count()) {
            return;
        }
        let (heard_key, term_key) = (phonetic_key(&heard), phonetic_key(&term));
        let longest = heard_key.len().max(term_key.len());
        let (heard_bones, term_bones) = (skeleton(&heard_key), skeleton(&term_key));
        let alike = edit_distance(&heard_key, &term_key) * 2 <= longest
            || (term_bones.len() >= 3 && edit_distance(&heard_bones, &term_bones) * 3 <= term_bones.len());
        if longest < 3 || heard_key.get(..1) != term_key.get(..1) || !alike {
            return;
        }
        found.push(Correction { term, heard: Some(heard.to_lowercase()) });
    };
    let (mut i, mut j, mut start) = (0, 0, (0, 0));
    while i < n || j < m {
        if i < n && j < m && old_keys[i] == new_keys[j] {
            hunk(&old[start.0..i], &new[start.1..j], &mut found);
            i += 1;
            j += 1;
            start = (i, j);
        } else if j < m && (i == n || table[i][j + 1] >= table[i + 1][j]) {
            j += 1;
        } else {
            i += 1;
        }
    }
    hunk(&old[start.0..n], &new[start.1..m], &mut found);
    found
}

/// Written with capitals the way names are ("GitHub", "Claude"), not just
/// at the start of a sentence.
fn is_cased(text: &str) -> bool {
    text.split(' ').any(|word| word.chars().any(char::is_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary(terms: &[&str]) -> Vocabulary {
        Vocabulary::new(terms.iter().map(|term| (*term, &[][..])))
    }

    const USER_TERMS: &[&str] = &[
        "Claude", "Claude Code", "Claude Desktop", "Codex", "GitHub", "Loquara", "Parakeet", "SuperWhisper", "Tauri",
        "Rust", "sherpa-onnx", "try again", "remote control", "commit", "pull request", "Windows", "macOS", "Linux",
        "Spotify", "Teams", "Zoom", "API", "frontend", "backend",
    ];

    #[test]
    fn keys_fold_polish_and_english_spellings_together() {
        assert_eq!(phonetic_key("Claude"), phonetic_key("klod"));
        assert_eq!(phonetic_key("sherpa-onnx"), phonetic_key("szerpa ONX"));
        assert_eq!(phonetic_key("Tauri"), phonetic_key("Tałri"));
        assert_eq!(phonetic_key("Loquara"), phonetic_key("lokwara"));
        assert_eq!(phonetic_key("GitHub"), phonetic_key("git hub"));
    }

    /// Takes the recogniser really produced, from the user's history.
    #[test]
    fn hand_fixes_become_terms_with_what_was_heard() {
        let found = corrections(
            "Otwórz github i zrób traje aga na pull requeste w kloadkode. Właśnie przez Loara.",
            "Otwórz GitHub i zrób try again na pull requeście w Claude Code. Właśnie przez Loquarę.",
        );
        assert_eq!(found, vec![
            Correction { term: "GitHub".into(), heard: None },
            Correction { term: "try again".into(), heard: Some("traje aga".into()) },
            Correction { term: "Claude Code".into(), heard: Some("kloadkode".into()) },
            Correction { term: "Loquarę".into(), heard: Some("loara".into()) },
        ]);
    }

    #[test]
    fn rewording_and_everyday_fixes_are_not_vocabulary() {
        assert!(corrections("Zrób to jutro rano.", "Zrób to dzisiaj wieczorem.").is_empty());
        assert!(corrections("mam kod i klucz", "mam kot i klucz").is_empty());
        assert!(corrections("To jest test", "To jest bardzo dobry test").is_empty());
        assert!(corrections("Pierwsze zdanie.", "pierwsze zdanie.").is_empty());
        assert_eq!(
            corrections("zrób to w cloud code", "zrób to w Claude Code"),
            vec![Correction { term: "Claude Code".into(), heard: Some("cloud code".into()) }],
        );
    }

    #[test]
    fn real_misrecognitions_become_the_term() {
        let vocabulary = vocabulary(USER_TERMS);
        for (heard, expected) in [
            ("Dostałem na Clad Cod. Na moim komputerze Claddesktop.", "Dostałem na Claude Code. Na moim komputerze Claude Desktop."),
            ("bo jak się wejdzie w kloda, to właśnie", "bo jak się wejdzie w Claude'a, to właśnie"),
            ("No bo cloda mam tylko Pro.", "No bo Claude'a mam tylko Pro."),
            ("Bo cały czas robię tragen. No i nie chce działać", "Bo cały czas robię try again. No i nie chce działać"),
            ("jak się wtedy zachowuje ten parkit?", "jak się wtedy zachowuje ten Parakeet?"),
            ("rastowa biblioteka pod Parkit zrobiona", "rastowa biblioteka pod Parakeet zrobiona"),
            ("korzystamy z szerpa ONX. Ale", "korzystamy z sherpa-onnx. Ale"),
            ("rozważyć Taurii 2 z natywnym", "rozważyć Tauri 2 z natywnym"),
            ("Skorzystanie z Tałri i z niego", "Skorzystanie z Tauri i z niego"),
            ("klucz na githuby do odczytywania", "klucz na GitHuby do odczytywania"),
            ("stoimy na git hubie, czy były", "stoimy na GitHubie, czy były"),
            ("Jak masz Super Whispera na przykład", "Jak masz SuperWhispera na przykład"),
            ("Wtedy. Frendend czy backend dostaną", "Wtedy. Frontend czy backend dostaną"),
            ("stoi symfonia web appi.", "stoi symfonia web API."),
            ("zrób to na pull requeste w kloadkode.", "zrób to na pull requeste w Claude Code."),
            ("zrób to na pull requeście w GitHubie.", "zrób to na pull requeście w GitHubie."),
            ("robię dyktowanie przez Lockwara,", "robię dyktowanie przez Loquara,"),
        ] {
            assert_eq!(vocabulary.apply(heard), expected, "{heard}");
        }
    }

    /// Lines from the same history that look like a term and must stay.
    #[test]
    fn similar_everyday_words_stay() {
        let vocabulary = vocabulary(USER_TERMS);
        for text in [
            "To jest jakaś tragedia, wygląda tragicznie.",
            "Pigment Mic i Mig Bud Max. Luna Max, fast.",
            "Scena akcentowa. Linki i link w opisie, plakat i pakiet.",
            "Pobierz pakiety, kodeks i kod QR. Cloud jest szybki.",
            "Skorzystanie z tauri i z niego",
            "w środku jest app i dane",
            "Dostęp do Linuxa i na Windowsie, nie ma osobnego backendu.",
            "Kursor jest tam, gdzie pole nie jest rozszerzone.",
        ] {
            assert_eq!(vocabulary.apply(text), text.replace("tauri", "Tauri"), "{text}");
        }
    }

    #[test]
    fn short_terms_need_a_near_spelling() {
        let vocabulary = vocabulary(&["Vite", "Zoom", "API"]);
        assert_eq!(vocabulary.apply("Takie zły to już. Wyte. Zum, że to."), "Takie zły to już. Wyte. Zum, że to.");
        assert_eq!(vocabulary.apply("web Api i zoom"), "web API i Zoom");
    }

    #[test]
    fn everyday_words_are_only_replaced_by_an_exact_sound_or_alias() {
        let everyday = Arc::new(HashSet::from(["parkit".to_owned()]));
        let plain = vocabulary(&["Parakeet"]).protect(everyday.clone());
        assert_eq!(plain.apply("ten parkit i parakit"), "ten parkit i Parakeet");
        let aliased = Vocabulary::new([("Parakeet", &["parkit".to_owned()][..])]).protect(everyday);
        assert_eq!(aliased.apply("ten parkit"), "ten Parakeet");
    }

    #[test]
    fn aliases_casing_and_punctuation() {
        let aliases = ["parakit".to_owned()];
        let vocabulary = Vocabulary::new([("Parakeet", &aliases[..]), ("GitHub", &[][..])]);
        assert_eq!(vocabulary.apply("(parakit), github!"), "(Parakeet), GitHub!");
        let phrase = vocabulary_of("try again");
        assert_eq!(phrase.apply("Try again. try again"), "Try again. try again");
        assert_eq!(phrase.apply("Tragen. ok"), "Try again. ok");
        assert_eq!(vocabulary_of("iPhone").apply("Iphone działa"), "iPhone działa");
        assert_eq!(vocabulary_of("Codex").apply("aliasy kodeksu"), "aliasy kodeksu");
        let codex = Vocabulary::new([("Codex", &["kodeks".to_owned()][..])]);
        assert_eq!(codex.apply("sesje kodeksa i Kodeks"), "sesje Codexa i Codex");
    }

    fn vocabulary_of(term: &str) -> Vocabulary {
        vocabulary(&[term])
    }

    #[test]
    fn code_like_tokens_are_left_alone() {
        assert_eq!(vocabulary_of("Claude").apply("klod_2 i kLod"), "klod_2 i kLod");
    }

    #[test]
    fn near_misses_suggest_an_alias_without_the_ending() {
        let vocabulary = vocabulary(&["Codex", "Parakeet"]);
        let misses = vocabulary.near_misses("najnowsze sesje kodeksa i parakit");
        assert_eq!(misses.iter().map(|(term, heard)| (vocabulary.term(*term), heard.as_str())).collect::<Vec<_>>(), vec![("Codex", "kodeks")]);
    }

    #[test]
    fn frequent_words_count_transcripts_not_repetitions() {
        let words = frequent_words(["tak tak tak", "nie tak", "nie"], 2);
        assert_eq!(words, HashSet::from(["tak".to_owned(), "nie".to_owned()]));
    }
}
