//! The browser's own pages: `about:accueil`, `about:historique`,
//! `about:preferences`, and the pages that stand in for a failure or a
//! download.
//!
//! They are HTML, laid out by the same engine as any site: no second way
//! of drawing a page, and whatever the engine gets wrong shows here first.
//! Their actions are links to `about:action/...`, which the window
//! performs before showing the preferences again: the browser's own
//! pages carry no script, so that a page of the web cannot pass for one.

use alloc::format;
use alloc::string::String;

use crate::web::page::escape;

const STYLE: &str = "<style>
body { background: #f6f7f9; color: #202124; margin: 0 }
.page { max-width: 820px; margin: 0 auto; padding: 24px }
h1 { color: #1a4d8f }
h2 { font-size: 18px; margin-top: 28px; border-bottom: 1px solid #d0d4da }
.box { background: white; border: 1px solid #d0d4da; padding: 12px; margin: 12px 0 }
.dim { color: #6b7280 }
.error { color: #b3261e; font-weight: bold }
a { color: #1a5fb4 }
li { margin: 4px 0 }
</style>";

/// Shortcuts shown on the home page.
const LINKS: &[(&str, &str)] = &[
    ("DuckDuckGo", "https://html.duckduckgo.com/html/"),
    ("Wikipedia (fr)", "https://fr.wikipedia.org/wiki/Wikip%C3%A9dia:Accueil_principal"),
    ("Wikipedia (en)", "https://en.wikipedia.org/wiki/Main_Page"),
    ("Hacker News", "https://news.ycombinator.com/"),
    ("Le langage Rust", "https://www.rust-lang.org/fr"),
    ("OSDev Wiki", "https://wiki.osdev.org/Expanded_Main_Page"),
];

pub(crate) fn wrap(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>{}</title>{}</head><body><div class=\"page\">{}</div></body></html>",
        escape(title),
        STYLE,
        body
    )
}

/// The home page: a search box and the places visited last.
pub fn home(search_action: &str, field: &str, recent: &[(String, String)]) -> String {
    let mut body = String::new();
    body.push_str("<h1>Nova Navigateur</h1>");
    body.push_str("<div class=\"box\"><form method=\"get\" action=\"");
    body.push_str(&escape(search_action));
    body.push_str(&format!(
        "\"><input type=\"text\" name=\"{}\" size=\"60\" placeholder=\"Rechercher sur le web\"> \
         <input type=\"submit\" value=\"Rechercher\"></form></div>",
        field
    ));
    body.push_str("<h2>Raccourcis</h2><ul>");
    for (label, url) in LINKS {
        body.push_str(&format!("<li><a href=\"{}\">{}</a></li>", url, label));
    }
    body.push_str("</ul>");
    if !recent.is_empty() {
        body.push_str("<h2>Visites recentes</h2><ul>");
        for (url, title) in recent.iter().take(8) {
            let label = if title.is_empty() { url } else { title };
            body.push_str(&format!("<li><a href=\"{}\">{}</a> <span class=\"dim\">{}</span></li>", escape(url), escape(label), escape(url)));
        }
        body.push_str("</ul>");
    }
    body.push_str(
        "<h2>Raccourcis clavier</h2><ul>\
         <li>Ctrl+T : nouvel onglet - Ctrl+W : fermer l'onglet - Ctrl+Tab : onglet suivant</li>\
         <li>Ctrl+L : barre d'adresse - F5 : recharger - Alt+Gauche / Alt+Droite : precedent / suivant</li>\
         <li>Molette, fleches, Page haut / bas, Espace : defiler - Ctrl+clic : ouvrir dans un onglet</li>\
         <li>Ctrl+H : historique</li>\
         <li>Ctrl+J : console des scripts de la derniere page</li></ul>\
         <p class=\"dim\">Pages HTML et CSS, scripts JavaScript (ES2022 et DOM). Les images ne sont pas affichees. \
         <a href=\"about:preferences\">Preferences, cookies et cache</a> - <a href=\"about:console\">Console</a></p>",
    );
    wrap("Accueil", &body)
}

/// `about:console`: what the scripts of the last page printed, and
/// their errors.
pub fn console(lines: &[String]) -> String {
    let mut body = String::from("<h1>Console</h1>");
    if lines.is_empty() {
        body.push_str("<p class=\"dim\">Aucune ligne : la derniere page n'a rien ecrit, ou n'avait pas de script.</p>");
    } else {
        body.push_str("<div class=\"box\"><pre>");
        for line in lines.iter().rev().take(500).rev() {
            body.push_str(&escape(line));
            body.push('\n');
        }
        body.push_str("</pre></div>");
    }
    wrap("Console", &body)
}

pub fn history(entries: &[(String, String)]) -> String {
    let mut body = String::from("<h1>Historique</h1>");
    if entries.is_empty() {
        body.push_str("<p class=\"dim\">Aucune page visitee.</p>");
    } else {
        body.push_str("<ol>");
        for (url, title) in entries.iter().rev() {
            let label = if title.is_empty() { url } else { title };
            body.push_str(&format!("<li><a href=\"{}\">{}</a><br><span class=\"dim\">{}</span></li>", escape(url), escape(label), escape(url)));
        }
        body.push_str("</ol>");
    }
    body.push_str("<p><a href=\"about:action/effacer-historique\">Effacer l'historique</a></p>");
    wrap("Historique", &body)
}

pub fn preferences(
    engine: &str,
    domains: &[(String, usize)],
    cookies: usize,
    cache_entries: usize,
    cache_bytes: usize,
    message: &str,
) -> String {
    let mut body = String::from("<h1>Preferences</h1>");
    if !message.is_empty() {
        body.push_str(&format!("<div class=\"box\">{}</div>", escape(message)));
    }
    body.push_str("<h2>Moteur de recherche</h2><ul>");
    for (key, label) in [
        ("duckduckgo", "DuckDuckGo (version HTML, par defaut)"),
        ("duckduckgo-lite", "DuckDuckGo Lite"),
        ("wikipedia", "Wikipedia (recherche d'articles)"),
    ] {
        let marker = if key == engine { " <b>(actif)</b>" } else { "" };
        body.push_str(&format!("<li><a href=\"about:action/moteur/{}\">{}</a>{}</li>", key, label, marker));
    }
    body.push_str("</ul>");
    body.push_str(&format!(
        "<h2>Cookies</h2><p>{} cookie(s) pour {} site(s). Les cookies de session disparaissent a la fermeture \
         du navigateur ; les autres sont gardes dans vos reglages jusqu'a leur expiration.</p>",
        cookies,
        domains.len()
    ));
    if !domains.is_empty() {
        body.push_str("<ul>");
        for (domain, count) in domains.iter().take(50) {
            body.push_str(&format!("<li>{} : {}</li>", escape(domain), count));
        }
        body.push_str("</ul>");
    }
    body.push_str("<p><a href=\"about:action/effacer-cookies\">Effacer tous les cookies</a></p>");
    body.push_str(&format!(
        "<h2>Cache</h2><p>{} page(s) ou feuille(s) de style en memoire, {} Kio. Le cache respecte Cache-Control, \
         Expires, ETag et Last-Modified ; il est aussi garde sur le disque.</p>\
         <p><a href=\"about:action/effacer-cache\">Vider le cache</a></p>",
        cache_entries,
        cache_bytes / 1024
    ));
    body.push_str(
        "<h2>Historique</h2><p><a href=\"about:historique\">Voir l'historique</a> - \
         <a href=\"about:action/effacer-historique\">Effacer l'historique</a></p>\
         <h2>Certificats</h2><p><a href=\"about:certificats\">Autorites de confiance et exceptions par site</a></p>",
    );
    wrap("Preferences", &body)
}

pub fn error(url: &str, reason: &str) -> String {
    let body = format!(
        "<h1>Page inaccessible</h1><div class=\"box\"><p class=\"error\">{}</p><p>Adresse : {}</p></div>\
         <p><a href=\"{}\">Reessayer</a> - <a href=\"about:accueil\">Accueil</a></p>\
         <p class=\"dim\">Verifiez la connexion dans Parametres, onglet Reseau. Un site qui n'accepte que TLS 1.0 ou 1.1, \
         ou qui exige JavaScript pour afficher son contenu, ne peut pas etre ouvert ici.</p>",
        escape(reason),
        escape(url),
        escape(url)
    );
    wrap("Erreur", &body)
}

pub fn downloaded(url: &str, path: Option<&str>, bytes: usize, content_type: &str, failure: Option<&str>) -> String {
    let body = match (path, failure) {
        (Some(path), None) => format!(
            "<h1>Telechargement termine</h1><div class=\"box\"><p>{} ({} Kio, {})</p><p>Enregistre dans <b>{}</b></p></div>\
             <p class=\"dim\">Ce type de fichier ne s'affiche pas dans le navigateur : ouvrez-le avec l'explorateur.</p>",
            escape(url),
            bytes / 1024,
            escape(content_type),
            escape(path)
        ),
        (_, failure) => format!(
            "<h1>Telechargement impossible</h1><div class=\"box\"><p class=\"error\">{}</p><p>{}</p></div>",
            escape(failure.unwrap_or("aucune session ouverte")),
            escape(url)
        ),
    };
    wrap("Telechargement", &body)
}

/// The search URL of an engine key.
pub fn engine_url(key: &str) -> &'static str {
    match key {
        "duckduckgo-lite" => "https://lite.duckduckgo.com/lite/?q=",
        "wikipedia" => "https://fr.wikipedia.org/w/index.php?search=",
        _ => "https://html.duckduckgo.com/html/?q=",
    }
}

/// The form action of the home page's search box for an engine.
pub fn engine_form(key: &str) -> &'static str {
    match key {
        "duckduckgo-lite" => "https://lite.duckduckgo.com/lite/",
        "wikipedia" => "https://fr.wikipedia.org/w/index.php",
        _ => "https://html.duckduckgo.com/html/",
    }
}

/// The name of the query field of an engine's form.
pub fn engine_field(key: &str) -> &'static str {
    match key {
        "wikipedia" => "search",
        _ => "q",
    }
}
