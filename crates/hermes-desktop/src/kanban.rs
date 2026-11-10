//! Kanban view state.
//!
//! The event wiring stays in `app.rs`, like every other surface; this module
//! owns the state those handlers touch so the window type does not accumulate
//! two dozen board-specific fields. All boards are addressed explicitly
//! (`?board=` on every request); the host's active board is never switched.

use gpui::prelude::*;
use gpui::{Context, SharedString, div, rgb};
use hermes_api::{KanbanBoard, KanbanBoardInfo, KanbanTaskDetail};

use crate::app::HermesApp;
use crate::backend::Command;
use crate::theme::*;

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

impl HermesApp {
    fn kanban_size_label(bytes: u64) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{bytes} B")
        }
    }

    pub(crate) fn kanban_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("kanban-board")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();
        if let Some(status) = &self.kanban.status {
            list = list.child(self.note(status));
        }
        if let Some(detail) = &self.kanban.detail {
            let task = &detail.task;
            let mut panel = div()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_3()
                .rounded_lg()
                .bg(rgb(surface()))
                .child(self.section_label(&format!("TASK · {}", task.status)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(SharedString::from(task.title.clone())),
                );
            for (label, value) in [
                ("Description", task.body.as_deref()),
                ("Assignee", task.assignee.as_deref()),
                ("Summary", task.latest_summary.as_deref()),
                ("Result", task.result.as_deref()),
            ] {
                if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                    panel = panel.child(self.section_label(label)).child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(value.to_string())),
                    );
                }
            }
            panel = panel.child(self.section_label("PRIORITY")).child(
                div()
                    .text_sm()
                    .text_color(rgb(text()))
                    .child(SharedString::from(task.priority.to_string())),
            );
            panel = panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .id("kanban-edit-body")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.detail_saving && !this.kanban.attachment_busy {
                                    let current = this
                                        .kanban
                                        .detail
                                        .as_ref()
                                        .and_then(|detail| detail.task.body.clone())
                                        .unwrap_or_default();
                                    this.kanban.assignee_draft = None;
                                    this.kanban.body_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit description"),
                    )
                    .child(
                        div()
                            .id("kanban-edit-assignee")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.detail_saving && !this.kanban.attachment_busy {
                                    let current = this
                                        .kanban
                                        .detail
                                        .as_ref()
                                        .and_then(|detail| detail.task.assignee.clone())
                                        .unwrap_or_default();
                                    this.kanban.body_draft = None;
                                    this.kanban.assignee_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit assignee"),
                    )
                    .child(
                        div()
                            .id("kanban-edit-priority")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.detail_saving && !this.kanban.attachment_busy {
                                    let current = this
                                        .kanban
                                        .detail
                                        .as_ref()
                                        .map(|detail| detail.task.priority.to_string())
                                        .unwrap_or_default();
                                    this.kanban.body_draft = None;
                                    this.kanban.assignee_draft = None;
                                    this.kanban.priority_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit priority"),
                    ),
            );
            if let Some(draft) = &self.kanban.body_draft {
                panel = panel
                    .child(self.section_label("DESCRIPTION"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Ctrl+Enter saves · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-body-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_body(cx)),
                                    )
                                    .child(if self.kanban.detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save description"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-body-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban.detail_saving {
                                            this.kanban.body_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            if let Some(draft) = &self.kanban.assignee_draft {
                panel = panel
                    .child(self.section_label("ASSIGNEE"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Enter saves · empty clears the assignee · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-assignee-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_assignee(cx)),
                                    )
                                    .child(if self.kanban.detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save assignee"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-assignee-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban.detail_saving {
                                            this.kanban.assignee_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            if let Some(draft) = &self.kanban.priority_draft {
                panel = panel
                    .child(self.section_label("PRIORITY"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Digits only · Enter saves · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-priority-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_priority(cx)),
                                    )
                                    .child(if self.kanban.detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save priority"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-priority-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban.detail_saving {
                                            this.kanban.priority_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            panel = panel
                .child(self.section_label(&format!("ATTACHMENTS · {}", detail.attachments.len())));
            if detail.attachments.is_empty() {
                panel = panel.child(self.note("No attachments."));
            }
            for attachment in &detail.attachments {
                let attachment_id = attachment.id;
                let removing = self.kanban.attachment_delete == Some(attachment_id);
                let mut row = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(attachment.filename.clone())),
                            )
                            .child(div().text_xs().text_color(rgb(text_muted())).child(
                                SharedString::from(format!(
                                    "{} · {}",
                                    Self::kanban_size_label(attachment.size),
                                    attachment
                                        .uploaded_by
                                        .clone()
                                        .unwrap_or_else(|| "unknown".into())
                                )),
                            )),
                    );
                if removing {
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-remove-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_kanban_attachment(attachment_id, cx)
                                }))
                                .child("Confirm remove"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-cancel-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kanban.attachment_delete = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        );
                } else {
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-save-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_kanban_attachment(attachment_id, cx)
                                }))
                                .child("Save"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-delete-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.kanban.attachment_busy {
                                        this.kanban.attachment_delete = Some(attachment_id);
                                        cx.notify();
                                    }
                                }))
                                .child("Remove"),
                        );
                }
                panel = panel.child(row);
            }
            panel = panel.child(
                div()
                    .id("kanban-attach-file")
                    .text_sm()
                    .text_color(rgb(accent()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.attach_kanban_file(cx)))
                    .child(if self.kanban.attachment_busy {
                        "Attaching…"
                    } else {
                        "Attach file"
                    }),
            );
            panel = panel.child(self.section_label("COMMENTS"));
            if detail.comments.is_empty() {
                panel = panel.child(self.note("No comments yet."));
            }
            for comment in &detail.comments {
                panel = panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .child(div().text_xs().text_color(rgb(text_muted())).child(
                            SharedString::from(
                                comment.author.clone().unwrap_or_else(|| "Unknown".into()),
                            ),
                        ))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(text()))
                                .child(SharedString::from(comment.body.clone())),
                        ),
                );
            }
            if let Some(draft) = &self.kanban.comment_draft {
                panel = panel
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Enter sends · Shift+Enter adds a line"))
                    .child(
                        div()
                            .id("kanban-comment-submit")
                            .px_3()
                            .py_1()
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.submit_kanban_comment(cx)))
                            .child(if self.kanban.comment_saving {
                                "Sending…"
                            } else {
                                "Send comment"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-comment-cancel")
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.comment_saving {
                                    this.kanban.comment_draft = None;
                                    cx.notify();
                                }
                            }))
                            .child("Cancel comment"),
                    );
            } else {
                panel = panel.child(
                    div()
                        .id("kanban-comment-new")
                        .text_sm()
                        .text_color(rgb(accent()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.kanban.creating
                                && !this.kanban.board_saving
                                && this.kanban.draft.is_none()
                                && this.kanban.board_draft.is_none()
                            {
                                this.kanban.comment_draft = Some(String::new());
                                cx.notify();
                            }
                        }))
                        .child("Add comment"),
                );
            }
            panel = panel.child(
                div()
                    .id("kanban-detail-close")
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.kanban.comment_saving
                            && !this.kanban.attachment_busy
                            && !this.kanban.detail_saving
                        {
                            this.kanban.detail_target = None;
                            this.kanban.detail = None;
                            this.kanban.comment_draft = None;
                            this.kanban.attachment_delete = None;
                            this.kanban.body_draft = None;
                            this.kanban.assignee_draft = None;
                            this.kanban.priority_draft = None;
                            cx.notify();
                        }
                    }))
                    .child("Close details"),
            );
            list = list.child(panel);
        }
        if let Some(draft) = &self.kanban.board_draft {
            list = list.child(self.note(if self.kanban.board_renaming.is_some() {
                "Board display name (slug stays unchanged)"
            } else {
                "New board slug (host validates it; existing slugs open the existing board)"
            }));
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(
                        div()
                            .id("kanban-board-save")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.save_kanban_board(cx)))
                            .child(if self.kanban.board_saving {
                                "Saving…"
                            } else {
                                "Save"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-board-cancel")
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.board_saving {
                                    this.kanban.board_draft = None;
                                    this.kanban.board_renaming = None;
                                    cx.notify();
                                }
                            }))
                            .child("Cancel"),
                    ),
            );
        }
        if let Some(draft) = &self.kanban.draft {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(
                        div()
                            .id("kanban-create-submit")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.create_kanban_task(cx)))
                            .child(if self.kanban.editing.is_some() {
                                "Save"
                            } else {
                                "Create"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-create-cancel")
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.creating {
                                    this.kanban.draft = None;
                                    this.kanban.editing = None;
                                }
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            );
            if self.kanban.editing.is_none() {
                list = list.child(
                    self.note("New tasks start in triage; no worker is assigned or dispatched."),
                );
            }
        }
        if !self.kanban.boards.is_empty() {
            let mut choices = div().flex().flex_wrap().gap_2();
            for board in &self.kanban.boards {
                let slug = board.slug.clone();
                let selected = self.kanban.selected.as_ref() == Some(&slug);
                choices = choices.child(
                    div()
                        .id(SharedString::from(format!("kanban-board-{slug}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |v| {
                            v.bg(rgb(accent_soft())).text_color(rgb(accent()))
                        })
                        .when(!selected, |v| v.bg(rgb(surface())).text_color(rgb(text())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.kanban.creating
                                && !this.kanban.board_saving
                                && this.kanban.board_draft.is_none()
                                && !this.kanban.comment_saving
                                && this.kanban.comment_draft.is_none()
                                && !this.kanban.attachment_busy
                                && !this.kanban.detail_saving
                                && this.kanban.body_draft.is_none()
                                && this.kanban.assignee_draft.is_none()
                            {
                                this.kanban.selected = Some(slug.clone());
                                this.kanban.detail_target = None;
                                this.kanban.detail = None;
                                this.kanban.attachment_delete = None;
                                this.kanban.board_remove = None;
                                this.kanban.body_draft = None;
                                this.kanban.assignee_draft = None;
                                this.kanban.priority_draft = None;
                                this.kanban.comment_draft = None;
                                this.kanban.board = None;
                                this.kanban.draft = None;
                                this.kanban.editing = None;
                                this.kanban.delete = None;
                                this.kanban.status = Some("Loading board…".into());
                                this.backend.send(Command::LoadKanban {
                                    board: Some(slug.clone()),
                                });
                                cx.notify();
                            }
                        }))
                        .child(SharedString::from(board.name.clone())),
                );
            }
            list = list.child(choices);
        }
        if let Some(selected) = &self.kanban.selected {
            let archive_pending =
                self.kanban.board_remove.as_ref() == Some(&(selected.clone(), false));
            let delete_pending =
                self.kanban.board_remove.as_ref() == Some(&(selected.clone(), true));
            let archive_slug = selected.clone();
            let delete_slug = selected.clone();
            let archive_label = if archive_pending {
                "Confirm archive"
            } else {
                "Archive board"
            };
            let delete_label = if delete_pending {
                "Confirm delete"
            } else {
                "Delete board"
            };
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("kanban-board-archive")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.kanban.board_saving
                                    || this.kanban.detail_saving
                                    || this.kanban.attachment_busy
                                    || this.kanban.comment_saving
                                {
                                    return;
                                }
                                if this.kanban.board_remove.as_ref()
                                    == Some(&(archive_slug.clone(), false))
                                {
                                    this.kanban.board_remove = None;
                                    this.kanban.status = Some("Archiving board…".into());
                                    this.backend.send(Command::RemoveKanbanBoard {
                                        slug: archive_slug.clone(),
                                        hard: false,
                                    });
                                } else {
                                    this.kanban.board_remove =
                                        Some((archive_slug.clone(), false));
                                    this.kanban.status = Some(
                                        "Archive this board? Click again to confirm; its tasks stay on disk."
                                            .into(),
                                    );
                                }
                                cx.notify();
                            }))
                            .child(archive_label),
                    )
                    .child(
                        div()
                            .id("kanban-board-delete")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(danger()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.kanban.board_saving
                                    || this.kanban.detail_saving
                                    || this.kanban.attachment_busy
                                    || this.kanban.comment_saving
                                {
                                    return;
                                }
                                if this.kanban.board_remove.as_ref()
                                    == Some(&(delete_slug.clone(), true))
                                {
                                    this.kanban.board_remove = None;
                                    this.kanban.status = Some("Deleting board…".into());
                                    this.backend.send(Command::RemoveKanbanBoard {
                                        slug: delete_slug.clone(),
                                        hard: true,
                                    });
                                } else {
                                    this.kanban.board_remove = Some((delete_slug.clone(), true));
                                    this.kanban.status = Some(
                                        "Delete this board and its tasks permanently? Click again to confirm."
                                            .into(),
                                    );
                                }
                                cx.notify();
                            }))
                            .child(delete_label),
                    )
                    .when(self.kanban.board_remove.is_some(), |this| {
                        this.child(
                            div()
                                .id("kanban-board-remove-cancel")
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kanban.board_remove = None;
                                    this.kanban.status = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        )
                    }),
            );
        }
        if let Some(board) = &self.kanban.board {
            for column in &board.columns {
                let mut section =
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.section_label(&format!(
                            "{} · {}",
                            column.name.to_uppercase(),
                            column.tasks.len()
                        )));
                for task in &column.tasks {
                    let id = task.id.clone();
                    let next = match task.status.as_str() {
                        "triage" => Some("todo"),
                        "todo" => Some("ready"),
                        "ready" | "running" => Some("review"),
                        "review" => Some("done"),
                        "blocked" | "scheduled" => Some("ready"),
                        _ => None,
                    };
                    let mut row = div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(bg_elev()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(task.title.clone())),
                                )
                                .when_some(task.assignee.clone(), |this, assignee| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .child(SharedString::from(assignee)),
                                    )
                                }),
                        );
                    if let Some(next) = next {
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("kanban-move-{id}")))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(surface()))
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.kanban.status = Some(format!("Moving task to {next}…"));
                                    this.backend.send(Command::UpdateKanbanTask {
                                        id: id.clone(),
                                        patch: serde_json::json!({"status": next}),
                                        board: this.kanban.selected.clone(),
                                        editing: false,
                                    });
                                    cx.notify();
                                }))
                                .child(SharedString::from(format!("→ {next}"))),
                        );
                    }
                    let detail_id = task.id.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("kanban-detail-{}", task.id)))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.kanban.comment_saving
                                    && this.kanban.comment_draft.is_none()
                                    && this.kanban.board_draft.is_none()
                                    && this.kanban.draft.is_none()
                                    && let Some(board) = this.kanban.selected.clone()
                                {
                                    this.kanban.detail_target =
                                        Some((board.clone(), detail_id.clone()));
                                    this.kanban.detail = None;
                                    this.kanban.attachment_delete = None;
                                    this.kanban.body_draft = None;
                                    this.kanban.assignee_draft = None;
                                    this.kanban.status = Some("Loading task details…".into());
                                    this.backend.send(Command::LoadKanbanTask {
                                        board,
                                        id: detail_id.clone(),
                                    });
                                    cx.notify();
                                }
                            }))
                            .child("Details"),
                    );
                    let edit_id = task.id.clone();
                    let edit_title = task.title.clone();
                    let delete_id = task.id.clone();
                    let confirm_delete = self.kanban.delete.as_deref() == Some(task.id.as_str());
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!("kanban-edit-{}", task.id)))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.kanban.creating
                                        && !this.kanban.board_saving
                                        && this.kanban.comment_draft.is_none()
                                        && !this.kanban.comment_saving
                                        && this.kanban.board_draft.is_none()
                                    {
                                        this.kanban.editing = Some(edit_id.clone());
                                        this.kanban.draft = Some(edit_title.clone());
                                        this.kanban.delete = None;
                                        cx.notify();
                                    }
                                }))
                                .child("Edit"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("kanban-delete-{}", task.id)))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.kanban.delete.as_deref() == Some(delete_id.as_str()) {
                                        this.kanban.delete = None;
                                        this.kanban.status = Some("Deleting task…".into());
                                        this.backend.send(Command::DeleteKanbanTask {
                                            id: delete_id.clone(),
                                            board: this.kanban.selected.clone(),
                                        });
                                    } else {
                                        this.kanban.delete = Some(delete_id.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(if confirm_delete {
                                    "Confirm delete"
                                } else {
                                    "Delete"
                                }),
                        );
                    section = section.child(row);
                }
                list = list.child(section);
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_xs().text_color(rgb(text_muted())).child(
                        "Board selection is local · status changes use the host's validation",
                    ))
                    .child(
                        div()
                            .id("kanban-create-board")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.board_saving
                                    && !this.kanban.creating
                                    && this.kanban.draft.is_none()
                                    && this.kanban.comment_draft.is_none()
                                    && !this.kanban.comment_saving
                                {
                                    this.kanban.board_renaming = None;
                                    this.kanban.board_draft = Some(String::new());
                                    cx.notify();
                                }
                            }))
                            .child("＋ Board"),
                    )
                    .child(
                        div()
                            .id("kanban-rename-board")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban.board_saving
                                    && !this.kanban.creating
                                    && this.kanban.draft.is_none()
                                    && this.kanban.comment_draft.is_none()
                                    && !this.kanban.comment_saving
                                    && let Some(board) = this.kanban.boards.iter().find(|board| {
                                        Some(&board.slug) == this.kanban.selected.as_ref()
                                    })
                                {
                                    this.kanban.board_renaming = Some(board.slug.clone());
                                    this.kanban.board_draft = Some(board.name.clone());
                                    cx.notify();
                                }
                            }))
                            .child("Rename board"),
                    )
                    .child(
                        div()
                            .id("kanban-create")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(accent_soft()))
                            .text_xs()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.kanban.draft.is_none()
                                    && this.kanban.board_draft.is_none()
                                    && !this.kanban.board_saving
                                    && this.kanban.comment_draft.is_none()
                                    && !this.kanban.comment_saving
                                {
                                    this.kanban.editing = None;
                                    this.kanban.draft = Some(String::new());
                                }
                                cx.notify();
                            }))
                            .child("＋ Task"),
                    )
                    .child(
                        div()
                            .id("kanban-refresh")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(bg_elev()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.open_kanban(cx)))
                            .child("Refresh"),
                    ),
            )
            .child(list)
            .into_any_element()
    }
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
