use std::collections::{HashMap, VecDeque};

use markdown_winui::BlockTranscript;

/// Retain a small native presentation cache so switching tabs does not blank
/// the transcript while the canonical timeline refresh is in flight.
const SESSION_CACHE_CAPACITY: usize = 8;

#[derive(Default)]
pub(super) struct SessionTranscriptCache {
    entries: HashMap<String, BlockTranscript>,
    order: VecDeque<String>,
}

impl SessionTranscriptCache {
    pub(super) fn store(&mut self, seed: String, transcript: BlockTranscript) {
        if seed.is_empty() {
            return;
        }
        self.order.retain(|item| item != &seed);
        self.order.push_back(seed.clone());
        self.entries.insert(seed, transcript);
        while self.order.len() > SESSION_CACHE_CAPACITY {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
    }

    /// 零拷贝取出（BUG-003 根因 2）：move 出缓存，不再 clone。
    /// 被取出的会话再次切换时会由 store 重新写回。
    pub(super) fn restore(&mut self, seed: &str) -> Option<BlockTranscript> {
        self.order.retain(|item| item != seed);
        self.entries.remove(seed)
    }
}

#[cfg(test)]
mod session_cache_tests {
    use super::*;
    use markdown_winui::TimelineEntry as TLEntry;

    fn turn_opened(seq: u64, turn_id: &str, user_text: &str) -> TLEntry {
        TLEntry {
            timeline_seq: seq,
            turn_id: turn_id.into(),
            round_num: None,
            event: markdown_winui::TimelineEvent::TurnOpened {
                user_text: user_text.into(),
            },
        }
    }

    #[test]
    fn cache_restores_recent_transcript_and_evicts_oldest() {
        let mut cache = SessionTranscriptCache::default();
        for i in 0..=SESSION_CACHE_CAPACITY {
            let mut transcript = BlockTranscript::new();
            transcript.apply_entry(&turn_opened(1, &format!("t{i}"), &format!("q{i}")));
            cache.store(format!("s{i}"), transcript);
        }
        assert!(cache.restore("s0").is_none());
        assert_eq!(cache.restore("s8").unwrap().turns()[0].turn_id, "t8");
    }
}
