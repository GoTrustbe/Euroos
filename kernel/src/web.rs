//! Kernel side of **EuroWeb** (Track B, Sprint AB): the sovereign browser engine.
//! At boot we prove the first phase — HTML5 tokenizer + tree-construction → DOM —
//! on a realistic fragment, entirely in the kernel (no_std). Host-tested core:
//! [`euroweb`]. See `docs/EUROBROWSER-PLAN.md`.

use crate::serial_println;

/// Boot self-test: parse an HTML page and prove the DOM structure + entities.
pub fn selftest() {
    let html = "<!DOCTYPE html>\
        <html lang=\"nl\"><head><title>EuroOS &mdash; sovereign</title></head>\
        <body><h1 class=\"hero\">Welcome</h1>\
        <p>Built from scratch in <strong>Rust</strong> &amp; secure.</p>\
        <ul><li>HTML</li><li>CSS</li><li>Layout</li></ul>\
        <script>if (a<b) f()</script></body></html>";

    let dom = euroweb::parse(html);

    let html_n = dom.count_tag("html");
    let li_n = dom.count_tag("li");
    let strong_n = dom.count_tag("strong");

    // Title RCDATA with entity (&mdash; → —, &amp; → &).
    let title = (0..dom.len()).find(|&i| dom.tag(i) == Some("title"));
    let title_txt = title.map(|i| dom.text_content(i)).unwrap_or_default();

    // Attribute reachable.
    let h1 = (0..dom.len()).find(|&i| dom.tag(i) == Some("h1"));
    let h1_class = h1.and_then(|i| dom.attr(i, "class")).unwrap_or("");

    // RAWTEXT: <script> keeps `a<b` as text, NOT a <b> element.
    let script = (0..dom.len()).find(|&i| dom.tag(i) == Some("script"));
    let script_txt = script.map(|i| dom.text_content(i)).unwrap_or_default();
    let no_stray_b = dom.count_tag("b") == 0;

    // CSS cascade (AB-B2): specificity + inheritance over the DOM.
    let ua = euroweb::parse_stylesheet("h1 { color: black } p { color: gray }");
    let author = euroweb::parse_stylesheet(
        ".hero { color: blue } h1.hero { color: green } strong { font-weight: bold }",
    );
    let styles = euroweb::compute(&dom, &[&ua, &author]);
    // h1.hero (spec 0,1,1) beats .hero (0,1,0) and h1 UA (0,0,1).
    let h1_color = h1
        .and_then(|i| styles[i].get("color").cloned())
        .unwrap_or_default();
    // 'color' of <p> inherits down to the <strong> inside it.
    let strong = (0..dom.len()).find(|&i| dom.tag(i) == Some("strong"));
    let strong_color = strong
        .and_then(|i| styles[i].get("color").cloned())
        .unwrap_or_default();
    let css_ok = h1_color == "green" && strong_color == "gray";

    // Layout (AB-B3): block box model — two 50px divs stack vertically.
    let ldom = euroweb::parse("<body><div></div><div></div></body>");
    let lss = euroweb::parse_stylesheet("div { height: 50px }");
    let lstyles = euroweb::compute(&ldom, &[&lss]);
    let lb = euroweb::layout(&ldom, &lstyles, 800.0);
    let layout_ok = lb.children.len() == 2
        && lb.children[0].dimensions.content.y == 0.0
        && lb.children[1].dimensions.content.y == 50.0
        && lb.children[0].dimensions.content.width == 800.0;

    // Flexbox (AB-B4): two grow-1 items in a 300px container → 150px each.
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
        && title_txt == "EuroOS — sovereign"
        && h1_class == "hero"
        && script_txt.contains("a<b")
        && no_stray_b
        && css_ok
        && layout_ok
        && flex_ok;

    serial_println!(
        "[ab] EuroWeb tokenizer+DOM+CSS+layout+flex: {} nodes, html={} li={} strong={}, title=\"{}\", h1.class={}, h1.color={} strong.color(inherited)={}, layout div2.y={} flex(2×grow→150)={} {}",
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
        if ok { "✓" } else { "✗ ERROR" }
    );
}

/// AG-2 boot self-test: images (`<img>` + QOI/PPM decode) and forms
/// (`<input>`/`<form>` → real GET query). Proves the entire chain: decode →
/// layout-replaced-box → display-item → submit-URL.
pub fn selftest_ag2() {
    use euroweb::DisplayItem;

    // 1) euromedia: QOI round-trip + PPM decode.
    let mut img = euromedia::Image::new(2, 2, [0, 0, 0, 255]);
    img.set(0, 0, [226, 163, 58, 255]);
    img.set(1, 1, [45, 107, 224, 255]);
    let qoi_ok = euromedia::decode(&euromedia::encode(&img)) == Ok(img.clone());
    let ppm = euromedia::decode_ppm(b"P3 2 1 255 255 0 0 0 255 0").unwrap();
    let ppm_ok = ppm.get(0, 0) == Some([255, 0, 0, 255]) && ppm.get(1, 0) == Some([0, 255, 0, 255]);

    // 2) engine: <img> becomes an Image display item with src + intrinsic size.
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

    // 4) submit: load the demo page inline and build the real GET URL.
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
        "[ag2] EuroWeb images+forms: QOI={} PPM={} img-box={} field={} button={} submit-GET=\"{}\" {}",
        qoi_ok, ppm_ok, img_ok, field_ok, button_ok, target,
        if ok { "✓" } else { "✗ ERROR" }
    );
}

