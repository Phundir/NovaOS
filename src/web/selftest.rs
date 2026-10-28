//! Self-test of the web client, with no network: addresses, cookies, the
//! cache, decompression, parsing and layout on documents of its own.
//!
//! The network half -- TLS against real servers, redirects, gzip -- was
//! exercised on the development machine against thirty sites; it cannot
//! be here, since a boot must not depend on the internet being there.
//! That is named, not hidden: the proof of the network path is a page
//! that opens in the browser.

use alloc::string::{String, ToString};

use super::cache::{self, Entry};
use super::cookies::Jar;
use super::url::{self, Url};
use super::{css, html, inflate, layout, page, text};

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[web] ECHEC : {}", label);
        }
    }
}

/// Bytes of stack left, from the scheduler.
pub fn headroom() -> usize {
    crate::scheduler::stack_headroom()
}

/// Runs the checks. Returns (passed, failed).
pub fn run() -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };

    // --- addresses ---
    let base = Url::parse("https://exemple.org/a/b/page.html?x=1#haut");
    report.check("une URL absolue se lit", base.is_some());
    if let Some(base) = &base {
        report.check("hote et chemin", base.host == "exemple.org" && base.path == "/a/b/page.html" && base.query == "x=1");
        report.check(
            "un lien relatif se resout",
            base.join("../c.html").map(|u| u.to_string()) == Some(String::from("https://exemple.org/a/c.html")),
        );
        report.check(
            "un lien de schema relatif garde https",
            base.join("//autre.org/x").map(|u| u.host) == Some(String::from("autre.org")),
        );
        report.check(
            "un fragment seul reste sur la page",
            base.join("#bas").map(|u| (u.path, u.fragment)) == Some((String::from("/a/b/page.html"), String::from("bas"))),
        );
    }
    report.check("javascript: n'est pas une adresse", Url::parse("javascript:alert(1)").is_none());
    report.check(
        "une recherche va a DuckDuckGo",
        url::from_input("noyau rust", "https://html.duckduckgo.com/html/?q=") == "https://html.duckduckgo.com/html/?q=noyau+rust",
    );
    report.check("un nom de site devient une adresse", url::from_input("wikipedia.org", "") == "https://wikipedia.org");

    // --- cookies ---
    if let Some(site) = Url::parse("https://www.exemple.org/compte") {
        let mut jar = Jar::new();
        jar.store(&site, "session=abc; Path=/; Secure; HttpOnly", 1000);
        jar.store(&site, "pref=fr; Domain=exemple.org; Max-Age=3600", 1000);
        jar.store(&site, "vole=1; Domain=autre.org", 1000);
        report.check("un cookie d'un autre site est refuse", jar.len() == 2);
        let header = jar.header_for(&site, 1001).unwrap_or_default();
        report.check("les cookies reviennent au site", header.contains("session=abc") && header.contains("pref=fr"));
        let plain = Url::parse("http://www.exemple.org/").expect("static URL");
        report.check(
            "un cookie Secure ne part pas en clair",
            !jar.header_for(&plain, 1001).unwrap_or_default().contains("session"),
        );
        report.check("Max-Age expire", jar.header_for(&site, 5000).is_some_and(|h| !h.contains("pref")));
        let saved = Jar::load(&jar.save(), 1001);
        report.check("seuls les cookies persistants sont ecrits", saved.len() <= 1);
    }

    // --- cache ---
    let headers = alloc::vec![(String::from("cache-control"), String::from("max-age=60"))];
    report.check("max-age donne la fraicheur", cache::policy(200, &headers, 10, 100) == Some(160));
    let no_store = alloc::vec![(String::from("cache-control"), String::from("no-store"))];
    report.check("no-store n'est pas garde", cache::policy(200, &no_store, 10, 100).is_none());
    let entry = Entry {
        url: String::from("https://exemple.org/"),
        content_type: String::from("text/html"),
        etag: String::from("\"v1\""),
        last_modified: String::new(),
        stored: 1,
        fresh_until: 2,
        body: alloc::vec![1, 2, 3],
    };
    report.check("une entree se relit du disque", Entry::decode(&entry.encode()) == Some(entry));

    // --- DEFLATE: a stored block and a fixed-Huffman block ---
    report.check(
        "DEFLATE, bloc non compresse",
        inflate::inflate(&[0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'], 1024).as_deref() == Some(&b"abc"[..]),
    );
    // zlib.compress(b"hello hello hello"): a fixed-Huffman block with a
    // back-reference.
    report.check(
        "DEFLATE, codes de Huffman fixes",
        inflate::unzlib(
            &[0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06, 0x7d],
            1024,
        )
        .as_deref()
            == Some(&b"hello hello hello"[..]),
    );
    // A fixed-Huffman block expanding past its bound is refused, not
    // allocated: the limit is what stops a decompression bomb.
    report.check(
        "la decompression est bornee",
        inflate::unzlib(
            &[0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06, 0x7d],
            8,
        )
        .is_none(),
    );

    // --- text ---
    report.check("entites nommees et numeriques", text::decode_entities("&eacute;t&#233; &amp; &lt;b&gt;") == "\u{e9}t\u{e9} & <b>");
    report.check("les accents se replient en ASCII", text::fold("\u{e9}l\u{e8}ve \u{153}uvre \u{201c}x\u{201d}") == "eleve oeuvre \"x\"");

    // --- HTML ---
    let document = html::parse("<p>un<p>deux<ul><li>a<li>b</ul><script>if (a < b) {}</script><br/>fin");
    report.check("un <p> ferme le precedent", document.elements("p").len() == 2 && document.nodes[document.elements("p")[1]].parent == document.nodes[document.elements("p")[0]].parent);
    report.check("un <li> ferme le precedent", document.elements("li").iter().all(|li| document.tag(document.nodes[*li].parent.unwrap_or(0)) == Some("ul")));
    report.check("un script reste du texte", document.text_content(document.elements("script")[0]).contains("a < b"));

    // --- CSS ---
    let sheet = css::parse("@media (max-width: 500px) { p { color: red } } div.a > p:first-child, #x { color: #00f !important }", 800);
    report.check("@media ecarte ce qui ne s'applique pas", sheet.rules.len() == 1);
    report.check("specificite d'un id", css::parse_selector("#x").map(|s| s.specificity) == Some(1 << 16));
    report.check("couleurs hexadecimales et nommees", css::parse_color("#0a0", 0) == Some(0x00aa00) && css::parse_color("navy", 0) == Some(0x000080));
    report.check("rgba se melange au fond", css::parse_color("rgba(0,0,0,0.5)", 0xffffff) == Some(0x808080));

    // --- cascade and layout, end to end ---
    // The headroom closure reports the real stack: the layout must stop
    // descending on the kernel's stack floor, as it does in the browser.
    let source = "<html><head><style>.titre { color: #c00; font-size: 32px } .cache { display: none }</style></head>\
                  <body><h1 class=\"titre\">Titre</h1><p>Un <a href=\"/lien\">lien</a> et du texte.</p>\
                  <div class=\"cache\">invisible</div><form action=\"/cherche\"><input name=\"q\" value=\"x\"></form>\
                  <table><tr><td>a</td><td>b</td></tr></table></body></html>";
    let loaded = page::from_html(Url::parse("https://exemple.org/").expect("static URL"), source);
    let laid = page::render(&loaded, 600, 400, &crate::web::selftest::headroom);
    let texts: alloc::vec::Vec<(String, u32, i32)> = laid
        .page
        .paints
        .iter()
        .filter_map(|paint| match paint {
            layout::Paint::Text { text, color, scale, .. } => Some((text.clone(), *color, *scale)),
            _ => None,
        })
        .collect();
    report.check("la feuille de la page s'applique", texts.iter().any(|(t, c, s)| t == "Titre" && *c == 0xcc0000 && *s == 2));
    report.check("display: none n'est pas dessine", !texts.iter().any(|(t, _, _)| t.contains("invisible")));
    report.check("un lien a sa zone cliquable", laid.page.links.iter().any(|link| link.href == "/lien"));
    report.check("un champ de formulaire est trouve", laid.page.controls.iter().any(|c| c.name == "q" && c.value == "x") && laid.forms.len() == 1);
    report.check("les cellules d'une ligne sont cote a cote", {
        let a = laid.page.paints.iter().find_map(|p| match p {
            layout::Paint::Text { text, x, y, .. } if text == "a" => Some((*x, *y)),
            _ => None,
        });
        let b = laid.page.paints.iter().find_map(|p| match p {
            layout::Paint::Text { text, x, y, .. } if text == "b" => Some((*x, *y)),
            _ => None,
        });
        matches!((a, b), (Some((xa, ya)), Some((xb, yb))) if xb > xa && ya == yb)
    });

    (report.passed, report.failed)
}
