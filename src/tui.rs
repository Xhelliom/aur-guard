//! Interface de paramétrage en terminal (ratatui).
//! Édite les réglages principaux + la whitelist (avec suggestions), et les
//! enregistre. Les clés API vont dans le fichier de secrets, pas dans la config.

use crate::aur;
use crate::config::{Config, DelayMode, Provider, Secrets};
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

// Index des champs de l'écran principal.
const F_DELAY: usize = 0;
const F_MODE: usize = 1;
const F_HELPER: usize = 2;
const F_SCAN: usize = 3;
const F_AI: usize = 4;
const F_PROVIDER: usize = 5;
const F_MODEL: usize = 6;
const F_APIKEY: usize = 7;
const F_VOTES: usize = 8;
const F_WHITELIST: usize = 9;
const FIELDS: usize = 10;

const DELAY_MAX: u64 = 365;
const VOTES_MIN: u32 = 1;
const VOTES_MAX: u32 = 9;

#[derive(PartialEq)]
enum Screen {
    Main,
    Whitelist,
}

struct App {
    cfg: Config,
    installed: Vec<String>,
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
            installed: aur::installed_aur_packages(),
            sel: 0,
            screen: Screen::Main,
            wl_sel: 0,
            input: None,
            status: "↑/↓ naviguer · ←/→ modifier · Entrée éditer/whitelist · s sauver · q quitter"
                .into(),
            dirty: false,
        }
    }

    /// Suggestions = paquets AUR installés absents de la whitelist.
    fn suggestions(&self) -> Vec<String> {
        self.installed
            .iter()
            .filter(|p| !self.cfg.is_whitelisted(p))
            .cloned()
            .collect()
    }

    fn adjust(&mut self, delta: i64) {
        match self.sel {
            F_DELAY => {
                let v = self.cfg.delay_days as i64 + delta;
                self.cfg.delay_days = v.clamp(0, DELAY_MAX as i64) as u64;
            }
            F_MODE => {
                self.cfg.delay_mode = match self.cfg.delay_mode {
                    DelayMode::Lag => DelayMode::Hold,
                    DelayMode::Hold => DelayMode::Lag,
                };
            }
            F_HELPER => {
                self.cfg.helper = if self.cfg.helper == "yay" {
                    "paru".into()
                } else {
                    "yay".into()
                };
            }
            F_SCAN => self.cfg.use_aur_scan = !self.cfg.use_aur_scan,
            F_AI => self.cfg.ai.enabled = !self.cfg.ai.enabled,
            F_PROVIDER => {
                self.cfg.ai.provider = cycle_provider(self.cfg.ai.provider, delta >= 0);
            }
            F_VOTES => {
                let v = self.cfg.ai.confirm_votes as i64 + delta;
                self.cfg.ai.confirm_votes = v.clamp(VOTES_MIN as i64, VOTES_MAX as i64) as u32;
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
            ("Mode du délai".into(), format!("{:?}", self.cfg.delay_mode)),
            ("Helper AUR".into(), self.cfg.helper.clone()),
            (
                "Scan statique (aur-scan)".into(),
                onoff(self.cfg.use_aur_scan),
            ),
            ("Review IA".into(), onoff(self.cfg.ai.enabled)),
            ("Provider IA".into(), format!("{:?}", self.cfg.ai.provider)),
            ("Modèle".into(), self.model_display()),
            ("Clé API".into(), self.apikey_display()),
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

    fn model_display(&self) -> String {
        if self.cfg.ai.model.is_empty() {
            format!("(défaut : {})", self.cfg.ai.provider.default_model())
        } else {
            self.cfg.ai.model.clone()
        }
    }

    fn apikey_display(&self) -> String {
        let p = self.cfg.ai.provider;
        if std::env::var(p.default_key_env())
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            format!("définie (${})", p.default_key_env())
        } else if Secrets::load().get(p).is_some() {
            "enregistrée".into()
        } else {
            "non définie".into()
        }
    }
}

fn cycle_provider(p: Provider, forward: bool) -> Provider {
    match (p, forward) {
        (Provider::Groq, true) => Provider::Anthropic,
        (Provider::Anthropic, true) => Provider::Openai,
        (Provider::Openai, true) => Provider::Groq,
        (Provider::Groq, false) => Provider::Openai,
        (Provider::Anthropic, false) => Provider::Groq,
        (Provider::Openai, false) => Provider::Anthropic,
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
            let quit = match app.screen {
                Screen::Main => main_keys(app, key.code),
                Screen::Whitelist => {
                    whitelist_keys(app, key.code);
                    false
                }
            };
            if quit {
                break;
            }
        }
    }
    Ok(())
}

