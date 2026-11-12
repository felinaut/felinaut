//! State retained across refreshes and explicit reopening of live items.

pub(super) fn retain_item_state(item: &mut super::Item, old: Option<&super::Item>, first: bool) {
    let Some(old) = old else {
        item.seen = first;
        item.dismissed = None;
        return;
    };

    let changed =
        !item.updated.is_empty() && !old.updated.is_empty() && item.updated != old.updated;
    item.seen = old.seen && !changed;
    item.dismissed = old.dismissed.clone();
    if item.updated.is_empty() {
        item.updated.clone_from(&old.updated);
        return;
    }

    if let Some(at) = &old.dismissed {
        if at.is_empty() {
            // The first known time establishes a baseline, not evidence of new activity.
            item.dismissed = Some(item.updated.clone());
            item.seen = old.seen;
        } else if *at != item.updated {
            item.dismissed = None;
            item.seen = false;
        }
    }
}

pub fn restore_items(folders: &mut [super::LiveFolder], url: &str) {
    for item in folders
        .iter_mut()
        .flat_map(|folder| &mut folder.sections)
        .flat_map(|section| &mut section.items)
        .filter(|item| item.url == url)
    {
        item.dismissed = None;
        item.seen = true;
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Item, LiveFolder, github, parse, serialize};
    use super::{restore_items, retain_item_state};

    const URL: &str = "https://github.com/a/b/pull/1";
    const OTHER: &str = "https://github.com/a/b/pull/2";

    fn item(updated: &str, seen: bool, dismissed: Option<&str>) -> Item {
        Item {
            title: "Title".into(),
            url: URL.into(),
            seen,
            updated: updated.into(),
            dismissed: dismissed.map(str::to_owned),
            detail: "a/b #1".into(),
        }
    }

    fn refresh(old: &Item, updated: &str) -> Item {
        let mut next = item(updated, false, None);
        retain_item_state(&mut next, Some(old), false);
        next
    }

    #[test]
    fn missing_same_then_changed_preserves_reliable_dismissal() {
        for seen in [false, true] {
            let old = item("1", seen, Some("1"));
            let missing = refresh(&old, "");
            assert_eq!(missing, old);
            let same = refresh(&missing, "1");
            assert_eq!(same, old);
            let changed = refresh(&same, "2");
            assert_eq!(changed, item("2", false, None));
            assert!(changed.shown());
        }
    }

    #[test]
    fn seen_and_unseen_items_retain_state_without_evidence_of_change() {
        for seen in [false, true] {
            let old = item("1", seen, None);
            let missing = refresh(&old, "");
            assert_eq!(missing, old);
            let same = refresh(&missing, "1");
            assert_eq!(same, old);
            assert_eq!(refresh(&same, "2"), item("2", false, None));

            let unknown = item("", seen, None);
            assert_eq!(refresh(&unknown, ""), unknown);
            let known = refresh(&unknown, "1");
            assert_eq!(known, item("1", seen, None));
            assert_eq!(refresh(&known, "2"), item("2", false, None));
        }
    }

    #[test]
    fn unknown_dismissal_baseline_is_established_without_reopening() {
        for seen in [false, true] {
            let old = item("", seen, Some(""));
            let missing = refresh(&old, "");
            assert_eq!(missing, old);
            let known = refresh(&missing, "1");
            assert_eq!(known, item("1", seen, Some("1")));
            assert_eq!(refresh(&known, ""), known);
            assert_eq!(refresh(&known, "1"), known);
            assert_eq!(refresh(&known, "2"), item("2", false, None));

            // An older persisted item can have a known update but no dismissal baseline.
            let old = item("older", seen, Some(""));
            assert_eq!(refresh(&old, ""), old);
            assert_eq!(refresh(&old, "1"), known);
        }
    }

    #[test]
    fn dismissal_timestamp_is_the_reliable_reopening_baseline() {
        for updated in ["", "2"] {
            let old = item(updated, true, Some("1"));
            assert_eq!(refresh(&old, ""), old);
            assert_eq!(refresh(&old, "3"), item("3", false, None));
        }
    }

    #[test]
    fn first_refresh_only_marks_new_items_seen() {
        for first in [false, true] {
            for updated in ["", "1"] {
                let mut next = item(updated, !first, Some("stale"));
                retain_item_state(&mut next, None, first);
                assert_eq!(next, item(updated, first, None));
            }
        }
        let old = item("1", false, None);
        let mut next = item("1", true, None);
        retain_item_state(&mut next, Some(&old), true);
        assert_eq!(next, old);
    }

    #[test]
    fn explicit_reopen_restores_all_duplicates_and_nothing_else() {
        let mut folders = vec![LiveFolder::new(&github::SPEC); 2];
        for folder in &mut folders {
            folder.open = false;
            for section in &mut folder.sections {
                section.open = false;
                section.shown = false;
                section.items = vec![
                    item("1", false, Some("1")),
                    item("", false, Some("")),
                    item("2", false, None),
                    Item {
                        url: OTHER.into(),
                        ..item("1", false, Some("1"))
                    },
                ];
            }
        }
        let mut expected = folders.clone();
        for folder in &mut expected {
            for section in &mut folder.sections {
                for entry in &mut section.items[..3] {
                    entry.seen = true;
                    entry.dismissed = None;
                }
            }
        }
        restore_items(&mut folders, URL);
        assert_eq!(folders, expected);
        restore_items(&mut folders, URL);
        assert_eq!(folders, expected, "reopening is idempotent");
    }

    #[test]
    fn missing_url_and_empty_folders_are_noops() {
        restore_items(&mut [], URL);
        let mut folders = vec![LiveFolder::new(&github::SPEC)];
        restore_items(&mut folders, URL);
        folders[0].sections[0].items = vec![item("1", false, Some("1"))];
        let before = folders.clone();
        for url in [OTHER, "", "not a URL"] {
            restore_items(&mut folders, url);
            assert_eq!(folders, before);
        }
    }

    #[test]
    fn persistence_preserves_refresh_baselines_and_explicit_reopen() {
        let mut folders = vec![LiveFolder::new(&github::SPEC)];
        folders[0].sections[0].items = vec![
            refresh(&item("1", true, Some("1")), ""),
            refresh(&item("", false, Some("")), ""),
            item("1", true, None),
        ];
        let mut restored = parse(&serialize(&folders));
        assert_eq!(restored, folders);
        let items = &mut restored[0].sections[0].items;
        items[0] = refresh(&items[0], "1");
        assert_eq!(items[0], item("1", true, Some("1")));
        items[0] = refresh(&items[0], "2");
        assert_eq!(items[0], item("2", false, None));
        items[1] = refresh(&items[1], "1");
        assert_eq!(items[1], item("1", false, Some("1")));
        restored = parse(&serialize(&restored));
        assert_eq!(
            restored[0].sections[0].items[1].dismissed.as_deref(),
            Some("1")
        );
        restore_items(&mut restored, URL);
        assert!(
            restored[0].sections[0]
                .items
                .iter()
                .all(|i| i.seen && i.shown())
        );
        assert_eq!(parse(&serialize(&restored)), restored);
        let before = restored.clone();
        restore_items(&mut restored, OTHER);
        assert_eq!(restored, before);
    }
}
