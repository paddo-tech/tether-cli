//! Every key the dashboard handles, per context. The footer and the help overlay are drawn
//! from these tables, and a test checks them against the key handlers, so they cannot drift.

use crate::dashboard::app::{App, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Binding {
    /// Every key that does this; the first one is the one a click on the hint sends
    pub codes: &'static [KeyCode],
    pub ctrl: bool,
    /// The key as the user reads it
    pub key: &'static str,
    /// Short text for the footer
    pub hint: &'static str,
    /// Text for the help overlay
    pub help: &'static str,
    /// Footer order of importance, 1 first; 0 shows the binding only in help
    pub prio: u8,
}

impl Binding {
    pub fn event(&self) -> Option<KeyEvent> {
        let modifiers = if self.ctrl {
            KeyModifiers::CONTROL
        } else {
            KeyModifiers::NONE
        };
        self.codes.first().map(|c| KeyEvent::new(*c, modifiers))
    }

    /// The key as the footer shows it, where room is short.
    pub fn short_key(&self) -> &'static str {
        match self.key {
            "Enter" => "⏎",
            "Ctrl+K" => "^K",
            key => key,
        }
    }
}

const fn b(
    codes: &'static [KeyCode],
    key: &'static str,
    hint: &'static str,
    help: &'static str,
    prio: u8,
) -> Binding {
    Binding {
        codes,
        ctrl: false,
        key,
        hint,
        help,
        prio,
    }
}

const fn ctrl(
    code: &'static [KeyCode],
    key: &'static str,
    hint: &'static str,
    help: &'static str,
    prio: u8,
) -> Binding {
    Binding {
        codes: code,
        ctrl: true,
        key,
        hint,
        help,
        prio,
    }
}

use KeyCode::{Char, Delete, Down, Enter, Esc, Left, Right, Up};

const MOVE: Binding = b(
    &[Char('j'), Char('k'), Down, Up],
    "j/k ↑↓",
    "move",
    "Move",
    0,
);

/// The help key. The footer always shows it, as "more" when it had to leave keys out.
pub const HELP: Binding = b(&[Char('?')], "?", "more", "This help: every key", 0);

pub const GLOBAL: &[Binding] = &[
    b(&[Char('s')], "s", "sync", "Sync now", 2),
    b(
        &[Char('d')],
        "d",
        "daemon",
        "Start the daemon, or stop it (asks)",
        5,
    ),
    b(&[Char('r')], "r", "refresh", "Read the state again", 7),
    ctrl(&[Char('k')], "Ctrl+K", "commands", "Command palette", 4),
    b(
        &[
            KeyCode::Tab,
            Char('1'),
            Char('2'),
            Char('3'),
            Char('4'),
            Char('5'),
            Char('6'),
        ],
        "Tab 1-6",
        "tabs",
        "Next tab, or go to a tab",
        0,
    ),
    HELP,
    b(
        &[Esc],
        "Esc",
        "back",
        "Close, collapse or go back. Never quits",
        0,
    ),
    b(&[Char('q')], "q", "quit", "Quit", 3),
    ctrl(&[Char('c')], "Ctrl+C", "quit", "Quit", 0),
    b(&[], "click", "", "Select; click again to open", 0),
];

const OVERVIEW: &[Binding] = &[
    b(
        &[Enter],
        "Enter",
        "daemon log",
        "Open the whole daemon log, read-only",
        1,
    ),
    b(
        &[Char('j'), Char('k'), Down, Up],
        "j/k ↑↓",
        "scroll",
        "Scroll the file list",
        0,
    ),
];

const FILES: &[Binding] = &[
    b(
        &[Enter],
        "Enter",
        "open",
        "Expand a section, open file history, show a diff",
        1,
    ),
    b(
        &[Esc],
        "Esc",
        "collapse",
        "Close the diff, then the history",
        0,
    ),
    MOVE,
    b(
        &[Char('R')],
        "R",
        "restore",
        "Restore the file to the selected commit (asks)",
        2,
    ),
    b(
        &[Char('b')],
        "b",
        "backups",
        "Restore the file from a backup (asks)",
        5,
    ),
    b(
        &[Char('i')],
        "i",
        "import",
        "Import a dotfile from another profile",
        3,
    ),
    b(
        &[Char('t')],
        "t",
        "shared",
        "Turn sharing on or off for the file",
        4,
    ),
    b(
        &[Char('x')],
        "x",
        "remove",
        "Remove the file from this profile (asks)",
        6,
    ),
];

