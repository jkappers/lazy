//! The words of the docs views (D26 voice: plain, short, no exclamation marks, a concrete reason
//! where there is one).

use lazy_domain::Timestamp;

pub const DOCS_TITLE: &str = "Docs";
pub const NOTES_TITLE: &str = "Notes for me";
pub const NEW_PAGE: &str = "New page";
pub const NEW_BOARD: &str = "New board";
pub const ALL_PLACES: &str = "All conversations";
pub const SEARCH_PLACEHOLDER: &str = "Search pages and boards";
pub const LIVE: &str = "Pages";
pub const TRASH: &str = "Trash";
pub const SHOW_MORE: &str = "Show more";
pub const LOADING: &str = "Loading…";

pub const EMPTY_TITLE: &str = "Nothing written here yet.";
pub const EMPTY_BODY: &str =
    "Pages and boards live in a conversation, so everyone there can read them. Start with a page.";
pub const EMPTY_READ_ONLY: &str =
    "Pages and boards live in a conversation. You can read them, but only members can add to them.";
pub const TRASH_EMPTY_TITLE: &str = "The trash is empty.";
pub const TRASH_EMPTY_BODY: &str = "Pages you delete wait here for 30 days.";
pub const LIST_FAILED_TITLE: &str = "The pages didn't load";
pub const NOTES_EMPTY_TITLE: &str = "No notes here.";
pub const NOTES_EMPTY_ASSIGNED: &str =
    "Notes that someone assigns to you on a page will appear here.";
pub const NOTES_EMPTY_CREATED: &str = "Notes you write on a page will appear here.";
pub const NOTES_FAILED_TITLE: &str = "The notes didn't load";

pub const SAVED: &str = "Saved";
pub const VIEW_ONLY: &str = "View only";
pub const SAVING: &str = "Saving…";
pub const UNSAVED: &str = "Unsaved changes";
pub const SAVE_RETRYING: &str = "Couldn't save, retrying";
pub const SAVE_CONFLICT: &str = "Not saved: changed by someone else";

pub const CONFLICT_TITLE: &str = "Someone changed this page while you were editing.";
pub const CONFLICT_BODY: &str = "Keep yours to save your version over theirs (theirs stays in History), or take theirs and drop your edits.";
pub const KEEP_MINE: &str = "Keep mine";
pub const TAKE_THEIRS: &str = "Take theirs";
pub const NEWER_TITLE: &str = "Someone changed this page.";
pub const NEWER_BODY: &str =
    "You have edits that aren't saved yet. Loading their version would replace them.";
pub const LOAD_THEIRS: &str = "Load their version";
pub const GONE_TITLE: &str = "This page is gone.";
pub const GONE_BODY: &str = "It was deleted for good, or you no longer have access to it.";
pub const RECOVERED_TITLE: &str = "Edits that couldn't be saved";
pub const RECOVERED_BODY: &str = "These were made before Lazy closed, on a page that changed in the meantime. They are kept here so nothing is lost.";
pub const COPY_TEXT: &str = "Copy text";
pub const DISMISS: &str = "Dismiss";
pub const READ_ONLY_NOTE: &str =
    "You can read this, but only members of the conversation can change it.";
pub const TRASHED_NOTE: &str = "This is in the trash.";
pub const PAGE_FAILED_TITLE: &str = "The page didn't load";

pub const HISTORY: &str = "History";
pub const HISTORY_EMPTY: &str =
    "No earlier versions yet. One is kept each time the page changes, at most every five minutes.";
pub const HISTORY_FAILED: &str = "The history didn't load";
pub const RESTORE_VERSION: &str = "Restore this version";
pub const BACK_TO_CURRENT: &str = "Back to current";
pub const RESTORE_NOTE: &str =
    "Your page as it is now is kept in History first, so you can undo a restore.";
pub const RESTORE_WAIT: &str = "Your edits are being saved first.";

pub const NOTES: &str = "Notes";
pub const ADD_NOTE: &str = "Add note";
pub const ADD_NOTE_HINT: &str = "Select some text to add a note to it.";
pub const NOTES_PANE_EMPTY: &str =
    "No notes on this page. Select text, then choose Add note from the … menu.";