/// **[post]** (Sprint 4) — prove that the EuroWeb engine recognizes a `method="post"` form
/// and builds the CORRECT POST request (method + urlencoded body), alongside the
/// existing GET path. No external server needed: we verify the request construction.
pub fn selftest_post() {
    let html = "<html><head><title>Contact</title></head><body>\
        <form action=\"/contact\" method=\"post\">\
        <input type=\"text\" name=\"naam\" value=\"Ada\">\
        <input type=\"text\" name=\"bericht\" value=\"Hallo EuroOS\">\
        <input type=\"submit\" value=\"Versturen\"></form></body></html>";
    crate::webview::init("http://euro-os.eu/");
    crate::webview::load_inline("http://euro-os.eu/", html);
    let dom = euroweb::parse(html);
    let submit = (0..dom.len()).find(|&i| dom.tag(i) == Some("input") && dom.attr(i, "type") == Some("submit"));
    let (method, action, body) = submit.and_then(crate::webview::post_request).unwrap_or_default();
    let method_ok = method == "post";
    let action_ok = action == "http://euro-os.eu/contact";
    let body_ok = body == "naam=Ada&bericht=Hallo+EuroOS"; // urlencoded (space → +)

    // Counter-check: a GET form stays "get" (no false POST).
    let get_html = "<body><form action=\"/zoek\" method=\"get\"><input type=\"text\" name=\"q\" value=\"x\"><input type=\"submit\"></form></body>";
    crate::webview::load_inline("http://euro-os.eu/", get_html);
    let gdom = euroweb::parse(get_html);
    let gsub = (0..gdom.len()).find(|&i| gdom.tag(i) == Some("input") && gdom.attr(i, "type") == Some("submit"));
    let get_method = gsub.and_then(crate::webview::post_request).map(|(m, _, _)| m).unwrap_or_default();
    let get_ok = get_method == "get";

    let ok = method_ok && action_ok && body_ok && get_ok;
    serial_println!(
        "[post] EuroWeb form POST: method={method_ok} action-abs={action_ok} body=\"{body}\" ({}) get-stays-get={get_ok} {}",
        if body_ok { "correct urlencoded" } else { "ERROR" },
        if ok { "✓" } else { "✗ ERROR" }
    );
}

/// **[js]** (Sprint 4) — prove that a small JS snippet REALLY runs on a page:
/// the EuroJS engine executes the `<script>`, `document.write` mutates the page, and
/// `console.log` is captured. We load the page and check that the
/// document.write output appears in the (augmented) DOM.
pub fn selftest_js() {
    // 1) Pure engine: run_page captures console.log + document.write.
    let (_r, logs, writes) =
        eurojs::run_page("console.log('start'); var x = 6 * 7; document.write('Antwoord: ' + x);");
    let engine_ok = logs.iter().any(|l| l == "start") && writes.join("") == "Antwoord: 42";

    // 2) Integration: a page with a script → document.write lands in the page.
    let html = "<html><head><title>JS</title></head><body><h1>EuroJS</h1>\
        <script>var n = 0; for (var i = 1; i <= 8; i = i + 1) { n = n + i; } \
        document.write('<p>Som 1..8 = ' + n + '</p>'); console.log('berekend', n);</script>\
        </body></html>";
    crate::webview::init("http://euro-os.eu/");
    crate::webview::load_inline("http://euro-os.eu/", html);
    let rendered = crate::webview::active_html();
    let dom_ok = rendered.contains("Som 1..8 = 36");
    // The page shows the truly-computed value via the DOM (paint → euroweb).
    let painted = {
        let d = euroweb::parse(&rendered);
        let s = euroweb::compute(&d, &[]);
        let lb = euroweb::layout(&d, &s, 800.0);
        let items = euroweb::paint(&d, &s, &lb);
        items.iter().any(|i| matches!(i, euroweb::DisplayItem::Text { text, .. } if text.contains("36")))
    };

    let ok = engine_ok && dom_ok && painted;
    serial_println!(
        "[js] EuroJS on a page: engine(console+document.write)={engine_ok} dom-mutated={dom_ok} rendered={painted} {}",
        if ok { "✓" } else { "✗ ERROR" }
    );
}
