//! Storage and content classification for the clipboard daemon.
//!
//! No platform I/O lives here — clipboard backends are in `clipd-platform`, so
//! this crate is testable without an X server.

pub mod detect;
pub mod store;

pub use store::{now_ms, text_capture, Captured, Store};

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn stores_and_reads_back() {
        let (mut s, _d) = temp_store();
        let item = s.insert(text_capture("hello world"), false).unwrap().unwrap();
        assert_eq!(item.preview, "hello world");
        assert_eq!(s.recent(10).unwrap().len(), 1);
    }

    /// The Phase 0 finding: GNOME re-offers the current clipboard when the
    /// owning app exits. Identical content must not create a second entry or
    /// inflate use_count.
    #[test]
    fn suppresses_reoffer_of_head() {
        let (mut s, _d) = temp_store();
        s.insert(text_capture("same"), false).unwrap().unwrap();
        assert!(s.insert(text_capture("same"), false).unwrap().is_none());
        assert!(s.insert(text_capture("same"), false).unwrap().is_none());

        let items = s.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].use_count, 1, "re-offers must not inflate use_count");
    }

    /// Regression: GNOME's hand-off re-offers the same copy with a *different*
    /// flavor list. Identity must come from the content, or every copy lands
    /// twice — which is exactly what the first end-to-end run produced.
    #[test]
    fn reoffer_with_different_flavor_list_is_still_the_same_copy() {
        let (mut s, _d) = temp_store();

        // As the source application offers it.
        let original = Captured {
            flavors: vec![
                ("UTF8_STRING".into(), b"npm install react".to_vec()),
                ("STRING".into(), b"npm install react".to_vec()),
            ],
            source_app: None,
            hinted_secret: false,
        };
        // As GNOME re-offers it after the app exits: STRING gone,
        // text/plain;charset=utf-8 added.
        let reoffer = Captured {
            flavors: vec![
                ("text/plain;charset=utf-8".into(), b"npm install react".to_vec()),
                ("UTF8_STRING".into(), b"npm install react".to_vec()),
            ],
            source_app: None,
            hinted_secret: false,
        };

        assert!(s.insert(original, false).unwrap().is_some());
        assert!(
            s.insert(reoffer, false).unwrap().is_none(),
            "re-offer with a different flavor list must be recognised as the same copy"
        );
        assert_eq!(s.recent(10).unwrap().len(), 1);
    }

    #[test]
    fn images_are_identified_by_pixels_not_by_a_caption() {
        let (mut s, _d) = temp_store();
        let with_caption = Captured {
            flavors: vec![
                ("image/png".into(), b"\x89PNG-pixels".to_vec()),
                ("UTF8_STRING".into(), b"screenshot.png".to_vec()),
            ],
            source_app: None,
            hinted_secret: false,
        };
        let renamed = Captured {
            flavors: vec![
                ("image/png".into(), b"\x89PNG-pixels".to_vec()),
                ("UTF8_STRING".into(), b"a-different-name.png".to_vec()),
            ],
            source_app: None,
            hinted_secret: false,
        };
        assert!(s.insert(with_caption, false).unwrap().is_some());
        assert!(s.insert(renamed, false).unwrap().is_none());
    }

    /// But re-copying something that is *not* at the head is a real copy: it
    /// promotes the existing row instead of duplicating it.
    #[test]
    fn recopying_an_older_item_promotes_it() {
        let (mut s, _d) = temp_store();
        s.insert(text_capture("first"), false).unwrap();
        s.insert(text_capture("second"), false).unwrap();
        let again = s.insert(text_capture("first"), false).unwrap().unwrap();

        assert_eq!(again.use_count, 2);
        let items = s.recent(10).unwrap();
        assert_eq!(items.len(), 2, "dedupe by content hash, no duplicate row");
        assert_eq!(items[0].preview, "first", "re-copied item returns to the head");
    }

    #[test]
    fn secrets_are_not_stored_and_not_indexed() {
        let (mut s, _d) = temp_store();
        assert!(s.insert(text_capture("ghp_abcdefghijklmnopqrstuvwxyz"), false).unwrap().is_none());
        assert_eq!(s.count().unwrap(), 0);

        // With storage allowed, it is kept but stays out of the search index.
        let (mut s2, _d2) = temp_store();
        let item = s2.insert(text_capture("ghp_abcdefghijklmnopqrstuvwxyz"), true).unwrap().unwrap();
        assert!(item.sensitive);
        assert!(s2.search("ghp_", 10).unwrap().is_empty(), "secrets must not be searchable");
    }

    #[test]
    fn search_matches_substrings() {
        let (mut s, _d) = temp_store();
        s.insert(text_capture("npm install react"), false).unwrap();
        s.insert(text_capture("cargo build --release"), false).unwrap();

        // Mid-token substring — this is what the trigram tokenizer buys us.
        let hits = s.search("m inst", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].preview, "npm install react");

        // Short queries fall back to LIKE rather than returning nothing.
        assert_eq!(s.search("np", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_treats_punctuation_literally() {
        let (mut s, _d) = temp_store();
        s.insert(text_capture("https://example.com/a?b=c"), false).unwrap();
        assert_eq!(s.search("example.com/a", 10).unwrap().len(), 1);
        // Must not blow up on FTS5 syntax characters.
        assert!(s.search("\"quoted\" AND *", 10).is_ok());
    }

    #[test]
    fn keeps_all_flavors_of_one_copy() {
        let (mut s, _d) = temp_store();
        let cap = Captured {
            flavors: vec![
                ("text/html".into(), b"<b>bold</b>".to_vec()),
                ("UTF8_STRING".into(), b"bold".to_vec()),
            ],
            source_app: Some("firefox".into()),
            hinted_secret: false,
        };
        let item = s.insert(cap, false).unwrap().unwrap();
        assert_eq!(item.mimes.len(), 2);

        let flavors = s.flavors(item.id).unwrap();
        assert_eq!(flavors.len(), 2, "plain-text fallback survives for paste-as-plain");
    }

    #[test]
    fn password_hint_is_honoured_even_for_innocuous_text() {
        let (mut s, _d) = temp_store();
        let cap = Captured {
            flavors: vec![("UTF8_STRING".into(), b"hunter2".to_vec())],
            source_app: Some("keepassxc".into()),
            hinted_secret: true,
        };
        assert!(s.insert(cap, false).unwrap().is_none());
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn pins_survive_clear_all() {
        let (mut s, _d) = temp_store();
        let keep = s.insert(text_capture("keep me"), false).unwrap().unwrap();
        s.insert(text_capture("drop me"), false).unwrap();
        s.set_pinned(keep.id, true).unwrap();

        s.clear_all().unwrap();
        let items = s.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].preview, "keep me");
    }

    #[test]
    fn prune_respects_pins_and_keeps_newest() {
        let (mut s, _d) = temp_store();
        for i in 0..10 {
            s.insert(text_capture(&format!("item {i}")), false).unwrap();
        }
        let oldest = s.recent(100).unwrap().last().unwrap().id;
        s.set_pinned(oldest, true).unwrap();

        s.prune(3).unwrap();
        let items = s.recent(100).unwrap();
        assert_eq!(items.len(), 4, "3 unpinned + 1 pinned");
        assert!(items.iter().any(|i| i.id == oldest), "pinned item survives pruning");
    }

    #[test]
    fn deleting_head_lets_the_same_content_be_captured_again() {
        let (mut s, _d) = temp_store();
        let item = s.insert(text_capture("transient"), false).unwrap().unwrap();
        s.delete(item.id).unwrap();
        // head_hash must have been recomputed, or this would be suppressed.
        assert!(s.insert(text_capture("transient"), false).unwrap().is_some());
    }
}
