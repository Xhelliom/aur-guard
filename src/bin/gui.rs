//! Interface graphique GTK4 / libadwaita pour aur-guard.
//!
//! Vue principale : les mises à jour AUR (vérification + verdicts + apply).
//! Les réglages vivent dans un dialogue séparé (bouton engrenage).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{glib, Adjustment, Orientation, StringList};
use libadwaita as adw;
use libadwaita::prelude::*;

use aur_guard::aur;
use aur_guard::config::{Config, DelayMode, Provider, Secrets};
use aur_guard::pipeline::{self, Decision, Outcome};

const APP_ID: &str = "fr.xhelliom.AurGuard";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn provider_index(p: Provider) -> u32 {
    match p {
        Provider::Groq => 0,
        Provider::Anthropic => 1,
        Provider::Openai => 2,
    }
}

fn provider_from_index(i: u32) -> Provider {
    match i {
        1 => Provider::Anthropic,
        2 => Provider::Openai,
        _ => Provider::Groq,
    }
}

fn provider_name(p: Provider) -> &'static str {
    match p {
        Provider::Groq => "Groq",
        Provider::Anthropic => "Anthropic",
        Provider::Openai => "OpenAI",
    }
}

// =====================================================================
// Fenêtre principale : MISES À JOUR
// =====================================================================

fn build_ui(app: &adw::Application) {
    let cfg = Rc::new(RefCell::new(Config::load_or_init().unwrap_or_default()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("aur-guard")
        .default_width(560)
        .default_height(720)
        .build();

    let header = adw::HeaderBar::new();
    let settings_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Paramètres")
        .build();
    header.pack_end(&settings_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);

    let page = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    let updates = adw::PreferencesGroup::builder()
        .title("Mises à jour AUR")
        .description("Vérifie les paquets selon la chaîne de décision configurée")
        .build();

    let check_btn = gtk::Button::builder()
        .label("Vérifier les mises à jour")
        .css_classes(["suggested-action", "pill"])
        .build();
    let apply_btn = gtk::Button::builder()
        .label("Installer les paquets sûrs")
        .css_classes(["pill"])
        .sensitive(false)
        .build();
    let btn_box = gtk::Box::new(Orientation::Horizontal, 8);
    btn_box.append(&check_btn);
    btn_box.append(&apply_btn);
    updates.set_header_suffix(Some(&btn_box));

    let results = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    results.append(&info_row(
        "Clique sur « Vérifier les mises à jour » pour lancer l'analyse.",
    ));
    updates.add(&results);

    page.append(&updates);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build();
    toolbar.set_content(Some(&scroller));

    // Pile de navigation : « mises à jour » à la racine ; les réglages sont
    // poussés comme une page plein écran (et non un dialogue flottant).
    let updates_page = adw::NavigationPage::new(&toolbar, "aur-guard");
    let nav = adw::NavigationView::new();
    nav.add(&updates_page);

    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&nav));
    window.set_content(Some(&overlay));

    // Bouton engrenage -> page de paramètres plein écran.
    {
        let cfg = cfg.clone();
        let nav = nav.clone();
        let overlay = overlay.clone();
        settings_btn.connect_clicked(move |_| {
            nav.push(&build_settings_page(&cfg, &overlay));
        });
    }

    wire_check(&cfg, &check_btn, &apply_btn, &results);
    wire_apply(&apply_btn, &overlay);

    window.present();
}

/// Branche le bouton « Vérifier » : évaluation en arrière-plan puis affichage.
fn wire_check(
    cfg: &Rc<RefCell<Config>>,
    check_btn: &gtk::Button,
    apply_btn: &gtk::Button,
    results: &gtk::ListBox,
) {
    let cfg = cfg.clone();
    let results = results.clone();
    let check_btn_outer = check_btn.clone();
    let apply_btn = apply_btn.clone();
    check_btn.connect_clicked(move |_| {
        check_btn_outer.set_sensitive(false);
        check_btn_outer.set_label("Vérification…");
        clear_listbox(&results);

        let snapshot = cfg.borrow().clone();
        let (tx, rx) = async_channel::bounded::<Result<Vec<Outcome>, String>>(1);
        std::thread::spawn(move || {
            let res = pipeline::evaluate(&snapshot).map_err(|e| e.to_string());
            let _ = tx.send_blocking(res);
        });

        let results = results.clone();
        let check_btn_inner = check_btn_outer.clone();
        let apply_btn = apply_btn.clone();
        glib::spawn_future_local(async move {
            if let Ok(res) = rx.recv().await {
                match res {
                    Ok(outcomes) => {
                        let mut safe = 0;
                        if outcomes.is_empty() {
                            results.append(&info_row("Aucune mise à jour AUR disponible."));
                        }
                        for o in &outcomes {
                            if matches!(o.decision, Decision::Allow) {
                                safe += 1;
                            }
                            results.append(&outcome_row(o));
                        }
                        apply_btn.set_sensitive(safe > 0);
                    }
                    Err(e) => results.append(&info_row(&format!("Erreur : {e}"))),
                }
            }
            check_btn_inner.set_sensitive(true);
            check_btn_inner.set_label("Vérifier les mises à jour");
        });
    });
}

