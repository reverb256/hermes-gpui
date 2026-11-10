//! Kanban view state.
//!
//! The event wiring stays in `app.rs`, like every other surface; this module
//! owns the state those handlers touch so the window type does not accumulate
//! two dozen board-specific fields. All boards are addressed explicitly
//! (`?board=` on every request); the host's active board is never switched.

use hermes_api::{KanbanBoard, KanbanBoardInfo, KanbanTaskDetail};

#[derive(Default)]
pub struct Kanban {
    /// Boards the host offers, and the one this client is showing.
    pub boards: Vec<KanbanBoardInfo>,
    pub selected: Option<String>,
    pub board: Option<KanbanBoard>,
    /// Inline task-title editing and the two-click delete confirmation.
    pub editing: Option<String>,
    pub delete: Option<String>,
    pub status: Option<String>,
    pub draft: Option<String>,
    pub creating: bool,
    /// Board create/rename/remove state.
    pub board_draft: Option<String>,
    pub board_renaming: Option<String>,
    pub board_saving: bool,
    /// Board removal awaiting a second click: `(slug, hard_delete)`.
    pub board_remove: Option<(String, bool)>,
    /// Open task detail, keyed by `(board, task id)` so late responses for a
    /// previously opened task are ignored.
    pub detail_target: Option<(String, String)>,
    pub detail: Option<KanbanTaskDetail>,
    pub comment_draft: Option<String>,
    pub comment_saving: bool,
    pub attachment_delete: Option<i64>,
    pub attachment_busy: bool,
    pub body_draft: Option<String>,
    pub assignee_draft: Option<String>,
    pub priority_draft: Option<String>,
    pub detail_saving: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_board_selects_nothing() {
        let kanban = Kanban::default();
        assert!(kanban.boards.is_empty());
        assert!(kanban.selected.is_none());
        assert!(kanban.board.is_none());
        assert!(kanban.detail_target.is_none());
    }

    #[test]
    fn selection_and_detail_are_independent() {
        let mut kanban = Kanban {
            selected: Some("work".into()),
            ..Kanban::default()
        };
        assert_eq!(kanban.selected.as_deref(), Some("work"));
        assert!(kanban.detail_target.is_none());
        kanban.detail_target = Some(("work".into(), "t_1".into()));
        assert!(kanban.detail_target.is_some());
    }
}