/// Touches de l'écran principal. Renvoie true s'il faut quitter.
fn main_keys(app: &mut App, code: KeyCode) -> bool {
    // Mode saisie d'un champ texte (modèle / clé API).
    if let Some(buf) = app.input.as_mut() {
        match code {
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Enter => commit_text_field(app),
            KeyCode::Esc => {
                app.input = None;
                app.status = "Saisie annulée".into();
            }
            _ => {}
        }
        return false;
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.dirty {
                app.status = "Modifs non sauvées — 's' pour sauver, 'Q' pour quitter sans".into();
            } else {
                return true;
            }
        }
        KeyCode::Char('Q') => return true,
        KeyCode::Up => app.sel = (app.sel + FIELDS - 1) % FIELDS,
        KeyCode::Down | KeyCode::Tab => app.sel = (app.sel + 1) % FIELDS,
        KeyCode::Left => app.adjust(-1),
        KeyCode::Right | KeyCode::Char(' ') => app.adjust(1),
        KeyCode::Enter => match app.sel {
            F_WHITELIST => {
                app.screen = Screen::Whitelist;
                app.wl_sel = 0;
                app.status =
                    "↑/↓ · a ajouter · d retirer · Entrée ajoute une suggestion · Échap retour"
                        .into();
            }
            F_MODEL => {
                app.input = Some(app.cfg.ai.model.clone());
                app.status = "Nom du modèle puis Entrée (Échap annule)".into();
            }
            F_APIKEY => {
                app.input = Some(String::new());
                app.status = format!(
                    "Clé API {} puis Entrée (Échap annule)",
                    app.cfg.ai.provider.default_key_env()
                );
            }
            _ => {}
        },
        KeyCode::Char('s') => save(app),
        _ => {}
    }
    false
}

/// Valide le champ texte en cours d'édition (modèle ou clé API).
fn commit_text_field(app: &mut App) {
    let Some(buf) = app.input.take() else {
        return;
    };
    match app.sel {
        F_MODEL => {
            app.cfg.ai.model = buf.trim().to_string();
            app.dirty = true;
            app.status = "Modèle mis à jour".into();
        }
        F_APIKEY => {
            if buf.trim().is_empty() {
                app.status = "Clé vide ignorée".into();
                return;
            }
            let mut secrets = Secrets::load();
            secrets.set(app.cfg.ai.provider, Some(buf));
            app.status = match secrets.save() {
                Ok(_) => "✔ Clé API enregistrée (secrets.toml, 0600)".into(),
                Err(e) => format!("Erreur secrets : {e}"),
            };
        }
        _ => {}
    }
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
                add_whitelist(app, &name);
                app.input = None;
                app.status =
                    "a ajouter · d retirer · Entrée ajoute une suggestion · Échap retour".into();
            }
            KeyCode::Esc => {
                app.input = None;
                app.status = "Saisie annulée".into();
            }
            _ => {}
        }
        return;
    }

    let wl_len = app.cfg.whitelist.len();
    let total = wl_len + app.suggestions().len();
    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.screen = Screen::Main;
            app.status = "↑/↓ · ←/→ modifier · Entrée éditer · s sauver · q quitter".into();
        }
        KeyCode::Up if total > 0 => app.wl_sel = (app.wl_sel + total - 1) % total,
        KeyCode::Down if total > 0 => app.wl_sel = (app.wl_sel + 1) % total,
        KeyCode::Char('a') => {
            app.input = Some(String::new());
            app.status = "Nom du paquet puis Entrée (Échap pour annuler)".into();
        }
        KeyCode::Char('d') if app.wl_sel < wl_len => {
            let removed = app.cfg.whitelist.remove(app.wl_sel);
            app.dirty = true;
            if app.wl_sel >= app.cfg.whitelist.len() && app.wl_sel > 0 {
                app.wl_sel -= 1;
            }
            app.status = format!("Retiré : {removed}");
        }
        // Entrée sur une suggestion : on l'ajoute à la whitelist.
        KeyCode::Enter if app.wl_sel >= wl_len => {
            let sugg = app.suggestions();
            if let Some(name) = sugg.get(app.wl_sel - wl_len).cloned() {
                add_whitelist(app, &name);
                app.status = format!("Ajouté : {name}");
            }
        }
        KeyCode::Char('s') => save(app),
        _ => {}
    }
}

