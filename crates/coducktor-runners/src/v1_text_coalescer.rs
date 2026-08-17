//! Per-item coalescing of streamed assistant text into whole v1 `text` events, shared by the
//! Codex and OpenCode backends.
//!
//! The v1 `AgentEvent`/[`coducktor_core::workflows::run::EventInput`] contract treats `text` as
//! a complete assistant block — that is what the RunManager persists one NDJSON line per event,
//! and what every consumer renders as one paragraph. Emitting each delta as its own `text` event
//! would persist one line per token, and a turn-end marker split across deltas (`CE`+`Z`+`:D`+
//! `ONE`) would slip past per-event marker stripping.

/// Accumulated deltas per open item, in arrival order — a `Vec` rather than a `HashMap` because
/// [`Self::flush`] must replay items in the order their first delta arrived, and a session only
/// ever has a handful of items open at once.
#[derive(Debug, Default)]
pub struct V1TextCoalescer {
    pending: Vec<(String, String)>,
    /// Items already emitted — a late snapshot/delta must not double-emit. The anonymous bucket
    /// (`""`) is deliberately never inserted here: without an identity, "already emitted" can't
    /// be told apart from "the next id-less item".
    done: std::collections::HashSet<String>,
}

impl V1TextCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    fn take_pending(&mut self, key: &str) -> Option<String> {
        let index = self.pending.iter().position(|(k, _)| k == key)?;
        Some(self.pending.remove(index).1)
    }

    /// Buffer one streamed delta of an item. Deltas with no item id share the `""` bucket (at
    /// most one anonymous item streams at a time).
    pub fn append(&mut self, item_id: Option<&str>, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let key = item_id.unwrap_or("");
        if self.done.contains(key) {
            return;
        }
        if let Some(entry) = self.pending.iter_mut().find(|(k, _)| k == key) {
            entry.1.push_str(delta);
        } else {
            self.pending.push((key.to_owned(), delta.to_owned()));
        }
    }

    /// The item is final: return its text exactly once — the wire snapshot when the backend sent
    /// one (authoritative full text), else the accumulated deltas. `None` when there is nothing
    /// to emit (already emitted, or an empty completion).
    pub fn complete(
        &mut self,
        item_id: Option<&str>,
        snapshot_text: Option<&str>,
    ) -> Option<String> {
        let key = item_id.unwrap_or("").to_owned();
        if self.done.contains(&key) {
            return None;
        }
        if !key.is_empty() {
            self.done.insert(key.clone());
        }
        let mut buffered = self.take_pending(&key);
        if buffered.is_none() && !key.is_empty() {
            // Deltas that arrived without an item id belong to the only in-flight item.
            buffered = self.take_pending("");
        }
        let text = match snapshot_text {
            Some(snapshot) if !snapshot.is_empty() => snapshot.to_owned(),
            _ => buffered.unwrap_or_default(),
        };
        (!text.is_empty()).then_some(text)
    }

    /// Turn/session boundary: return whatever never saw its item complete, in arrival order, so
    /// interrupted turns still surface their partial prose. The `done` latch survives on purpose
    /// — item ids are unique per session, and a re-sent snapshot after the boundary must not
    /// re-emit.
    pub fn flush(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (key, text) in self.pending.drain(..) {
            if !text.is_empty() {
                out.push(text);
            }
            if !key.is_empty() {
                self.done.insert(key);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_deltas_and_emits_one_text_on_complete() {
        let mut coalescer = V1TextCoalescer::new();
        for delta in ["github", ".com", "/open", "-merc", "ato"] {
            coalescer.append(Some("m1"), delta);
        }
        assert_eq!(
            coalescer.complete(Some("m1"), None),
            Some("github.com/open-mercato".to_owned())
        );
    }

    #[test]
    fn prefers_the_completion_snapshot_over_accumulated_deltas() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(Some("m1"), "partial");
        assert_eq!(
            coalescer.complete(Some("m1"), Some("the full authoritative text")),
            Some("the full authoritative text".to_owned())
        );
    }

    #[test]
    fn emits_the_snapshot_for_an_item_that_never_streamed() {
        let mut coalescer = V1TextCoalescer::new();
        assert_eq!(
            coalescer.complete(Some("m1"), Some("whole message at once")),
            Some("whole message at once".to_owned())
        );
    }

    #[test]
    fn emits_nothing_for_an_empty_completion() {
        let mut coalescer = V1TextCoalescer::new();
        assert_eq!(coalescer.complete(Some("m1"), None), None);
        assert_eq!(coalescer.complete(Some("m2"), Some("")), None);
    }

    #[test]
    fn never_double_emits_a_repeated_complete_for_the_same_item() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(Some("m1"), "hello");
        assert_eq!(
            coalescer.complete(Some("m1"), Some("hello")),
            Some("hello".to_owned())
        );
        assert_eq!(coalescer.complete(Some("m1"), Some("hello")), None);
    }

    #[test]
    fn claims_id_less_deltas_for_the_completing_item() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(None, "no id ");
        coalescer.append(None, "yet");
        assert_eq!(
            coalescer.complete(Some("m1"), None),
            Some("no id yet".to_owned())
        );
    }

    #[test]
    fn flush_surfaces_buffered_prose_in_arrival_order() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(Some("a"), "first");
        coalescer.append(Some("b"), "second");
        assert_eq!(
            coalescer.flush(),
            vec!["first".to_owned(), "second".to_owned()]
        );
        assert_eq!(coalescer.flush(), Vec::<String>::new());
    }

    #[test]
    fn a_resent_snapshot_after_flush_does_not_re_emit() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(Some("m1"), "partial prose");
        coalescer.flush();
        assert_eq!(
            coalescer.complete(Some("m1"), Some("partial prose plus tail")),
            None
        );
    }

    #[test]
    fn keeps_items_independent_interleaved_deltas_do_not_mix() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(Some("a"), "aaa");
        coalescer.append(Some("b"), "bbb");
        assert_eq!(coalescer.complete(Some("b"), None), Some("bbb".to_owned()));
        assert_eq!(coalescer.complete(Some("a"), None), Some("aaa".to_owned()));
    }

    #[test]
    fn the_anonymous_bucket_is_reusable_across_items() {
        let mut coalescer = V1TextCoalescer::new();
        coalescer.append(None, "one");
        assert_eq!(coalescer.complete(None, None), Some("one".to_owned()));
        coalescer.append(None, "two");
        assert_eq!(coalescer.complete(None, None), Some("two".to_owned()));
    }
}
