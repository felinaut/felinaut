//! Asana: your incomplete tasks, and those due within a week, from Asana's own API. It answers
//! the web session's cookie on a top-level load, but its pages block `fetch`, so the hidden
//! view loads `users/me` for the workspace and hops to the task list from there.

use super::{Source, Spec};

const ME: &str = "https://app.asana.com/api/1.0/users/me?opt_fields=workspaces";

/// shortcut: only the first workspace; loop over `workspaces` if someone has several.
macro_rules! tasks {
    ($filter:literal) => {
        api!(
            signed_out: "d => d.errors",
            hop: "d => location.pathname.endsWith('/users/me') && (d.data.workspaces || [])[0] ? '/api/1.0/tasks?assignee=me&completed_since=now&limit=100&opt_fields=name,permalink_url,modified_at,due_on,due_at,memberships.project.name&workspace=' + d.data.workspaces[0].gid : null",
            list: concat!("d => (Array.isArray(d.data) ? d.data : [])", $filter),
            map: "t => ({ title: t.name, url: t.permalink_url, updated: t.modified_at, detail: [(t.memberships || [])[0]?.project?.name, (t.due_at || t.due_on) && 'due ' + new Date(t.due_at || t.due_on + 'T12:00').toLocaleDateString(undefined, { month: 'short', day: 'numeric' })].filter(Boolean).join(' · ') })",
        )
    };
}

pub static SPEC: Spec = Spec {
    key: "asana",
    name: "Asana",
    noun: "tasks",
    summary: "Your tasks, and those due this week",
    icon: crate::brand::icon::ASANA,
    host: "app.asana.com",
    sign_in_url: "https://app.asana.com/-/login",
    sign_in_paths: &["/-/login"],
    sources: &[
        Source {
            key: "due",
            label: "Due this week",
            url: ME,
            // Overdue too; soonest first.
            extractor: tasks!(
                ".filter(t => (t.due_at || t.due_on) && Date.parse(t.due_at || t.due_on) < Date.now() + 7 * 864e5).sort((a, b) => Date.parse(a.due_at || a.due_on) - Date.parse(b.due_at || b.due_on))"
            ),
        },
        Source {
            key: "mine",
            label: "My tasks",
            url: ME,
            extractor: tasks!(""),
        },
    ],
};
