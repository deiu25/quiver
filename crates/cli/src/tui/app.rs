use std::path::PathBuf;

use toolhub_core::tool::{ToolMeta, ToolType};

use crate::tui::filter;

/// Pure UI state. No I/O. Side effects (e.g. spawning `$EDITOR`) are surfaced
/// via [`SideEffect`] so the event loop in `commands/tui.rs` can act on them.
pub struct App {
    pub tools: Vec<ToolMeta>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub mode: Mode,
    pub search_buf: String,
    /// Search buffer at the moment the modal was opened, restored on Esc.
    pub search_snapshot: Option<String>,
    pub type_filter: Option<ToolType>,
    pub status: String,
    pub detail_scroll: u16,
    pub should_quit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    Detail,
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Back,
    Quit,
    EnterSearch,
    SearchPush(char),
    SearchPop,
    SearchCommit,
    SearchCancel,
    CycleType,
    ClearFilter,
    OpenEditor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideEffect {
    OpenEditor(PathBuf),
}

const TYPE_CYCLE: [Option<ToolType>; 6] = [
    None,
    Some(ToolType::Skill),
    Some(ToolType::Plugin),
    Some(ToolType::Mcp),
    Some(ToolType::Cli),
    Some(ToolType::Doc),
];

const PAGE_JUMP: usize = 10;

impl App {
    pub fn new(tools: Vec<ToolMeta>) -> Self {
        let mut app = Self {
            tools,
            filtered: Vec::new(),
            selected: 0,
            mode: Mode::List,
            search_buf: String::new(),
            search_snapshot: None,
            type_filter: None,
            status: String::new(),
            detail_scroll: 0,
            should_quit: false,
        };
        app.recompute_filtered();
        app
    }

