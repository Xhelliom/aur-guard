//! Interface de paramétrage en terminal (ratatui).
//! Édite les réglages principaux + la whitelist, et les enregistre.

use crate::config::{Config, Provider};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::io;

const FIELDS: usize = 7;

#[derive(PartialEq)]
enum Screen {
    Main,
    Whitelist,
}

struct App {
    cfg: Config,
    sel: usize,
    screen: Screen,
    wl_sel: usize,
    input: Option<String>,
    status: String,
    dirty: bool,
}

impl App {
    fn new(cfg: Config) -> Self {
        App {
            cfg,
            sel: 0,
            screen: Screen::Main,
            wl_sel: 0,
            input: None,
            status:
                "↑/↓ naviguer · ←/→ ou Espace modifier · Entrée: whitelist · s sauver · q quitter"
                    .into(),
            dirty: false,
        }
    }

    fn adjust(&mut self, delta: i64) {
        match self.sel {
            0 => {
                let v = self.cfg.delay_days as i64 + delta;
                self.cfg.delay_days = v.clamp(0, 365) as u64;
            }
            1 => {
                self.cfg.helper = if self.cfg.helper == "yay" {
                    "paru".into()
                } else {
                    "yay".into()
                };
            }
            2 => self.cfg.use_aur_scan = !self.cfg.use_aur_scan,
            3 => self.cfg.ai.enabled = !self.cfg.ai.enabled,
            4 => {
                self.cfg.ai.provider = match (self.cfg.ai.provider, delta >= 0) {
                    (Provider::Groq, true) => Provider::Anthropic,
                    (Provider::Anthropic, true) => Provider::Openai,
                    (Provider::Openai, true) => Provider::Groq,
                    (Provider::Groq, false) => Provider::Openai,
                    (Provider::Anthropic, false) => Provider::Groq,
                    (Provider::Openai, false) => Provider::Anthropic,
                };
            }
            5 => {
                let v = self.cfg.ai.confirm_votes as i64 + delta;
                self.cfg.ai.confirm_votes = v.clamp(1, 9) as u32;
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn rows(&self) -> Vec<(String, String)> {
        vec![
            (
                "Délai de sécurité".into(),
                format!("{} jours", self.cfg.delay_days),
            ),
            ("Helper AUR".into(), self.cfg.helper.clone()),
            (
                "Scan statique (aur-scan)".into(),
                onoff(self.cfg.use_aur_scan),
            ),
            ("Review IA".into(), onoff(self.cfg.ai.enabled)),
            ("Provider IA".into(), format!("{:?}", self.cfg.ai.provider)),
            (
                "Votes de confirmation".into(),
                self.cfg.ai.confirm_votes.to_string(),
            ),
            (
                "Whitelist".into(),
                format!("{} paquets ▸", self.cfg.whitelist.len()),
            ),
        ]
    }
}

fn onoff(b: bool) -> String {
    if b {
        "✅ activé".into()
    } else {
        "⬜ désactivé".into()
    }
}

/// Lance la TUI de configuration.
pub fn run(cfg: Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cfg);
    let res = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.screen {
                Screen::Main => {
                    if main_keys(app, key.code) {
                        break;
                    }
                }
                Screen::Whitelist => whitelist_keys(app, key.code),
            }
        }
    }
    Ok(())
}

/// Renvoie true s'il faut quitter.
fn main_keys(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.dirty {
                app.status =
                    "Modifs non sauvées — 's' pour sauver, 'Q' pour quitter sans sauver".into();
            } else {
                return true;
            }
        }
        KeyCode::Char('Q') => return true,
        KeyCode::Up => app.sel = (app.sel + FIELDS - 1) % FIELDS,
        KeyCode::Down | KeyCode::Tab => app.sel = (app.sel + 1) % FIELDS,
        KeyCode::Left => app.adjust(-1),
        KeyCode::Right | KeyCode::Char(' ') => app.adjust(1),
        KeyCode::Enter if app.sel == 6 => {
            app.screen = Screen::Whitelist;
            app.wl_sel = 0;
            app.status = "↑/↓ naviguer · a ajouter · d supprimer · Échap retour".into();
        }
        KeyCode::Char('s') => match app.cfg.save() {
            Ok(_) => {
                app.dirty = false;
                app.status = "✔ Configuration enregistrée".into();
            }
            Err(e) => app.status = format!("Erreur sauvegarde : {e}"),
        },
        _ => {}
    }
    false
}