/// Branche le bouton « Installer » : lance `aur-guard apply` dans un terminal.
/// La CLI gère toute la logique (lag = makepkg, sinon helper -S) et le sudo.
fn wire_apply(apply_btn: &gtk::Button, overlay: &adw::ToastOverlay) {
    let overlay = overlay.clone();
    apply_btn.connect_clicked(move |_| {
        let _ = launch_in_terminal("aur-guard apply");
        overlay.add_toast(adw::Toast::new("Installation lancée dans un terminal"));
    });
}

// =====================================================================
// Dialogue de PARAMÈTRES
// =====================================================================

fn build_settings_page(
    cfg: &Rc<RefCell<Config>>,
    overlay: &adw::ToastOverlay,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();

    // --- Groupe Général ---
    let general = adw::PreferencesGroup::builder()
        .title("Délai & helper")
        .build();
    let delay_row = adw::SpinRow::builder()
        .title("Délai de sécurité (jours)")
        .adjustment(&Adjustment::new(
            cfg.borrow().delay_days as f64,
            0.0,
            365.0,
            1.0,
            7.0,
            0.0,
        ))
        .build();
    let mode_row = adw::ComboRow::builder()
        .title("Mode du délai")
        .subtitle("Lag : révision d'il y a N jours · Hold : bloque les maj récentes")
        .model(&StringList::new(&["Lag (différé)", "Hold (blocage)"]))
        .selected(u32::from(cfg.borrow().delay_mode == DelayMode::Hold))
        .build();
    let helper_row = adw::ComboRow::builder()
        .title("Helper AUR")
        .model(&StringList::new(&["yay", "paru"]))
        .selected(if cfg.borrow().helper == "paru" { 1 } else { 0 })
        .build();
    let scan_row = adw::SwitchRow::builder()
        .title("Scan statique (aur-scan)")
        .subtitle("Délègue à aur-scan s'il est installé")
        .active(cfg.borrow().use_aur_scan)
        .build();
    general.add(&delay_row);
    general.add(&mode_row);
    general.add(&helper_row);
    general.add(&scan_row);

    // --- Groupe Review IA ---
    let ai = adw::PreferencesGroup::builder().title("Review IA").build();
    let ai_row = adw::SwitchRow::builder()
        .title("Activer la review IA")
        .active(cfg.borrow().ai.enabled)
        .build();
    let provider_row = adw::ComboRow::builder()
        .title("Provider")
        .model(&StringList::new(&["Groq", "Anthropic", "OpenAI"]))
        .selected(provider_index(cfg.borrow().ai.provider))
        .build();
    let model_row = adw::EntryRow::builder()
        .title("Modèle (vide = défaut du provider)")
        .text(cfg.borrow().ai.model.as_str())
        .build();
    let key_row = adw::PasswordEntryRow::builder().build();
    let votes_row = adw::SpinRow::builder()
        .title("Votes de confirmation")
        .subtitle("Déclenchés seulement pour confirmer un blocage")
        .adjustment(&Adjustment::new(
            cfg.borrow().ai.confirm_votes as f64,
            1.0,
            9.0,
            1.0,
            1.0,
            0.0,
        ))
        .build();
    refresh_key_row(&key_row, provider_from_index(provider_row.selected()));
    {
        // Met à jour le libellé de la clé quand le provider change.
        let key_row = key_row.clone();
        provider_row.connect_selected_notify(move |row| {
            refresh_key_row(&key_row, provider_from_index(row.selected()));
        });
    }
    ai.add(&ai_row);
    ai.add(&provider_row);
    ai.add(&model_row);
    ai.add(&key_row);
    ai.add(&votes_row);

    // --- Groupe Whitelist ---
    let wl = build_whitelist_group(cfg);

    page.add(&general);
    page.add(&ai);
    page.add(&wl);

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let nav_page = adw::NavigationPage::new(&toolbar, "Paramètres");

    // Sauvegarde quand on quitte la page de réglages (retour à l'accueil).
    {
        let cfg = cfg.clone();
        let overlay = overlay.clone();
        let provider_row = provider_row.clone();
        nav_page.connect_hidden(move |_| {
            let provider = provider_from_index(provider_row.selected());
            {
                let mut c = cfg.borrow_mut();
                c.delay_days = delay_row.value() as u64;
                c.delay_mode = if mode_row.selected() == 1 {
                    DelayMode::Hold
                } else {
                    DelayMode::Lag
                };
                c.helper = if helper_row.selected() == 1 {
                    "paru".into()
                } else {
                    "yay".into()
                };
                c.use_aur_scan = scan_row.is_active();
                c.ai.enabled = ai_row.is_active();
                c.ai.provider = provider;
                c.ai.model = model_row.text().trim().to_string();
                c.ai.confirm_votes = votes_row.value() as u32;
            }

            // La clé saisie (si non vide) va dans le fichier de secrets 0600.
            let typed = key_row.text().to_string();
            if !typed.trim().is_empty() {
                let mut secrets = Secrets::load();
                secrets.set(provider, Some(typed));
                if let Err(e) = secrets.save() {
                    overlay.add_toast(adw::Toast::new(&format!("Erreur secrets : {e}")));
                }
            }

            let toast = match cfg.borrow().save() {
                Ok(_) => adw::Toast::new("Paramètres enregistrés"),
                Err(e) => adw::Toast::new(&format!("Erreur : {e}")),
            };
            toast.set_timeout(2);
            overlay.add_toast(toast);
        });
    }

    nav_page
}

