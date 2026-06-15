//! Interface graphique GTK4 / libadwaita pour aur-guard.
//! Réglages éditables + rapport des mises à jour AUR (verdicts) + apply.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{glib, Adjustment, Orientation, StringList};
use libadwaita as adw;
use libadwaita::prelude::*;

use aur_guard::config::{Config, DelayMode, Provider};
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

fn build_ui(app: &adw::Application) {
    let cfg = Config::load_or_init().unwrap_or_default();
    let cfg = Rc::new(RefCell::new(cfg));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("aur-guard")
        .default_width(560)
        .default_height(720)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let page = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    // ---------------------------------------------------------------
    // Groupe RÉGLAGES
    // ---------------------------------------------------------------
    let settings = adw::PreferencesGroup::builder()
        .title("Réglages")
        .description("Chaîne de décision : whitelist → délai → scan → review IA")
        .build();

    let delay_row = adw::SpinRow::builder()
        .title("Délai de sécurité (jours)")
        .subtitle("Une maj plus récente est retardée")
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
        .subtitle("Lag : installe la révision d'il y a N jours · Hold : bloque les maj récentes")
        .model(&StringList::new(&["Lag (différé)", "Hold (blocage)"]))
        .selected(if cfg.borrow().delay_mode == DelayMode::Hold {
            1
        } else {
            0
        })
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

    let ai_row = adw::SwitchRow::builder()
        .title("Review IA du diff PKGBUILD")
        .active(cfg.borrow().ai.enabled)
        .build();

    let provider_row = adw::ComboRow::builder()
        .title("Provider IA")
        .model(&StringList::new(&["Groq", "Anthropic", "OpenAI"]))
        .selected(provider_index(cfg.borrow().ai.provider))
        .build();

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

    let wl_expander = adw::ExpanderRow::builder()
        .title("Whitelist")
        .subtitle(format!(
            "{} paquets de confiance",
            cfg.borrow().whitelist.len()
        ))
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
            let is_new = !name.is_empty() && !cfg.borrow().whitelist.contains(&name);
            if is_new {
                {
                    let mut c = cfg.borrow_mut();
                    c.whitelist.push(name.clone());
                    c.whitelist.sort();
                }
                expander.add_row(&make_pkg_row(&name, &cfg, &expander));
                update_wl_subtitle(&expander, &cfg);
            }
            entry.set_text("");
        });
    }
    wl_expander.add_row(&wl_add);
    let initial: Vec<String> = cfg.borrow().whitelist.clone();
    for pkg in initial {
        wl_expander.add_row(&make_pkg_row(&pkg, &cfg, &wl_expander));
    }

    settings.add(&delay_row);
    settings.add(&mode_row);
    settings.add(&helper_row);
    settings.add(&scan_row);
    settings.add(&ai_row);
    settings.add(&provider_row);
    settings.add(&votes_row);
    settings.add(&wl_expander);

    let save_btn = gtk::Button::builder()
        .label("Enregistrer la configuration")
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::End)
        .build();

    // ---------------------------------------------------------------
    // Groupe MISES À JOUR
    // ---------------------------------------------------------------
    let updates = adw::PreferencesGroup::builder()
        .title("Mises à jour AUR")
        .build();

    let check_btn = gtk::Button::builder()
        .label("Vérifier les mises à jour")
        .css_classes(["pill"])
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
    results.set_visible(false);
    updates.add(&results);

    page.append(&settings);
    page.append(&save_btn);
    page.append(&updates);
    scroller.set_child(Some(&page));

    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&scroller));
    toolbar.set_content(Some(&overlay));
    window.set_content(Some(&toolbar));

    // ---------------------------------------------------------------
    // Sauvegarde
    // ---------------------------------------------------------------
    {
        let cfg = cfg.clone();
        let delay_row = delay_row.clone();
        let mode_row = mode_row.clone();
        let helper_row = helper_row.clone();
        let scan_row = scan_row.clone();
        let ai_row = ai_row.clone();
        let provider_row = provider_row.clone();
        let votes_row = votes_row.clone();
        let overlay = overlay.clone();
        save_btn.connect_clicked(move |_| {
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
                c.ai.provider = provider_from_index(provider_row.selected());
                c.ai.confirm_votes = votes_row.value() as u32;
            }
            let toast = match cfg.borrow().save() {
                Ok(_) => adw::Toast::new("Configuration enregistrée"),
                Err(e) => adw::Toast::new(&format!("Erreur : {e}")),
            };
            toast.set_timeout(2);
            overlay.add_toast(toast);
        });
    }

    // ---------------------------------------------------------------
    // Vérification (en arrière-plan)
    // ---------------------------------------------------------------
    {
        let cfg = cfg.clone();
        let results = results.clone();
        let check_btn_inner = check_btn.clone();
        let apply_btn = apply_btn.clone();
        check_btn.connect_clicked(move |_| {
            check_btn_inner.set_sensitive(false);
            check_btn_inner.set_label("Vérification…");
            clear_listbox(&results);
            results.set_visible(true);

            let snapshot = cfg.borrow().clone();
            let (tx, rx) = async_channel::bounded::<Result<Vec<Outcome>, String>>(1);
            std::thread::spawn(move || {
                let res = pipeline::evaluate(&snapshot).map_err(|e| e.to_string());
                let _ = tx.send_blocking(res);
            });

            let results = results.clone();
            let check_btn_inner = check_btn_inner.clone();
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

    // ---------------------------------------------------------------
    // Apply : lance `aur-guard apply` dans un terminal. La CLI gère toute la
    // logique (mode lag = build via makepkg, sinon helper -S) et l'interaction
    // sudo.
    // ---------------------------------------------------------------
    {
        let overlay = overlay.clone();
        apply_btn.connect_clicked(move |_| {
            let _ = launch_in_terminal("aur-guard apply");
            overlay.add_toast(adw::Toast::new("Installation lancée dans un terminal"));
        });
    }

    window.present();
}

/// Crée une ligne de paquet whitelisté avec un bouton de suppression.
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

fn update_wl_subtitle(expander: &adw::ExpanderRow, cfg: &Rc<RefCell<Config>>) {
    expander.set_subtitle(&format!(
        "{} paquets de confiance",
        cfg.borrow().whitelist.len()
    ));
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
