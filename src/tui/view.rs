//! Ratatui rendering for the interactive digest pane.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::model::{CheckoutDigest, Commit, Head, Landed, RepoDigest};
use crate::render;

use super::state::DigestPane;

const HELP_WIDE: &str = "↑/k ↓/j move  wheel scroll  click focus  Enter expand  Esc collapse/back  t today  y yesterday  l since-last  R refresh  q quit";
const HELP_NARROW: &str = "↑↓/jk move · Enter expand · t/y/l window · R refresh · q quit";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MouseMap {
    rows: Vec<HitRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HitRow {
    y: u16,
    left: u16,
    right: u16,
    cursor: usize,
}

impl MouseMap {
    pub fn cursor_at(&self, column: u16, row: u16) -> Option<usize> {
        self.rows
            .iter()
            .find(|hit| hit.y == row && column >= hit.left && column < hit.right)
            .map(|hit| hit.cursor)
    }
}

pub fn render(frame: &mut Frame<'_>, pane: &DigestPane) -> MouseMap {
    let area = frame.area();
    if area.is_empty() {
        return MouseMap::default();
    }
    let header_height = if area.height >= 12 { 4 } else { 3 };
    let footer_height = if area.height >= 18 { 7 } else { 4 };
    let regions = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(3),
        Constraint::Length(footer_height),
    ])
    .split(area);

    render_header(frame, pane, regions[0]);
    let mouse = render_body(frame, pane, regions[1]);
    render_footer(frame, pane, regions[2]);
    mouse
}

fn normal() -> Style {
    Style::default().fg(Color::Reset)
}

