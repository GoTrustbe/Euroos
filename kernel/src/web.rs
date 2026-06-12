//! Kernel-zijde van **EuroWeb** (Spoor B, Sprint AB): de soevereine browser-engine.
//! Bij boot bewijzen we de eerste fase — HTML5-tokenizer + tree-construction → DOM —
//! op een realistisch fragment, geheel in de kernel (no_std). Host-geteste kern:
//! [`euroweb`]. Zie `docs/EUROBROWSER-PLAN.md`.

use crate::serial_println;

/// Boot-zelftest: parse een HTML-pagina en bewijs de DOM-structuur + entiteiten.
pub fn selftest() {
    let html = "<!DOCTYPE html>\
        <html lang=\"nl\"><head><title>EuroOS &mdash; soeverein</title></head>\
        <body><h1 class=\"hero\">Welkom</h1>\
        <p>Van nul gebouwd in <strong>Rust</strong> &amp; veilig.</p>\
        <ul><li>HTML</li><li>CSS</li><li>Layout</li></ul>\
        <script>if (a<b) f()</script></body></html>";

    let dom = euroweb::parse(html);

    let html_n = dom.count_tag("html");
    let li_n = dom.count_tag("li");
    let strong_n = dom.count_tag("strong");

    // Titel-RCDATA met entiteit (&mdash; → —, &amp; → &).
    let title = (0..dom.len()).find(|&i| dom.tag(i) == Some("title"));
    let title_txt = title.map(|i| dom.text_content(i)).unwrap_or_default();

    // Attribuut bereikbaar.
    let h1 = (0..dom.len()).find(|&i| dom.tag(i) == Some("h1"));
    let h1_class = h1.and_then(|i| dom.attr(i, "class")).unwrap_or("");

    // RAWTEXT: <script> bewaart `a<b` als tekst, géén <b>-element.
    let script = (0..dom.len()).find(|&i| dom.tag(i) == Some("script"));
    let script_txt = script.map(|i| dom.text_content(i)).unwrap_or_default();
    let no_stray_b = dom.count_tag("b") == 0;

    // CSS-cascade (AB-B2): specificiteit + overerving over de DOM.
    let ua = euroweb::parse_stylesheet("h1 { color: black } p { color: gray }");
    let author = euroweb::parse_stylesheet(
        ".hero { color: blue } h1.hero { color: green } strong { font-weight: bold }",
    );
    let styles = euroweb::compute(&dom, &[&ua, &author]);
    // h1.hero (spec 0,1,1) wint van .hero (0,1,0) en h1 UA (0,0,1).
    let h1_color = h1
        .and_then(|i| styles[i].get("color").cloned())
        .unwrap_or_default();
    // 'color' van <p> erft door naar de <strong> erin.
    let strong = (0..dom.len()).find(|&i| dom.tag(i) == Some("strong"));
    let strong_color = strong
        .and_then(|i| styles[i].get("color").cloned())
        .unwrap_or_default();
    let css_ok = h1_color == "green" && strong_color == "gray";

    // Layout (AB-B3): block-boxmodel — twee divs van 50px stapelen verticaal.
    let ldom = euroweb::parse("<body><div></div><div></div></body>");
    let lss = euroweb::parse_stylesheet("div { height: 50px }");
    let lstyles = euroweb::compute(&ldom, &[&lss]);
    let lb = euroweb::layout(&ldom, &lstyles, 800.0);
    let layout_ok = lb.children.len() == 2
        && lb.children[0].dimensions.content.y == 0.0
        && lb.children[1].dimensions.content.y == 50.0
        && lb.children[0].dimensions.content.width == 800.0;

    // Flexbox (AB-B4): twee grow-1 items in een 300px-container → elk 150px.
    let fitems = [
        euroweb::flex::FlexItem::new(50.0, 1.0, 1.0),
        euroweb::flex::FlexItem::new(50.0, 1.0, 1.0),
    ];
    let fr = euroweb::flex::solve(300.0, &fitems, euroweb::flex::Justify::Start, 0.0);
    let flex_ok = fr.len() == 2
        && (fr[0].main_size - 150.0).abs() < 0.01
        && (fr[1].main_pos - 150.0).abs() < 0.01;

    let ok = html_n == 1
        && li_n == 3
        && strong_n == 1
        && title_txt == "EuroOS — soeverein"
        && h1_class == "hero"
        && script_txt.contains("a<b")
        && no_stray_b
        && css_ok
        && layout_ok
        && flex_ok;

    serial_println!(
        "[ab] EuroWeb tokenizer+DOM+CSS+layout+flex: {} knopen, html={} li={} strong={}, titel=\"{}\", h1.class={}, h1.color={} strong.color(geërfd)={}, layout div2.y={} flex(2×grow→150)={} {}",
        dom.len(),
        html_n,
        li_n,
        strong_n,
        title_txt,
        h1_class,
        h1_color,
        strong_color,
        lb.children.get(1).map(|c| c.dimensions.content.y).unwrap_or(-1.0) as i64,
        flex_ok,
        if ok { "✓" } else { "✗ FOUT" }
    );
}

