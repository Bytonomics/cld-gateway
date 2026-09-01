#![forbid(unsafe_code)]

use crate::login::openai::LoginSelection;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::EnterAlternateScreen;
use crossterm::{event, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io::{self, Stdout};
use std::time::Duration;

pub fn login_menu() -> Result<LoginSelection, Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut selected = 0usize;
    loop {
        render(&mut terminal, selected)?;

        if event::poll(Duration::from_millis(100))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    cleanup_terminal(&mut terminal)?;
                    return Err("login aborted".into());
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => selected = (selected + 1).min(1),
                KeyCode::Char('1') => {
                    cleanup_terminal(&mut terminal)?;
                    return Ok(LoginSelection::Chatgpt);
                }
                KeyCode::Char('2') => {
                    cleanup_terminal(&mut terminal)?;
                    return Ok(LoginSelection::ApiKey);
                }
                KeyCode::Enter => {
                    let out = if selected == 0 {
                        LoginSelection::Chatgpt
                    } else {
                        LoginSelection::ApiKey
                    };
                    cleanup_terminal(&mut terminal)?;
                    return Ok(out);
                }
                _ => {}
            }
        }
    }
}

fn render(terminal: &mut Terminal<CrosstermBackend<Stdout>>, selected: usize) -> io::Result<()> {
    terminal.draw(|f| {
        let size = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Length(8),
                Constraint::Min(0),
            ])
            .split(size);

        let header = Paragraph::new(Text::from(logo_lines())).alignment(Alignment::Center);
        f.render_widget(header, chunks[0]);

        let block = Block::default().borders(Borders::ALL).title("Login");
        let inner = block.inner(chunks[1]).inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        f.render_widget(block, chunks[1]);

        let title = vec![
            Line::from(Span::styled(
                "Welcome to Gateway",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Anthropic-compatible proxy for Claude Code → OpenAI",
                Style::default().fg(Color::Gray),
            )),
            Line::from(""),
            Line::from("Sign in with ChatGPT to use your paid plan, or provide an API key."),
        ];
        f.render_widget(Paragraph::new(Text::from(title)), inner);

        let menu_area = chunks[2];
        f.render_widget(menu_paragraph(selected), menu_area);
    })?;
    Ok(())
}

fn logo_lines() -> Vec<Line<'static>> {
    // Minimal PCB-style “fan-in → router → fan-out” icon (5 inputs, 3 outputs).
    vec![
        Line::from(""),
        Line::from(Span::styled(
            "●──╮                         ╭──●",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "●──┼──╮                 ╭────┼──●",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "●──┼──┼──────╭──────────┤    │",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "●──┼──╯      │  GATEWAY  ├────┼──●",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "●──╯         ╰──────────╯",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
    ]
}

fn menu_paragraph(selected: usize) -> Paragraph<'static> {
    let items = ["1. Sign in with ChatGPT", "2. Provide your own API key"];
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    for (idx, label) in items.iter().enumerate() {
        let is_selected = idx == selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let prefix = if is_selected { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::styled((*label).to_string(), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Use ↑/↓ to move, Enter to select, q to quit",
        Style::default().fg(Color::Gray),
    )));

    Paragraph::new(Text::from(lines)).alignment(Alignment::Left)
}

fn cleanup_terminal(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::terminal::LeaveAlternateScreen;
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