fn render_header(frame: &mut Frame<'_>, pane: &DigestPane, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(normal())
        .title(Span::styled(
            " standup · interactive digest ",
            normal().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let end = pane
        .digest
        .window
        .until
        .as_ref()
        .unwrap_or(&pane.digest.generated_at);
    let window = format!(
        "{} · {} → {}",
        pane.active.label(),
        pane.digest.window.since.full(),
        end.full()
    );
    let totals = render::repo_stats(pane.digest.total_commits(), pane.digest.total_churn());
    frame.render_widget(
        Paragraph::new(vec![Line::from(window), Line::from(totals)]).style(normal()),
        inner,
    );
}

#[derive(Debug)]
enum BodyLine<'a> {
    Repository(&'a RepoDigest),
    Checkout {
        cursor: usize,
        checkout: &'a CheckoutDigest,
    },
    Commit(&'a Commit),
}

fn body_lines(pane: &DigestPane) -> Vec<BodyLine<'_>> {
    let mut lines = Vec::new();
    let mut cursor = 0;
    for repo in render::sorted_repos(&pane.digest) {
        lines.push(BodyLine::Repository(repo));
        for checkout in render::sorted_checkouts(repo) {
            lines.push(BodyLine::Checkout { cursor, checkout });
            if pane.expanded.contains(&checkout.report.path) {
                lines.extend(
                    render::sorted_commits(&checkout.report)
                        .into_iter()
                        .map(BodyLine::Commit),
                );
            }
            cursor += 1;
        }
    }
    lines
}

fn render_body(frame: &mut Frame<'_>, pane: &DigestPane, area: Rect) -> MouseMap {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(normal())
        .title(Span::styled(
            " workspaces by repository ",
            normal().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return MouseMap::default();
    }
    if pane.row_count() == 0 {
        frame.render_widget(
            Paragraph::new("no workspaces found.").style(normal()),
            inner,
        );
        return MouseMap::default();
    }

    let lines = body_lines(pane);
    let capacity = inner.height as usize;
    let cursor_line = lines
        .iter()
        .position(
            |line| matches!(line, BodyLine::Checkout { cursor, .. } if *cursor == pane.cursor),
        )
        .unwrap_or(0);
    let top = if lines.len() <= capacity {
        0
    } else {
        cursor_line
            .saturating_add(1)
            .saturating_sub(capacity)
            .min(lines.len() - capacity)
    };

    let mut mouse = MouseMap::default();
    for (offset, line) in lines.iter().skip(top).take(capacity).enumerate() {
        let row = Rect::new(inner.x, inner.y + offset as u16, inner.width, 1);
        match line {
            BodyLine::Repository(repo) => render_repository(frame, repo, row),
            BodyLine::Checkout { cursor, checkout } => {
                render_checkout(frame, pane, *cursor, checkout, row);
                mouse.rows.push(HitRow {
                    y: row.y,
                    left: row.x,
                    right: row.x.saturating_add(row.width),
                    cursor: *cursor,
                });
            }
            BodyLine::Commit(commit) => render_commit(frame, commit, row),
        }
    }
    mouse
}

fn render_repository(frame: &mut Frame<'_>, repo: &RepoDigest, area: Rect) {
    let stats = render::repo_stats(repo.commits, repo.churn);
    let width = area.width as usize;
    let name_budget = width
        .saturating_sub(render::display_width(&stats) + 5)
        .max(8);
    let name = render::truncate_right(&repo.name, name_budget);
    let gap = width
        .saturating_sub(render::display_width(&name) + render::display_width(&stats) + 2)
        .max(1);
    frame.render_widget(
        Paragraph::new(format!(" {name}{}{stats}", " ".repeat(gap)))
            .style(normal().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn render_checkout(
    frame: &mut Frame<'_>,
    pane: &DigestPane,
    cursor: usize,
    checkout: &CheckoutDigest,
    area: Rect,
) {
    let selected = pane.cursor == cursor;
    let row_style = if selected {
        normal().add_modifier(Modifier::REVERSED)
    } else {
        normal()
    };
    frame.render_widget(Block::default().style(row_style), area);

    let expanded = pane.expanded.contains(&checkout.report.path);
    let marker = if expanded { '▾' } else { '▸' };
    let workspace = workspace_label(checkout);
    let branch = render::head_label(&checkout.report.head);
    let volume = if area.width < 90 {
        format!(
            "{}c {}f +{} −{}",
            checkout.report.commits.len(),
            checkout.report.churn.files,
            checkout.report.churn.insertions,
            checkout.report.churn.deletions
        )
    } else {
        render::repo_stats(checkout.report.commits.len(), checkout.report.churn)
    };
    let tags = status_specs(checkout);
    let tags_width = tags
        .iter()
        .map(|(label, _)| render::display_width(label) + 1)
        .sum::<usize>();
    let columns = (area.width as usize).saturating_sub(4 + tags_width + 3);
    let workspace_width = (columns * 2 / 5).max(1);
    let branch_width = (columns * 3 / 10).max(1);
    let volume_width = columns
        .saturating_sub(workspace_width + branch_width)
        .max(1);
    let workspace = render::truncate_right(&workspace, workspace_width);
    let branch = render::truncate_right(&branch, branch_width);
    let volume = render::truncate_right(&volume, volume_width);
    let mut spans = vec![Span::styled(
        format!(
            " {marker} {workspace:<workspace_width$} {branch:<branch_width$} {volume:<volume_width$} "
        ),
        row_style,
    )];
    spans.extend(
        tags.into_iter()
            .map(|(label, color)| tag(label, color, row_style)),
    );
    frame.render_widget(Paragraph::new(Line::from(spans)).style(normal()), area);
}

fn workspace_label(checkout: &CheckoutDigest) -> String {
    if checkout.workspaces.is_empty() {
        let name = checkout
            .report
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| checkout.report.path.display().to_string());
        return format!("closed · {name}");
    }
    checkout
        .workspaces
        .iter()
        .map(|workspace| match workspace.number {
            Some(number) => format!("#{number} {}", workspace.label),
            None => workspace.label.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn tag(text: &'static str, color: Color, row_style: Style) -> Span<'static> {
    Span::styled(
        format!("{text} "),
        row_style.fg(color).add_modifier(Modifier::BOLD),
    )
}

fn status_specs(checkout: &CheckoutDigest) -> Vec<(&'static str, Color)> {
    let report = &checkout.report;
    let mut tags = Vec::new();
    if !report.problems.is_empty() || matches!(report.head, Head::BranchDeleted { .. }) {
        tags.push(("[ERROR]", Color::Red));
    }
    match report.landed {
        Landed::IsDefault { .. } => tags.push(("[DEFAULT]", Color::Green)),
        Landed::Merged { .. } => tags.push(("[MERGED]", Color::Green)),
        Landed::Equivalent { .. } => tags.push(("[LANDED]", Color::Green)),
        Landed::NotMerged { .. } => tags.push(("[UNLANDED]", Color::Yellow)),
        Landed::Unknown { .. } => tags.push(("[UNKNOWN]", Color::Red)),
    }
    if report.unpushed.at_risk() > 0 {
        tags.push(("[UNPUSHED]", Color::Yellow));
    }
    if !report.dirty.is_clean() {
        tags.push(("[DIRTY]", Color::Yellow));
    }
    tags
}

fn render_commit(frame: &mut Frame<'_>, commit: &Commit, area: Rect) {
    let when = render::commit_time(&commit.committed, true);
    let fixed = format!("     {when}  {}  ", commit.short_oid());
    let budget = area.width as usize;
    let subject = render::truncate_right(
        commit.subject.trim(),
        budget.saturating_sub(render::display_width(&fixed)).max(8),
    );
    let mut spans = vec![
        Span::styled(fixed, normal()),
        Span::styled(subject, normal()),
    ];
    if commit.is_merge {
        spans.push(Span::styled(" ", normal()));
        spans.push(tag("[MERGE]", Color::Green, normal()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(normal()), area);
}

fn render_footer(frame: &mut Frame<'_>, pane: &DigestPane, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(normal())
        .title(Span::styled(
            " details ",
            normal().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let help_y = inner.bottom().saturating_sub(1);
    let detail_height = inner.height.saturating_sub(1);
    let detail = Rect::new(inner.x, inner.y, inner.width, detail_height);
    let mut lines = detail_lines(pane);
    lines.truncate(detail.height as usize);
    frame.render_widget(Paragraph::new(lines).style(normal()), detail);

    let help = if inner.width as usize >= render::display_width(HELP_WIDE) {
        HELP_WIDE
    } else {
        HELP_NARROW
    };
    frame.render_widget(
        Paragraph::new(help).style(normal()),
        Rect::new(inner.x, help_y, inner.width, 1),
    );
}

fn detail_lines(pane: &DigestPane) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(message) = &pane.message {
        let color = if message.error {
            Color::Red
        } else {
            Color::Reset
        };
        let label = if message.error { "[ERROR]" } else { "[STATUS]" };
        lines.push(Line::from(vec![
            tag(label, color, normal()),
            Span::styled(message.text.clone(), normal()),
        ]));
    }
    if let Some(checkout) = pane.checkout(pane.cursor) {
        let report = &checkout.report;
        lines.push(Line::from(format!(
            "{} · {}",
            report.path.display(),
            render::landed_sentence(&report.landed, render::REF_COLUMNS)
        )));
        let mut facts = Vec::new();
        if let Some(unpushed) = render::unpushed_sentence(&report.unpushed) {
            facts.push(unpushed);
        }
        if let Some(dirty) = render::dirty_sentence(&report.dirty) {
            facts.push(dirty);
        }
        if !facts.is_empty() {
            lines.push(Line::from(facts.join(" · ")));
        }
        for problem in &report.problems {
            lines.push(Line::from(vec![
                tag("[ERROR]", Color::Red, normal()),
                Span::styled(problem.clone(), normal()),
            ]));
        }
    }
    for note in &pane.digest.notes {
        if lines.len() >= 4 {
            break;
        }
        lines.push(Line::from(note.message.clone()));
    }
    lines
}
