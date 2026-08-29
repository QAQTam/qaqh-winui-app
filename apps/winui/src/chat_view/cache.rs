use std::cell::RefCell;
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

thread_local! {
    /// UI 线程驻留的会话投影缓存（BUG-F1 Fix B）。离开 chat 视图时组件
    /// 子树整体卸载、use_ref 随之销毁，但本线程静态仍保有最近会话的
    /// 投影——返回 chat 时零等待恢复（随后台快照刷新校正漂移）。
    /// BlockTranscript 内含 Rc（非 Send），不能进 BridgeCore 的 Mutex
    /// （Arc<BridgeCore> 被 tokio 任务跨线程持有）；reactor 全部视图运行
    /// 在单一 UI 线程（engine.debug_assert_on_ui_thread），RefCell 足够。
    static SESSION_TRANSCRIPTS: RefCell<SessionTranscriptCache> =
        RefCell::new(SessionTranscriptCache::default());
}

/// 卸载 cleanup / 切会话分支写入（move 入缓存，零拷贝）。
pub(super) fn cache_store(seed: String, transcript: BlockTranscript) {
    SESSION_TRANSCRIPTS.with(|cache| cache.borrow_mut().store(seed, transcript));
}

/// 重挂载首帧 / 切会话分支读取（move 出缓存，零拷贝）。
pub(super) fn cache_take(seed: &str) -> Option<BlockTranscript> {
    SESSION_TRANSCRIPTS.with(|cache| cache.borrow_mut().restore(seed))
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