fn add_whitelist(app: &mut App, name: &str) {
    if !name.is_empty() && !app.cfg.is_whitelisted(name) {
        app.cfg.whitelist.push(name.to_string());
        app.cfg.whitelist.sort();
        app.dirty = true;
    }
}

fn save(app: &mut App) {
    app.status = match app.cfg.save() {
        Ok(_) => {
            app.dirty = false;
            "✔ Configuration enregistrée".into()
        }
        Err(e) => format!("Erreur sauvegarde : {e}"),
    };
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
            let selected = i == app.sel;
            let editing = selected && app.input.is_some();
            let shown = if editing {
                edit_buffer_display(app, i)
            } else {
                value
            };
            let marker = if selected { "▶ " } else { "  " };
            let label_style = if selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            let value_style = if editing {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(format!("{label:<28}"), label_style),
                Span::raw("  "),
                Span::styled(shown, value_style),
            ]))
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" réglages "));
    f.render_widget(list, area);
}

/// Affichage du buffer en cours d'édition (clé API masquée).
fn edit_buffer_display(app: &App, field: usize) -> String {
    let buf = app.input.as_deref().unwrap_or("");
    if field == F_APIKEY {
        format!("{}_", "•".repeat(buf.chars().count()))
    } else {
        format!("{buf}_")
    }
}

fn render_whitelist(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let wl_len = app.cfg.whitelist.len();
    let suggestions = app.suggestions();
    let mut items: Vec<ListItem> = Vec::new();

    for (i, pkg) in app.cfg.whitelist.iter().enumerate() {
        items.push(wl_line(
            pkg,
            i == app.wl_sel && app.input.is_none(),
            Color::Cyan,
            "",
        ));
    }
    if !suggestions.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  — suggestions (paquets AUR installés) —",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    for (i, pkg) in suggestions.iter().enumerate() {
        let selected = app.wl_sel == wl_len + i && app.input.is_none();
        items.push(wl_line(pkg, selected, Color::Green, "+ "));
    }
    if let Some(buf) = &app.input {
        items.push(ListItem::new(Line::from(vec![
            Span::styled("+ ", Style::default().fg(Color::Green)),
            Span::styled(format!("{buf}_"), Style::default().fg(Color::Green)),
        ])));
    }

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
        " whitelist ({wl_len}) · suggestions ({}) ",
        suggestions.len()
    )));
    f.render_widget(list, area);
}

fn wl_line(pkg: &str, selected: bool, accent: Color, prefix: &str) -> ListItem<'static> {
    let marker = if selected { "▶ " } else { "  " };
    let style = if selected {
        Style::default().fg(Color::Black).bg(accent)
    } else {
        Style::default()
    };
    ListItem::new(Line::from(vec![
        Span::raw(marker),
        Span::styled(format!("{prefix}{pkg}"), style),
    ]))
}
