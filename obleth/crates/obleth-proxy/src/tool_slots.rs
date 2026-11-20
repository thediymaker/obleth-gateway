//! Which streamed `tool_calls[]` fragment belongs to which call. Shared by the
//! Responses and Messages shims, so both read every upstream the same way.
//!
//! Most upstreams send `index`. Ones that omit it (older Ollama, some llama.cpp
//! builds) send each call whole with only `id`, so a fragment with a new,
//! non-empty `id` starts a new slot, and one with neither `index` nor `id`
//! continues whatever was most recently opened.

use serde_json::Value;
use std::collections::HashMap;

/// One streamed tool call as a shim accumulates it.
pub(crate) trait Slot: Default {
    /// The call id seen so far, if any.
    fn id(&self) -> Option<&str>;
}

#[derive(Default)]
pub(crate) struct ToolSlots<T> {
    slots: Vec<T>,
    by_index: HashMap<u64, usize>,
}

impl<T: Slot> ToolSlots<T> {
    /// The slot a fragment belongs to, and whether it was just opened.
    pub(crate) fn resolve(&mut self, call: &Value) -> (usize, bool) {
        if let Some(index) = call.get("index").and_then(Value::as_u64) {
            if let Some(&slot) = self.by_index.get(&index) {
                return (slot, false);
            }
            let slot = self.open();
            self.by_index.insert(index, slot);
            return (slot, true);
        }
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if self.slots.last().is_some_and(|t| t.id() == Some(id)) {
                return (self.slots.len() - 1, false);
            }
            return (self.open(), true);
        }
        if self.slots.is_empty() {
            return (self.open(), true);
        }
        (self.slots.len() - 1, false)
    }

    fn open(&mut self) -> usize {
        self.slots.push(T::default());
        self.slots.len() - 1
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, T> {
        self.slots.iter()
    }
}

impl<T> std::ops::Index<usize> for ToolSlots<T> {
    type Output = T;
    fn index(&self, slot: usize) -> &T {
        &self.slots[slot]
    }
}

impl<T> std::ops::IndexMut<usize> for ToolSlots<T> {
    fn index_mut(&mut self, slot: usize) -> &mut T {
        &mut self.slots[slot]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Call {
        id: Option<String>,
    }
    impl Slot for Call {
        fn id(&self) -> Option<&str> {
            self.id.as_deref()
        }
    }

    #[test]
    fn fragments_find_their_call_by_index_then_by_id_then_by_recency() {
        let mut slots: ToolSlots<Call> = ToolSlots::default();
        assert_eq!(slots.resolve(&json!({"index": 1})), (0, true));
        assert_eq!(slots.resolve(&json!({"index": 1})), (0, false));
        assert_eq!(slots.resolve(&json!({"index": 0})), (1, true));
        // Without an index, a new id opens a call; the same id continues it.
        let (slot, opened) = slots.resolve(&json!({"id": "a"}));
        assert!(opened);
        slots[slot].id = Some("a".into());
        assert_eq!(slots.resolve(&json!({"id": "a"})), (slot, false));
        assert!(slots.resolve(&json!({"id": "b"})).1);
        // Neither index nor id: whatever was opened last.
        assert_eq!(slots.resolve(&json!({})), (slots.len() - 1, false));
    }
}