fn whitelist_keys(app: &mut App, code: KeyCode) {
    // Mode saisie d'un nouveau paquet.
    if let Some(buf) = app.input.as_mut() {
        match code {
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Enter => {
                let name = buf.trim().to_string();
                if !name.is_empty() && !app.cfg.whitelist.contains(&name) {
                    app.cfg.whitelist.push(name);
                    app.cfg.whitelist.sort();
                    app.dirty = true;
                }
                app.input = None;
                app.status = "Ajouté. a ajouter · d supprimer · Échap retour".into();
            }
            KeyCode::Esc => {
                app.input = None;
                app.status = "Saisie annulée".into();
            }
            _ => {}
        }
        return;
    }

    let len = app.cfg.whitelist.len();
    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.screen = Screen::Main;
            app.status = "↑/↓ · ←/→ modifier · Entrée whitelist · s sauver · q quitter".into();
        }
        KeyCode::Up if len > 0 => app.wl_sel = (app.wl_sel + len - 1) % len,
        KeyCode::Down if len > 0 => app.wl_sel = (app.wl_sel + 1) % len,
        KeyCode::Char('a') => {
            app.input = Some(String::new());
            app.status = "Nom du paquet puis Entrée (Échap pour annuler)".into();
        }
        KeyCode::Char('d') if len > 0 => {
            let removed = app.cfg.whitelist.remove(app.wl_sel);
            if app.wl_sel >= app.cfg.whitelist.len() && app.wl_sel > 0 {
                app.wl_sel -= 1;
            }
            app.dirty = true;
            app.status = format!("Supprimé : {removed}");
        }
        KeyCode::Char('s') => match app.cfg.save() {
            Ok(_) => {
                app.dirty = false;
                app.status = "✔ Configuration enregistrée".into();
            }
            Err(e) => app.status = format!("Erreur sauvegarde : {e}"),
        },
        _ => {}
    }
}

fn ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    let header = match app.screen {
        Screen::Main => "aur-guard — configuration",
        Screen::Whitelist => "aur-guard — whitelist",
    };
    let title = Paragraph::new(header)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    match app.screen {
        Screen::Main => render_main(f, app, chunks[1]),
        Screen::Whitelist => render_whitelist(f, app, chunks[1]),
    }

    let status = Paragraph::new(app.status.clone()).block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);
}

fn render_main(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .rows()
        .into_iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let marker = if i == app.sel { "▶ " } else { "  " };
            let style = if i == app.sel {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(format!("{label:<28}"), style),
                Span::raw("  "),
                Span::styled(value, Style::default().fg(Color::Yellow)),
            ]))
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" réglages "));
    f.render_widget(list, area);
}

fn render_whitelist(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let mut items: Vec<ListItem> = app
        .cfg
        .whitelist
        .iter()
        .enumerate()
        .map(|(i, pkg)| {
            let marker = if i == app.wl_sel && app.input.is_none() {
                "▶ "
            } else {
                "  "
            };
            let style = if i == app.wl_sel && app.input.is_none() {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(pkg.clone(), style),
            ]))
        })
        .collect();

    if let Some(buf) = &app.input {
        items.push(ListItem::new(Line::from(vec![
            Span::styled("+ ", Style::default().fg(Color::Green)),
            Span::styled(format!("{buf}_"), Style::default().fg(Color::Green)),
        ])));
    }

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" whitelist ({} paquets) ", app.cfg.whitelist.len())),
    );
    f.render_widget(list, area);
}
