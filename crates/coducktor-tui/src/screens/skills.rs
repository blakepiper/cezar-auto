//! The Skills screen (spec §8.11) — replaces `routes/skills.tsx` +
//! `components/skill-detail.tsx`. A master/detail browser over locally discovered skills:
//! the list on the left with a source badge (project / global / team), the rendered body on
//! the right, `/` to filter. Reads `GET /skills` only — no import panel, no update banner,
//! no refresh action (decision 7 deleted the two network paths those served). Skills are
//! files the user manages with their editor; this screen reads them.

use coducktor_contract::{Skill, SkillSource};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, PendingAction, Route};
use crate::markdown::RenderCache;

/// Engine-fetched state for the open Skills screen.
pub struct SkillsUi {
    pub project: String,
    pub skills: Vec<Skill>,
    pub selected: usize,
    pub query: String,
    pub filter_open: bool,
    pub markdown: RenderCache,
}

impl Default for SkillsUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            skills: Vec::new(),
            selected: 0,
            query: String::new(),
            filter_open: false,
            markdown: RenderCache::new(),
        }
    }
}

/// The source badge text — "project" for every project-local source, "global" for the
/// global layer, "team" for a team repo (mirrors the web's `skill-detail.tsx` badge).
pub fn source_badge(source: SkillSource) -> &'static str {
    match source {
        SkillSource::Ai | SkillSource::Legacy | SkillSource::Agents => "project",
        SkillSource::Global => "global",
        SkillSource::Team => "team",
    }
}

pub fn open(app: &mut App, project: &str) {
    if app.skills_ui.project != project {
        app.skills_ui = SkillsUi {
            project: project.to_owned(),
            ..SkillsUi::default()
        };
    }
    app.request_navigate(Route::Skills {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadSkills {
        project: project.to_owned(),
    });
}

fn visible(app: &App) -> Vec<usize> {
    let query = app.skills_ui.query.to_lowercase();
    app.skills_ui
        .skills
        .iter()
        .enumerate()
        .filter_map(|(index, skill)| {
            if query.is_empty()
                || skill.name.to_lowercase().contains(&query)
                || skill
                    .description
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
            {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    // The filter line — `/` opens it, matching the tasks tables' filter-mode convention.
    let filter_label = if app.skills_ui.filter_open {
        format!("/ {}", app.skills_ui.query)
    } else {
        "/ to filter skills".to_owned()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            filter_label,
            Style::default().fg(app.theme.palette.soft_fg),
        ))),
        rows[0],
    );

    if app.skills_ui.skills.is_empty() {
        frame.render_widget(
            Paragraph::new("No skills found.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            rows[1],
        );
        return;
    }
    let visible_indices = visible(app);
    let list_width = (rows[1].width / 2).clamp(30, 44);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(list_width), Constraint::Min(1)])
        .split(rows[1]);
    render_list(frame, cols[0], app, &visible_indices);
    render_detail(frame, cols[1], app, &visible_indices);
}

