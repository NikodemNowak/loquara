//! Bounded, replaceable drafts. No keyboard or window operations live here.
#[derive(Default)]
pub struct DraftAssembler {
    completed: String,
    locked: String,
    interim: String,
}
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
impl DraftAssembler {
    pub fn update(&mut self, text: &str, locked: bool, utterance_final: bool) -> String {
        if utterance_final {
            self.completed = join(&[&self.completed, text]);
            self.locked.clear();
            self.interim.clear();
        } else if locked {
            self.locked = join(&[&self.locked, text]);
            self.interim.clear();
        } else {
            self.interim = text.trim().to_owned();
        }
        join(&[&self.completed, &self.locked, &self.interim])
    }
}
#[derive(Default)]
pub struct PreviewBuffer {
    samples: std::collections::VecDeque<f32>,
    generation: u64,
}
impl PreviewBuffer {
    pub fn push(&mut self, samples: &[i16]) {
        const LIMIT: usize = 16_000 * 30;
        for sample in samples {
            if self.samples.len() == LIMIT {
                self.samples.pop_front();
            }
            self.samples.push_back(f32::from(*sample) / 32768.0);
        }
        self.generation += samples.len() as u64;
    }
    pub fn snapshot(&self) -> (u64, Vec<f32>) {
        (self.generation, self.samples.iter().copied().collect())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_replace_entire_draft_and_allow_large_unicode_rewrites() {
        let mut draft = DraftAssembler::default();
        assert_eq!(
            draft.update("Dzień dobry 🦜", false, false),
            "Dzień dobry 🦜"
        );
        let rewrite = "Zażółć gęślą jaźń. ".repeat(100);
        assert_eq!(draft.update(&rewrite, false, false), rewrite.trim());
    }
    #[test]
    fn locked_segments_and_utterance_finals_preserve_repeated_phrases() {
        let mut draft = DraftAssembler::default();
        assert_eq!(draft.update("Tak", true, false), "Tak");
        assert_eq!(draft.update("tak", true, false), "Tak tak");
        assert_eq!(draft.update("Tak, tak!", true, true), "Tak, tak!");
        assert_eq!(draft.update("Tak, tak!", true, true), "Tak, tak! Tak, tak!");
    }
    #[test]
    fn a_locked_chunk_is_replaced_by_whole_utterance_revision() {
        let mut draft = DraftAssembler::default();
        draft.update("Dzień", true, false);
        assert_eq!(draft.update("dobry", false, false), "Dzień dobry");
        assert_eq!(draft.update("Dobry wieczór!", true, true), "Dobry wieczór!");
    }
    #[test]
    fn slow_decode_retains_only_latest_thirty_seconds_without_queueing_jobs() {
        let mut buffer = PreviewBuffer::default();
        buffer.push(&vec![100; 16_000 * 29]);
        let (first, _) = buffer.snapshot();
        for _ in 0..40 {
            buffer.push(&vec![200; 16_000]);
        }
        let (latest, samples) = buffer.snapshot();
        assert!(latest > first);
        assert_eq!(samples.len(), 16_000 * 30);
        assert!(
            samples
                .iter()
                .all(|sample| (*sample - 200.0 / 32768.0).abs() < 1e-6)
        );
    }
}

#[derive(Default)]
pub struct LocalPreview {
    consumed: u64,
    committed: String,
}
impl LocalPreview {
    pub fn update(
        &mut self,
        generation: u64,
        samples: &[f32],
        mut decode: impl FnMut(&[f32]) -> Result<String, String>,
    ) -> Result<String, String> {
        let first_retained = generation.saturating_sub(samples.len() as u64);
        if first_retained > self.consumed {
            return Err("local preview fell behind; using complete recording".into());
        }
        let offset = self.consumed.saturating_sub(first_retained) as usize;
        if offset > samples.len() {
            return Err("local preview sample coverage changed".into());
        }
        let pending = &samples[offset..];
        if pending.iter().all(|sample| sample.abs() < 0.008) {
            self.consumed = generation;
            return Ok(self.committed.clone());
        }
        let mut quiet = 0;
        let mut boundary = None;
        for (index, sample) in pending.iter().enumerate() {
            quiet = if sample.abs() < 0.008 { quiet + 1 } else { 0 };
            if index >= 16_000 * 10 && quiet >= 4_800 {
                boundary = Some(index + 1);
            }
        }
        let tail = if let Some(boundary) = boundary {
            let text = decode(&pending[..boundary])?;
            self.committed = join(&[&self.committed, &text]);
            self.consumed += boundary as u64;
            &pending[boundary..]
        } else {
            if pending.len() > 16_000 * 25 {
                return Err(
                    "continuous speech exceeded safe preview window; using complete recording"
                        .into(),
                );
            }
            pending
        };
        let interim = if tail.len() >= 8_000 {
            decode(tail)?
        } else {
            String::new()
        };
        Ok(join(&[&self.committed, &interim]))
    }
}
#[cfg(test)]
mod long_preview_tests {
    use super::*;
    #[test]
    fn long_preview_retains_prefix_and_repeated_phrases_across_silent_boundaries() {
        let mut preview = LocalPreview::default();
        let mut buffer = PreviewBuffer::default();
        let segment = [vec![1000; 16_000 * 11], vec![0; 16_000]].concat();
        for n in 1..=5 {
            buffer.push(&segment);
            let (generation, samples) = buffer.snapshot();
            let text = preview
                .update(generation, &samples, |_| Ok("tak tak".into()))
                .unwrap();
            assert_eq!(text, vec!["tak tak"; n].join(" "));
        }
    }
    #[test]
    fn slow_preview_with_a_coverage_gap_fails_instead_of_returning_tail() {
        let mut preview = LocalPreview::default();
        let mut buffer = PreviewBuffer::default();
        buffer.push(&vec![1000; 16_000 * 3]);
        let (n, samples) = buffer.snapshot();
        assert_eq!(
            preview
                .update(n, &samples, |_| Ok("beginning".into()))
                .unwrap(),
            "beginning"
        );
        buffer.push(&vec![1000; 16_000 * 35]);
        let (n, samples) = buffer.snapshot();
        assert!(
            preview
                .update(n, &samples, |_| panic!(
                    "gap must not decode a partial prefix"
                ))
                .is_err()
        );
    }
    #[test]
    fn long_uninterrupted_speech_falls_back_before_losing_words() {
        let mut preview = LocalPreview::default();
        assert!(
            preview
                .update(16_000 * 26, &vec![0.1; 16_000 * 26], |_| panic!(
                    "unsafe segment cut"
                ))
                .is_err()
        );
    }
}