/// AG-2 boot-zelftest: afbeeldingen (`<img>` + QOI/PPM-decode) en formulieren
/// (`<input>`/`<form>` → echte GET-query). Bewijst de hele keten: decode →
/// layout-replaced-box → display-item → submit-URL.
pub fn selftest_ag2() {
    use euroweb::DisplayItem;

    // 1) euromedia: QOI-round-trip + PPM-decode.
    let mut img = euromedia::Image::new(2, 2, [0, 0, 0, 255]);
    img.set(0, 0, [226, 163, 58, 255]);
    img.set(1, 1, [45, 107, 224, 255]);
    let qoi_ok = euromedia::decode(&euromedia::encode(&img)) == Ok(img.clone());
    let ppm = euromedia::decode_ppm(b"P3 2 1 255 255 0 0 0 255 0").unwrap();
    let ppm_ok = ppm.get(0, 0) == Some([255, 0, 0, 255]) && ppm.get(1, 0) == Some([0, 255, 0, 255]);

    // 2) engine: <img> wordt een Image-display-item met src + intrinsieke maat.
    let idom = euroweb::parse(r#"<body><img src="/logo.qoi" width="80" height="60"></body>"#);
    let istyles = euroweb::compute(&idom, &[]);
    let ilb = euroweb::layout(&idom, &istyles, 800.0);
    let iitems = euroweb::paint(&idom, &istyles, &ilb);
    let img_ok = iitems.iter().any(|i| matches!(i, DisplayItem::Image { src, w, h, .. } if src == "/logo.qoi" && *w == 80.0 && *h == 60.0));

    // 3) engine: <form> → Field + Button.
    let fdom = euroweb::parse(
        r#"<body><form action="/zoek" method="get"><input type="text" name="q" value="euro"><input type="submit" value="Zoek"></form></body>"#,
    );
    let fstyles = euroweb::compute(&fdom, &[]);
    let flb = euroweb::layout(&fdom, &fstyles, 800.0);
    let fitems = euroweb::paint(&fdom, &fstyles, &flb);
    let field_ok = fitems.iter().any(|i| matches!(i, DisplayItem::Field { name, value, .. } if name == "q" && value == "euro"));
    let button_ok = fitems.iter().any(|i| matches!(i, DisplayItem::Button { label, .. } if label == "Zoek"));

    // 4) submit: laad de demopagina inline en bouw de echte GET-URL.
    crate::webview::init("http://euro-os.eu/");
    crate::webview::load_inline("http://euro-os.eu/", &crate::webview::ag2_demo_html());
    let demo = crate::webview::ag2_demo_html();
    let ddom = euroweb::parse(&demo);
    let submit_node = (0..ddom.len())
        .find(|&i| ddom.tag(i) == Some("input") && ddom.attr(i, "type") == Some("submit"));
    let target = submit_node.and_then(crate::webview::submit_target).unwrap_or_default();
    let submit_ok = target == "http://euro-os.eu/zoek?q=soevereiniteit";

    let ok = qoi_ok && ppm_ok && img_ok && field_ok && button_ok && submit_ok;
    serial_println!(
        "[ag2] EuroWeb afbeeldingen+formulieren: QOI={} PPM={} img-box={} veld={} knop={} submit-GET=\"{}\" {}",
        qoi_ok, ppm_ok, img_ok, field_ok, button_ok, target,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