/// Met le libellé de la ligne de clé API à jour selon le provider, en indiquant
/// si une clé est déjà disponible (env ou secrets). Ne pré-remplit jamais la clé.
fn refresh_key_row(key_row: &adw::PasswordEntryRow, provider: Provider) {
    let env_set = std::env::var(provider.default_key_env())
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let file_set = Secrets::load().get(provider).is_some();
    let state = if env_set {
        " — définie via $ENV"
    } else if file_set {
        " — déjà enregistrée"
    } else {
        " — non définie"
    };
    key_row.set_title(&format!("Clé API {}{}", provider_name(provider), state));
}

/// Groupe d'édition de la whitelist : paquets actuels (suppression) + champ
/// d'ajout + suggestions (paquets AUR installés non encore whitelistés).
fn build_whitelist_group(cfg: &Rc<RefCell<Config>>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Whitelist")
        .description("Paquets de confiance : délai ignoré, mais scan + IA conservés")
        .build();

    let wl_expander = adw::ExpanderRow::builder()
        .title("Whitelist")
        .subtitle(format!("{} paquets", cfg.borrow().whitelist.len()))
        .build();
    let wl_add = adw::EntryRow::builder()
        .title("Ajouter un paquet…")
        .show_apply_button(true)
        .build();
    {
        let cfg = cfg.clone();
        let expander = wl_expander.clone();
        wl_add.connect_apply(move |entry| {
            let name = entry.text().trim().to_string();
            if add_to_whitelist(&cfg, &name) {
                expander.add_row(&make_pkg_row(&name, &cfg, &expander));
                update_wl_subtitle(&expander, &cfg);
            }
            entry.set_text("");
        });
    }
    wl_expander.add_row(&wl_add);
    for pkg in cfg.borrow().whitelist.clone() {
        wl_expander.add_row(&make_pkg_row(&pkg, cfg, &wl_expander));
    }
    group.add(&wl_expander);

    // Suggestions : paquets AUR installés absents de la whitelist.
    let suggestions: Vec<String> = aur::installed_aur_packages()
        .into_iter()
        .filter(|p| !cfg.borrow().is_whitelisted(p))
        .collect();
    if !suggestions.is_empty() {
        let sug_expander = adw::ExpanderRow::builder()
            .title("Suggestions")
            .subtitle(format!(
                "{} paquets AUR installés à whitelister",
                suggestions.len()
            ))
            .build();
        for pkg in suggestions {
            sug_expander.add_row(&make_suggestion_row(&pkg, cfg, &wl_expander, &sug_expander));
        }
        group.add(&sug_expander);
    }

    group
}