fn render_list(frame: &mut Frame<'_>, area: Rect, app: &mut App, visible_indices: &[usize]) {
    let block = Block::default().borders(Borders::ALL).title("Skills");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let selected_position = visible_indices
        .iter()
        .position(|index| *index == app.skills_ui.selected)
        .unwrap_or(0);
    let lines: Vec<Line<'static>> = visible_indices
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(position, index)| {
            let skill = &app.skills_ui.skills[*index];
            let mut style = Style::default().fg(app.theme.palette.fg);
            if position == selected_position {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(vec![
                Span::styled(format!("{}  ", skill.name), style),
                Span::styled(
                    format!("[{}]", source_badge(skill.source)),
                    Style::default().fg(app.theme.palette.soft_fg),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (position, index) in visible_indices
        .iter()
        .enumerate()
        .take(inner.height as usize)
    {
        if let Some(y) = inner.y.checked_add(position as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                crate::input::hitmap::HitAction::SkillsScreen(*index),
            );
        }
    }
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, app: &mut App, visible_indices: &[usize]) {
    let Some(skill) = visible_indices
        .iter()
        .find(|index| **index == app.skills_ui.selected)
        .and_then(|index| app.skills_ui.skills.get(*index))
        .or_else(|| {
            visible_indices
                .first()
                .and_then(|index| app.skills_ui.skills.get(*index))
        })
    else {
        frame.render_widget(
            Paragraph::new("Nothing matches the filter.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    let block = Block::default().borders(Borders::ALL).title(format!(
        "{}  [{}]",
        skill.name,
        source_badge(skill.source)
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Description pinned above the body — two rows when present, body fills the rest.
    let has_description = skill
        .description
        .as_deref()
        .is_some_and(|description| !description.is_empty());
    let (description_area, body_area) = if has_description {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(inner);
        (Some(rows[0]), rows[1])
    } else {
        (None, inner)
    };
    if let Some(description_area) = description_area {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                skill.description.clone().unwrap_or_default(),
                Style::default().fg(app.theme.palette.accent),
            ))),
            description_area,
        );
    }
    let body = Paragraph::new(app.skills_ui.markdown.text(&skill.body).clone());
    frame.render_widget(body, body_area);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.skills_ui.filter_open {
        match key.code {
            KeyCode::Esc => {
                app.skills_ui.filter_open = false;
                app.skills_ui.query.clear();
            }
            KeyCode::Backspace => {
                app.skills_ui.query.pop();
            }
            KeyCode::Enter => app.skills_ui.filter_open = false,
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.skills_ui.query.push(character);
            }
            _ => {}
        }
        return true;
    }
    match key.code {
        KeyCode::Char('/') => {
            app.skills_ui.filter_open = true;
            app.skills_ui.query.clear();
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let visible_indices = visible(app);
            if !visible_indices.is_empty() {
                let position = visible_indices
                    .iter()
                    .position(|index| *index == app.skills_ui.selected)
                    .unwrap_or(0);
                let next = (position + 1).min(visible_indices.len() - 1);
                app.skills_ui.selected = visible_indices[next];
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let visible_indices = visible(app);
            if !visible_indices.is_empty() {
                let position = visible_indices
                    .iter()
                    .position(|index| *index == app.skills_ui.selected)
                    .unwrap_or(0);
                let next = position.saturating_sub(1);
                app.skills_ui.selected = visible_indices[next];
            }
            true
        }
        KeyCode::Esc => {
            app.request_back();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app_with_skills() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.skills_ui.skills = vec![
            Skill {
                name: "om-fix".to_owned(),
                description: Some("Fix the thing".to_owned()),
                interactive: None,
                body: "# Fix\n\nRun the fix protocol.".to_owned(),
                path: ".ai/skills/om-fix.md".to_owned(),
                source: SkillSource::Agents,
                team: None,
            },
            Skill {
                name: "omarchy".to_owned(),
                description: Some("Desktop config".to_owned()),
                interactive: None,
                body: "Body two.".to_owned(),
                path: "~/.agents/skills/omarchy.md".to_owned(),
                source: SkillSource::Global,
                team: None,
            },
        ];
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_names_source_badges_and_the_markdown_body() {
        let mut app = app_with_skills();
        let content = render(&mut app, 120, 40);
        assert!(content.contains("om-fix"));
        assert!(content.contains("omarchy"));
        assert!(content.contains("[project]"));
        assert!(content.contains("[global]"));
        assert!(content.contains("Fix the thing"));
        assert!(content.contains("/ to filter skills"));
    }

    #[test]
    fn filtering_restricts_the_list() {
        let mut app = app_with_skills();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        for character in "omarchy".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        let content = render(&mut app, 120, 40);
        assert!(content.contains("omarchy"));
        assert!(!content.contains("om-fix"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.skills_ui.filter_open);
    }

    #[test]
    fn source_badge_maps_every_source() {
        assert_eq!(source_badge(SkillSource::Ai), "project");
        assert_eq!(source_badge(SkillSource::Legacy), "project");
        assert_eq!(source_badge(SkillSource::Agents), "project");
        assert_eq!(source_badge(SkillSource::Global), "global");
        assert_eq!(source_badge(SkillSource::Team), "team");
    }

    #[test]
    fn snapshot_skills_at_three_sizes() {
        let mut app = app_with_skills();
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("skills_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