pub const MORE_LABEL: &str = "More";
pub const NOTE_DETACHED: &str = "The text this note was on has changed.";
pub const NOTE_PLACEHOLDER: &str = "Write a note";
pub const REPLY_PLACEHOLDER: &str = "Reply";
pub const NOTE_BOARD_NOTE: &str = "Boards don't take notes.";

pub const FILES: &str = "Files";
pub const ATTACH_FILE: &str = "Attach a file";
pub const MOVE: &str = "Move…";
pub const MOVE_TO_TRASH: &str = "Move to trash";
pub const RESTORE: &str = "Restore";
pub const DELETE_FOREVER: &str = "Delete forever";
pub const EDIT: &str = "Edit";
pub const PREVIEW: &str = "Preview";
pub const BACK: &str = "Back";
pub const UNTITLED: &str = "Untitled";
pub const TITLE_PLACEHOLDER: &str = "Untitled";
pub const BODY_PLACEHOLDER: &str = "Write here. Markdown works.";

pub fn creating_failed(reason: &str) -> String {
    format!(
        "Couldn't make the page: {}",
        crate::states::sentence(reason)
    )
}

pub fn list_failed(reason: &str) -> String {
    crate::states::sentence(reason)
}

pub fn no_match(query: &str) -> String {
    format!("Nothing matches \u{201c}{}\u{201d}.", query.trim())
}

pub fn purge_question(title: &str) -> String {
    format!("Delete \u{201c}{title}\u{201d} for good?")
}

pub const PURGE_DETAIL: &str =
    "This removes the page, its history, its notes, and its files for everyone. You can't undo it.";

pub fn days_until_purge(days: i64) -> String {
    match days {
        d if d <= 0 => "Deleted today".to_owned(),
        1 => "Deleted for good tomorrow".to_owned(),
        d => format!("Deleted for good in {d} days"),
    }
}

/// Days from now until `purge_at`, rounded up, never negative.
pub fn days_left(purge_at: Timestamp, now: Timestamp) -> i64 {
    let seconds = (purge_at.as_offset_date_time() - now.as_offset_date_time()).whole_seconds();
    ((seconds + 86_399) / 86_400).max(0)
}

pub fn open_notes_label(count: u32) -> String {
    if count == 0 {
        NOTES.to_owned()
    } else {
        format!("{NOTES} ({count})")
    }
}

pub fn counts_line(open_notes: u32, files: u32) -> Option<String> {
    let mut parts = Vec::new();
    if open_notes > 0 {
        parts.push(format!(
            "{open_notes} open {}",
            if open_notes == 1 { "note" } else { "notes" }
        ));
    }
    if files > 0 {
        parts.push(format!(
            "{files} {}",
            if files == 1 { "file" } else { "files" }
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

pub fn version_label(version: u32, reason: lazy_domain::page::VersionReason) -> String {
    use lazy_domain::page::VersionReason::*;
    match reason {
        Autosave => format!("Version {version}"),
        PreRestore => format!("Version {version}, before a restore"),
    }
}

pub fn file_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{:.1} MB", b / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_as_short_phrases() {
        assert_eq!(counts_line(0, 0), None);
        assert_eq!(counts_line(1, 0).unwrap(), "1 open note");
        assert_eq!(counts_line(2, 1).unwrap(), "2 open notes · 1 file");
    }

    #[test]
    fn purge_days_are_plain() {
        assert_eq!(days_until_purge(0), "Deleted today");
        assert_eq!(days_until_purge(1), "Deleted for good tomorrow");
        assert_eq!(days_until_purge(30), "Deleted for good in 30 days");
    }

    #[test]
    fn sizes_are_short() {
        assert_eq!(file_size(10), "10 B");
        assert_eq!(file_size(2048), "2 KB");
        assert_eq!(file_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn no_string_shouts() {
        for s in [
            EMPTY_TITLE,
            CONFLICT_TITLE,
            GONE_TITLE,
            NOTES_EMPTY_TITLE,
            TRASH_EMPTY_TITLE,
        ] {
            assert!(!s.contains('!'));
        }
    }
}