// =====================================================================
// Helpers de widgets
// =====================================================================

/// Ajoute un paquet à la whitelist si nouveau. Retourne true si ajouté.
fn add_to_whitelist(cfg: &Rc<RefCell<Config>>, name: &str) -> bool {
    if name.is_empty() || cfg.borrow().is_whitelisted(name) {
        return false;
    }
    let mut c = cfg.borrow_mut();
    c.whitelist.push(name.to_string());
    c.whitelist.sort();
    true
}

/// Ligne de paquet whitelisté avec un bouton de suppression.
fn make_pkg_row(
    name: &str,
    cfg: &Rc<RefCell<Config>>,
    expander: &adw::ExpanderRow,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(name).build();
    let btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text("Retirer de la whitelist")
        .build();
    let name_owned = name.to_string();
    let cfg = cfg.clone();
    let expander = expander.clone();
    let row_clone = row.clone();
    btn.connect_clicked(move |_| {
        cfg.borrow_mut().whitelist.retain(|p| p != &name_owned);
        expander.remove(&row_clone);
        update_wl_subtitle(&expander, &cfg);
    });
    row.add_suffix(&btn);
    row
}

/// Ligne de suggestion : un bouton « + » l'ajoute à la whitelist et la déplace.
fn make_suggestion_row(
    name: &str,
    cfg: &Rc<RefCell<Config>>,
    wl_expander: &adw::ExpanderRow,
    sug_expander: &adw::ExpanderRow,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(name).build();
    let btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text("Ajouter à la whitelist")
        .build();
    let name_owned = name.to_string();
    let cfg = cfg.clone();
    let wl_expander = wl_expander.clone();
    let sug_expander = sug_expander.clone();
    let row_clone = row.clone();
    btn.connect_clicked(move |_| {
        if add_to_whitelist(&cfg, &name_owned) {
            wl_expander.add_row(&make_pkg_row(&name_owned, &cfg, &wl_expander));
            update_wl_subtitle(&wl_expander, &cfg);
        }
        sug_expander.remove(&row_clone);
    });
    row.add_suffix(&btn);
    row
}

fn update_wl_subtitle(expander: &adw::ExpanderRow, cfg: &Rc<RefCell<Config>>) {
    expander.set_subtitle(&format!("{} paquets", cfg.borrow().whitelist.len()));
}

fn clear_listbox(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn info_row(text: &str) -> adw::ActionRow {
    adw::ActionRow::builder().title(text).build()
}

fn outcome_row(o: &Outcome) -> adw::ActionRow {
    let (icon, label) = match &o.decision {
        Decision::Allow => {
            let s = if o.whitelisted {
                "Autorisé (whitelist)"
            } else {
                "Autorisé"
            };
            ("emblem-ok-symbolic", s.to_string())
        }
        Decision::Delayed(d) => (
            "appointment-soon-symbolic",
            format!("Retardé — modifié il y a {d}j"),
        ),
        Decision::Blocked(reason) => ("dialog-warning-symbolic", format!("BLOQUÉ — {reason}")),
    };
    let subtitle = match &o.lag {
        Some(t) if !o.update.old_ver.is_empty() => {
            format!("{}  ({} → {} J-N)", label, o.update.old_ver, t.version)
        }
        Some(t) => format!("{}  (→ {} J-N)", label, t.version),
        None => match (o.update.old_ver.is_empty(), o.update.new_ver.is_empty()) {
            (false, false) => format!("{}  ({} → {})", label, o.update.old_ver, o.update.new_ver),
            _ => label,
        },
    };
    let row = adw::ActionRow::builder()
        .title(&o.update.name)
        .subtitle(&subtitle)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row
}

/// Tente de lancer une commande dans un émulateur de terminal courant.
fn launch_in_terminal(cmd: &str) -> std::io::Result<()> {
    let full = format!("{cmd}; echo; read -p 'Entrée pour fermer…'");
    let candidates: [(&str, Vec<&str>); 4] = [
        ("foot", vec!["-e", "bash", "-c", &full]),
        ("kitty", vec!["bash", "-c", &full]),
        ("alacritty", vec!["-e", "bash", "-c", &full]),
        ("xterm", vec!["-e", "bash", "-c", &full]),
    ];
    for (term, args) in candidates {
        if std::process::Command::new(term).args(&args).spawn().is_ok() {
            return Ok(());
        }
    }
    Ok(())
}