    pub fn selected_tool(&self) -> Option<&ToolMeta> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.tools.get(*i))
    }

    pub fn apply(&mut self, action: AppAction) -> Option<SideEffect> {
        match (self.mode, action) {
            (_, AppAction::Quit) => self.should_quit = true,

            // ---- list nav
            (Mode::List, AppAction::Up) => self.move_selection(-1),
            (Mode::List, AppAction::Down) => self.move_selection(1),
            (Mode::List, AppAction::PageUp) => self.move_selection(-(PAGE_JUMP as isize)),
            (Mode::List, AppAction::PageDown) => self.move_selection(PAGE_JUMP as isize),
            (Mode::List, AppAction::Home) => self.selected = 0,
            (Mode::List, AppAction::End) => {
                self.selected = self.filtered.len().saturating_sub(1);
            }
            (Mode::List, AppAction::Enter) => {
                if self.selected_tool().is_some() {
                    self.mode = Mode::Detail;
                    self.detail_scroll = 0;
                }
            }
            (Mode::List, AppAction::EnterSearch) => {
                self.search_snapshot = Some(self.search_buf.clone());
                self.mode = Mode::Search;
            }
            (Mode::List, AppAction::CycleType) => {
                let cur = TYPE_CYCLE
                    .iter()
                    .position(|t| *t == self.type_filter)
                    .unwrap_or(0);
                let next = (cur + 1) % TYPE_CYCLE.len();
                self.type_filter = TYPE_CYCLE[next];
                self.recompute_filtered();
            }
            (Mode::List, AppAction::ClearFilter) => {
                self.search_buf.clear();
                self.type_filter = None;
                self.recompute_filtered();
            }

            // ---- detail
            (Mode::Detail, AppAction::Up) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            (Mode::Detail, AppAction::Down) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            (Mode::Detail, AppAction::PageUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(PAGE_JUMP as u16);
            }
            (Mode::Detail, AppAction::PageDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(PAGE_JUMP as u16);
            }
            (Mode::Detail, AppAction::Back) => {
                self.mode = Mode::List;
            }
            (Mode::Detail, AppAction::OpenEditor) => {
                return self.open_editor_side_effect();
            }

            // ---- search modal
            (Mode::Search, AppAction::SearchPush(c)) => {
                self.search_buf.push(c);
                self.recompute_filtered();
            }
            (Mode::Search, AppAction::SearchPop) => {
                self.search_buf.pop();
                self.recompute_filtered();
            }
            (Mode::Search, AppAction::SearchCommit) => {
                self.search_snapshot = None;
                self.mode = Mode::List;
            }
            (Mode::Search, AppAction::SearchCancel) => {
                if let Some(prev) = self.search_snapshot.take() {
                    self.search_buf = prev;
                    self.recompute_filtered();
                }
                self.mode = Mode::List;
            }

            _ => {}
        }
        None
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.filtered.len() as isize;
        let mut next = self.selected as isize + delta;
        if next < 0 {
            next = 0;
        }
        if next >= len {
            next = len - 1;
        }
        self.selected = next as usize;
    }

    fn open_editor_side_effect(&mut self) -> Option<SideEffect> {
        let Some(tool) = self.selected_tool() else {
            self.status = "no tool selected".into();
            return None;
        };
        let Some(install_path) = tool.install_path.as_deref() else {
            self.status = "no install path".into();
            return None;
        };
        let path = resolve_edit_target(install_path);
        Some(SideEffect::OpenEditor(path))
    }

    pub fn recompute_filtered(&mut self) {
        self.filtered = filter::apply(&self.tools, &self.search_buf, self.type_filter);
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

fn resolve_edit_target(install_path: &str) -> PathBuf {
    let p = PathBuf::from(install_path);
    if p.is_dir() {
        let skill_md = p.join("SKILL.md");
        if skill_md.exists() {
            return skill_md;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn meta(id: &str, name: &str, ttype: ToolType, install: Option<&str>) -> ToolMeta {
        let now = Utc::now();
        ToolMeta {
            id: id.into(),
            r#type: ttype,
            name: name.into(),
            source_repo: None,
            install_path: install.map(String::from),
            description: Some(format!("{name} desc")),
            long_description: None,
            category: None,
            triggers: vec![],
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        }
    }

    fn fixture() -> Vec<ToolMeta> {
        vec![
            meta("skill:design-md", "design-md", ToolType::Skill, Some("/tmp/d")),
            meta("skill:enhance", "enhance-prompt", ToolType::Skill, None),
            meta("plugin:caveman", "caveman", ToolType::Plugin, Some("/tmp/c")),
            meta("mcp:ruflo", "ruflo", ToolType::Mcp, None),
            meta("cli:codeburn", "codeburn", ToolType::Cli, Some("/tmp/cb")),
        ]
    }

    #[test]
    fn new_filters_to_full_list() {
        let app = App::new(fixture());
        assert_eq!(app.filtered.len(), 5);
        assert_eq!(app.selected, 0);
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn down_advances_selection_clamped() {
        let mut app = App::new(fixture());
        for _ in 0..10 {
            app.apply(AppAction::Down);
        }
        assert_eq!(app.selected, 4);
    }

    #[test]
    fn up_clamps_to_zero() {
        let mut app = App::new(fixture());
        app.apply(AppAction::Up);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn page_down_jumps_ten() {
        let mut tools = fixture();
        for i in 0..20 {
            tools.push(meta(
                &format!("skill:extra-{i}"),
                &format!("extra-{i}"),
                ToolType::Skill,
                None,
            ));
        }
        let mut app = App::new(tools);
        app.apply(AppAction::PageDown);
        assert_eq!(app.selected, 10);
    }

    #[test]
    fn enter_then_back_round_trips_mode() {
        let mut app = App::new(fixture());
        app.apply(AppAction::Enter);
        assert_eq!(app.mode, Mode::Detail);
        app.apply(AppAction::Back);
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn search_push_filters_live() {
        let mut app = App::new(fixture());
        app.apply(AppAction::EnterSearch);
        for c in "design".chars() {
            app.apply(AppAction::SearchPush(c));
        }
        assert_eq!(app.filtered, vec![0]);
        assert_eq!(app.mode, Mode::Search);
    }

    #[test]
    fn search_cancel_restores_prior_filter() {
        let mut app = App::new(fixture());
        app.apply(AppAction::EnterSearch);
        for c in "design".chars() {
            app.apply(AppAction::SearchPush(c));
        }
        app.apply(AppAction::SearchCancel);
        assert_eq!(app.mode, Mode::List);
        assert_eq!(app.search_buf, "");
        assert_eq!(app.filtered.len(), 5);
    }

    #[test]
    fn search_commit_keeps_filter() {
        let mut app = App::new(fixture());
        app.apply(AppAction::EnterSearch);
        for c in "caveman".chars() {
            app.apply(AppAction::SearchPush(c));
        }
        app.apply(AppAction::SearchCommit);
        assert_eq!(app.mode, Mode::List);
        assert_eq!(app.filtered, vec![2]);
    }

    #[test]
    fn cycle_type_rotates_through_all_variants_then_none() {
        let mut app = App::new(fixture());
        let expected = [
            Some(ToolType::Skill),
            Some(ToolType::Plugin),
            Some(ToolType::Mcp),
            Some(ToolType::Cli),
            Some(ToolType::Doc),
            None,
        ];
        for want in expected {
            app.apply(AppAction::CycleType);
            assert_eq!(app.type_filter, want);
        }
    }

    #[test]
    fn clear_filter_restores_full_list() {
        let mut app = App::new(fixture());
        app.apply(AppAction::CycleType);
        assert_eq!(app.filtered.len(), 2);
        app.apply(AppAction::ClearFilter);
        assert_eq!(app.filtered.len(), 5);
        assert_eq!(app.type_filter, None);
    }

    #[test]
    fn open_editor_returns_side_effect_with_install_path() {
        let mut app = App::new(fixture());
        let eff = app.apply(AppAction::OpenEditor);
        assert!(eff.is_none());
        app.apply(AppAction::Enter);
        let eff = app.apply(AppAction::OpenEditor);
        match eff {
            Some(SideEffect::OpenEditor(p)) => assert_eq!(p, PathBuf::from("/tmp/d")),
            other => panic!("expected OpenEditor, got {other:?}"),
        }
    }

    #[test]
    fn open_editor_without_install_path_sets_status() {
        let mut app = App::new(fixture());
        app.apply(AppAction::Down);
        app.apply(AppAction::Enter);
        let eff = app.apply(AppAction::OpenEditor);
        assert!(eff.is_none());
        assert_eq!(app.status, "no install path");
    }

    #[test]
    fn quit_sets_should_quit_from_any_mode() {
        let mut app = App::new(fixture());
        app.apply(AppAction::Enter);
        app.apply(AppAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn selected_clamps_when_filter_shrinks() {
        let mut app = App::new(fixture());
        app.apply(AppAction::End);
        app.apply(AppAction::CycleType);
        assert!(app.selected < app.filtered.len());
    }
}