const PACKAGES: &[Binding] = &[
    b(
        &[Enter],
        "Enter",
        "expand",
        "Expand a manager; show a history diff",
        1,
    ),
    b(
        &[Esc],
        "Esc",
        "collapse",
        "Close the diff, the history, then the list",
        0,
    ),
    MOVE,
    b(
        &[Char('t')],
        "t",
        "profiles",
        "Share: pick the profiles that install it",
        2,
    ),
    b(
        &[Char('x')],
        "x",
        "uninstall",
        "Uninstall the package (asks)",
        3,
    ),
    b(
        &[Char('i')],
        "i",
        "install",
        "Install a package that another machine has",
        4,
    ),
    b(
        &[Char('h')],
        "h",
        "history",
        "Open the manager's manifest history",
        6,
    ),
    b(
        &[Char('R')],
        "R",
        "rollback",
        "Roll back to the selected history entry (asks)",
        8,
    ),
];

const MACHINES: &[Binding] = &[
    b(
        &[Enter],
        "Enter",
        "details",
        "Open or close the machine's details",
        1,
    ),
    b(&[Esc], "Esc", "close", "Close the details", 0),
    b(
        &[
            Char('h'),
            Char('j'),
            Char('k'),
            Char('l'),
            Left,
            Down,
            Up,
            Right,
        ],
        "hjkl ←↓↑→",
        "move",
        "Move between cards",
        0,
    ),
    b(
        &[Char('p')],
        "p",
        "profile",
        "Set this machine's profile (its own card only)",
        2,
    ),
    b(
        &[Char('a')],
        "a",
        "trust",
        "Trust the machine's key (asks)",
        4,
    ),
    b(
        &[Char('x')],
        "x",
        "untrust",
        "Untrust the machine's key (asks)",
        6,
    ),
    b(
        &[Char('D')],
        "D",
        "remove old id",
        "Remove an old id of this machine (asks)",
        8,
    ),
];

const CONFIG: &[Binding] = &[
    b(
        &[Enter],
        "Enter",
        "edit",
        "Turn on or off, edit a value, or open a list",
        1,
    ),
    MOVE,
];

const LIST_ADD: Binding = b(
    &[Char('a')],
    "a",
    "add",
    "Add an item; Enter saves it, Esc cancels",
    1,
);
const LIST_REMOVE: Binding = b(
    &[Char('x'), Delete],
    "x",
    "remove",
    "Remove the item (asks)",
    2,
);
const LIST_BACK: Binding = b(&[Esc], "Esc", "back", "Back to the Config list", 1);

/// Keys inside a Config list. Typing a new item takes every key until Enter or Esc.
pub const CONFIG_LIST: &[Binding] = &[LIST_ADD, LIST_REMOVE, LIST_BACK, MOVE];

/// Keys inside the Dotfiles list, which also sets whether a missing file is created.
pub const DOTFILE_LIST: &[Binding] = &[
    LIST_ADD,
    LIST_REMOVE,
    b(
        &[Char('t')],
        "t",
        "create",
        "Create the file when it is missing",
        3,
    ),
    LIST_BACK,
    MOVE,
];

const SECURITY: &[Binding] = &[
    b(
        &[Char('a')],
        "a",
        "approve",
        "Approve and install; a key or failed signature asks first",
        1,
    ),
    b(&[Char('x')], "x", "reject", "Reject (asks)", 2),
    b(&[Enter], "Enter", "details", "Open or close the details", 3),
    b(&[Esc], "Esc", "close", "Close the details", 0),
    MOVE,
    b(
        &[Char('A')],
        "A",
        "approve all",
        "Approve all safe packages (asks)",
        4,
    ),
    b(
        &[Char('M')],
        "M",
        "approve machine",
        "Approve all from the item's machine (asks)",
        6,
    ),
];

pub fn tab(tab: Tab) -> &'static [Binding] {
    match tab {
        Tab::Overview => OVERVIEW,
        Tab::Files => FILES,
        Tab::Packages => PACKAGES,
        Tab::Machines => MACHINES,
        Tab::Config => CONFIG,
        Tab::Security => SECURITY,
    }
}

/// The keys of what is on screen now, with a title for the help overlay.
pub fn active(app: &App) -> (&'static str, &'static [Binding]) {
    if app.active_tab == Tab::Config {
        match &app.config.list_edit {
            Some(le) if le.is_dotfile() => return ("Dotfiles list", DOTFILE_LIST),
            Some(_) => return ("Config list", CONFIG_LIST),
            None => {}
        }
    }
    (app.active_tab.title(), tab(app.active_tab))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_footer_binding_has_a_hint_and_a_key() {
        for bindings in
            Tab::all()
                .iter()
                .map(|t| tab(*t))
                .chain([GLOBAL, CONFIG_LIST, DOTFILE_LIST])
        {
            for binding in bindings {
                assert!(!binding.help.is_empty());
                if binding.prio > 0 {
                    assert!(!binding.hint.is_empty() && binding.event().is_some());
                }
            }
        }
    }
}
