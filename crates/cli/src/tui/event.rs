use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::app::{AppAction, Mode};

/// Translate a crossterm key event into an [`AppAction`] given the current
/// [`Mode`]. Returns `None` for keys that have no mapping in the current mode.
pub fn map(mode: Mode, k: KeyEvent) -> Option<AppAction> {
    if k.kind != KeyEventKind::Press {
        return None;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
        return Some(AppAction::Quit);
    }
    match mode {
        Mode::List => map_list(k),
        Mode::Detail => map_detail(k),
        Mode::Search => map_search(k),
    }
}

fn map_list(k: KeyEvent) -> Option<AppAction> {
    Some(match k.code {
        KeyCode::Char('q') => AppAction::Quit,
        KeyCode::Up => AppAction::Up,
        KeyCode::Down => AppAction::Down,
        KeyCode::PageUp => AppAction::PageUp,
        KeyCode::PageDown => AppAction::PageDown,
        KeyCode::Home => AppAction::Home,
        KeyCode::End => AppAction::End,
        KeyCode::Enter => AppAction::Enter,
        KeyCode::Char('/') => AppAction::EnterSearch,
        KeyCode::Tab => AppAction::CycleType,
        KeyCode::Esc => AppAction::ClearFilter,
        _ => return None,
    })
}

fn map_detail(k: KeyEvent) -> Option<AppAction> {
    Some(match k.code {
        KeyCode::Char('q') => AppAction::Quit,
        KeyCode::Esc | KeyCode::Backspace => AppAction::Back,
        KeyCode::Up => AppAction::Up,
        KeyCode::Down => AppAction::Down,
        KeyCode::PageUp => AppAction::PageUp,
        KeyCode::PageDown => AppAction::PageDown,
        KeyCode::Char('e') => AppAction::OpenEditor,
        _ => return None,
    })
}

fn map_search(k: KeyEvent) -> Option<AppAction> {
    Some(match k.code {
        KeyCode::Esc => AppAction::SearchCancel,
        KeyCode::Enter => AppAction::SearchCommit,
        KeyCode::Backspace => AppAction::SearchPop,
        KeyCode::Char(c) => AppAction::SearchPush(c),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn list_arrows_map_to_nav() {
        assert_eq!(map(Mode::List, key(KeyCode::Up)), Some(AppAction::Up));
        assert_eq!(map(Mode::List, key(KeyCode::Down)), Some(AppAction::Down));
        assert_eq!(map(Mode::List, key(KeyCode::Enter)), Some(AppAction::Enter));
    }

    #[test]
    fn list_slash_opens_search() {
        assert_eq!(
            map(Mode::List, key(KeyCode::Char('/'))),
            Some(AppAction::EnterSearch)
        );
    }

    #[test]
    fn list_tab_cycles_type() {
        assert_eq!(map(Mode::List, key(KeyCode::Tab)), Some(AppAction::CycleType));
    }

    #[test]
    fn list_esc_clears_filter() {
        assert_eq!(map(Mode::List, key(KeyCode::Esc)), Some(AppAction::ClearFilter));
    }

    #[test]
    fn detail_e_opens_editor() {
        assert_eq!(
            map(Mode::Detail, key(KeyCode::Char('e'))),
            Some(AppAction::OpenEditor)
        );
    }

    #[test]
    fn detail_esc_returns_to_list() {
        assert_eq!(map(Mode::Detail, key(KeyCode::Esc)), Some(AppAction::Back));
    }

    #[test]
    fn search_chars_push_and_backspace_pops() {
        assert_eq!(
            map(Mode::Search, key(KeyCode::Char('x'))),
            Some(AppAction::SearchPush('x'))
        );
        assert_eq!(
            map(Mode::Search, key(KeyCode::Backspace)),
            Some(AppAction::SearchPop)
        );
    }

    #[test]
    fn search_enter_commits_esc_cancels() {
        assert_eq!(
            map(Mode::Search, key(KeyCode::Enter)),
            Some(AppAction::SearchCommit)
        );
        assert_eq!(
            map(Mode::Search, key(KeyCode::Esc)),
            Some(AppAction::SearchCancel)
        );
    }

    #[test]
    fn ctrl_c_quits_from_any_mode() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map(Mode::List, ev), Some(AppAction::Quit));
        assert_eq!(map(Mode::Detail, ev), Some(AppAction::Quit));
        assert_eq!(map(Mode::Search, ev), Some(AppAction::Quit));
    }

    #[test]
    fn unmapped_keys_return_none() {
        assert_eq!(map(Mode::List, key(KeyCode::F(5))), None);
    }
}
