//! HTML visuals of local runs, built from their Plotly and GeoJSON assets.
//!
//! Plotly assets render with plotly.js and GeoJSON assets with Leaflet,
//! both loaded from a CDN, from in-house templates. Chart.js assets have no
//! local visual. The SDK only writes the files: opening them is the
//! caller's job.
//!
//! An asset's content goes into the page as JSON in a `<script>`, with `<`,
//! `>`, `&` and the JavaScript line separators escaped, so no content can
//! end the script. Labels are escaped as HTML, and the map's tooltips and
//! popups set the features' properties as text, never as HTML.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::run_dir;
use crate::decision::fs::write_atomically;
use crate::{Error, Result};

/// The plotly.js that Plotly visuals load, from the 3.x line, with its
/// subresource integrity hash.
const PLOTLY_JS: &str = r#"<script src="https://cdn.plot.ly/plotly-3.1.0.min.js" integrity="sha384-DAxS2fhSGacPW3IdpTjDpu+KotwjM8aHsfrkZRnfYyJIhAHoDav7jAJ+NmYcp6PL" crossorigin="anonymous"></script>"#;

/// The Leaflet that GeoJSON visuals load, with its subresource integrity
/// hashes.
const LEAFLET: &str = r#"<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" integrity="sha384-sHL9NAb7lN7rfvG5lfHpm643Xkcjzp4jFvuavGOndn6pjVqS6ny56CAt3nsEVT4H" crossorigin="anonymous">
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js" integrity="sha384-cxOPjt7s7Iz04uaHJceBmS+qpjv2JkIHNVcuOrM+YHwZOmJGBXI00mdUXEq65HTH" crossorigin="anonymous"></script>"#;

/// The page body that shows the Plotly figure `__FIGURE__`, a JSON object
/// with `data`, `layout`, `config` and `frames`, as plotly.py's `from_json`
/// reads it.
const PLOTLY_BODY: &str = r#"<style>html, body, #plot { height: 100%; margin: 0; }</style>
</head>
<body>
<div id="plot"></div>
<script>
const figure = __FIGURE__;
figure.config = Object.assign({ responsive: true }, figure.config);
Plotly.newPlot("plot", figure);
</script>
</body>
</html>
"#;

/// The page body that shows the GeoJSON `__DATA__` on an OpenStreetMap map,
/// centered and zoomed as `__VIEW__` says, with a tooltip and a popup of
/// the view's fields of each feature's properties, and a control that
/// toggles the layer. The layer's name is HTML, which Leaflet's layer
/// control sets as such.
const GEOJSON_BODY: &str = r#"<style>html, body, #map { height: 100%; margin: 0; }</style>
</head>
<body>
<div id="map"></div>
<script>
const view = __VIEW__;
const data = __DATA__;
const map = L.map("map").setView(view.center, view.zoom);
L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", {
  maxZoom: 19,
  attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
}).addTo(map);
function fieldsTable(properties, fields) {
  const table = document.createElement("table");
  for (const field of fields) {
    const row = table.insertRow();
    row.insertCell().textContent = field;
    const value = properties[field];
    row.insertCell().textContent =
      value === undefined ? "" : typeof value === "string" ? value : JSON.stringify(value);
  }
  return table;
}
const layer = L.geoJSON(data, {
  onEachFeature(feature, featureLayer) {
    const properties = (feature && feature.properties) || {};
    if (view.tooltip.length > 0) {
      featureLayer.bindTooltip(() => fieldsTable(properties, view.tooltip), { sticky: true });
    }
    if (view.popup.length > 0) {
      featureLayer.bindPopup(() => fieldsTable(properties, view.popup));
    }
  },
}).addTo(map);
L.control.layers(null, { [view.layer]: layer }).addTo(map);
</script>
</body>
</html>
"#;

/// The longest file name, without `.html`, that a visual gets.
const MAX_NAME_LEN: usize = 100;

/// Builds the HTML visuals of the local run `run_id` of the workspace in
/// `src` from its assets, `outputs/assets.json`, into the run's `visuals/`
/// directory, and returns their paths, in the order of the assets.
///
/// Each Plotly or GeoJSON asset whose content is a JSON object, or, for
/// GeoJSON, a string that holds one, gets `<label>.html`; one whose content
/// is a list gets `<label>_<index>.html` for each object in it. Other
/// assets, and other content, have no visual. Labels are made safe as file
/// names (ASCII letters, digits, `.`, `_` and `-`), and two visuals with the
/// same name, even in another case, both stay: the second gets `_2`, and so
/// on. A run without assets has no visuals.
///
/// It writes the same files every time, so it can run on every read.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `run_id` cannot be a local run ID, and
/// [`Error::Io`] if the assets cannot be read or are not JSON, or the files
/// cannot be written.
pub(crate) async fn render(src: &Path, run_id: &str) -> Result<Vec<PathBuf>> {
    let dir = run_dir::run_dir(src, run_id)?;
    let path = run_dir::assets_file(&dir);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(Error::Io { path, source }),
    };
    let assets: Value = serde_json::from_slice(&bytes).map_err(|error| Error::Io {
        path: path.clone(),
        source: io::Error::new(io::ErrorKind::InvalidData, error),
    })?;
    let visuals_dir = run_dir::visuals_dir(&dir);
    let mut names = Names::default();
    let mut paths = Vec::new();
    for (name, html) in pages(&assets) {
        let path = visuals_dir.join(format!("{}.html", names.unique(&name)));
        write_atomically(&path, html.as_bytes()).await?;
        paths.push(path);
    }
    Ok(paths)
}

/// A kind of visual that local runs render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Schema {
    Plotly,
    GeoJson,
}

/// The pages of the visual assets in `assets`, the content of
/// `assets.json`, `{"assets": […]}`: each one's file name, before it is
/// made unique, and its HTML.
fn pages(assets: &Value) -> Vec<(String, String)> {
    let assets = assets.get("assets").and_then(Value::as_array);
    let mut pages = Vec::new();
    for asset in assets.into_iter().flatten() {
        let Some(visual) = asset.get("visual") else {
            continue;
        };
        // Assets name the schema `schema` or `visual_schema`.
        let schema = visual.get("schema").or_else(|| visual.get("visual_schema"));
        let schema = match schema.and_then(Value::as_str) {
            Some("plotly") => Schema::Plotly,
            Some("geojson") => Schema::GeoJson,
            _ => continue,
        };
        let label = visual.get("label").and_then(Value::as_str).unwrap_or("");
        let parsed;
        let content = match asset.get("content") {
            // A GeoJSON string holds the GeoJSON.
            Some(Value::String(text)) if schema == Schema::GeoJson => {
                parsed = serde_json::from_str::<Value>(text).ok();
                parsed.as_ref()
            }
            content => content,
        };
        let base = file_name(label);
        match content {
            Some(Value::Object(content)) => {
                pages.push((base, page(schema, label, content, None)));
            }
            Some(Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    if let Value::Object(item) = item {
                        let html = page(schema, label, item, Some(index));
                        pages.push((format!("{base}_{index}"), html));
                    }
                }
            }
            _ => {}
        }
    }
    pages
}

/// The page of the `schema` visual `content` of the asset labeled `label`,
/// the item at `index` of its list, if it is one.
fn page(schema: Schema, label: &str, content: &Map<String, Value>, index: Option<usize>) -> String {
    match schema {
        Schema::Plotly => {
            let body = fill(PLOTLY_BODY, &[("__FIGURE__", &script_json(content))]);
            head(label, PLOTLY_JS) + &body
        }
        Schema::GeoJson => {
            let layer = match index {
                Some(index) => format!("{label} Layer {}", index + 1),
                None => format!("{label} Layer"),
            };
            let (latitude, longitude, zoom) = center_and_zoom(content);
            let (tooltip, popup) = fields(content);
            let view = json!({
                "center": [latitude, longitude],
                "zoom": zoom,
                "layer": escape_html(&layer),
                "tooltip": tooltip,
                "popup": popup,
            });
            let values = [
                ("__VIEW__", script_json(&view)),
                ("__DATA__", script_json(content)),
            ];
            let values = values.each_ref().map(|(key, value)| (*key, value.as_str()));
            head(label, LEAFLET) + &fill(GEOJSON_BODY, &values)
        }
    }
}

/// `template` with each placeholder of `values` replaced by its value, in
/// order. Only the template is searched, never a value, so a value that
/// holds a placeholder stays as it is.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    for (placeholder, value) in values {
        if let Some((before, after)) = rest.split_once(placeholder) {
            filled.push_str(before);
            filled.push_str(value);
            rest = after;
        }
    }
    filled.push_str(rest);
    filled
}

/// The start of a page titled `label`, up to its `<head>`'s `scripts`.
fn head(label: &str, scripts: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta \
         name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n\
         {scripts}\n",
        escape_html(label)
    )
}

/// The center, latitude and longitude, and the zoom level of a map of the
/// GeoJSON `data`: the mean of the coordinates of its features, zoomed out
/// more the more they spread, or New York City without features or
/// coordinates.
fn center_and_zoom(data: &Map<String, Value>) -> (f64, f64, u8) {
    const DEFAULT: (f64, f64, u8) = (40.7128, -74.0060, 12);
    let features = data.get("features").and_then(Value::as_array);
    let mut points = Vec::new();
    for feature in features.into_iter().flatten() {
        let coordinates = feature
            .get("geometry")
            .and_then(|geometry| geometry.get("coordinates"));
        if let Some(coordinates) = coordinates {
            collect_points(coordinates, &mut points);
        }
    }
    if points.is_empty() {
        return DEFAULT;
    }
    let count = points.len() as f64;
    let longitude = points.iter().map(|point| point.0).sum::<f64>() / count;
    let latitude = points.iter().map(|point| point.1).sum::<f64>() / count;
    let spread = |pick: fn(&(f64, f64)) -> f64| {
        let max = points.iter().map(pick).fold(f64::NEG_INFINITY, f64::max);
        let min = points.iter().map(pick).fold(f64::INFINITY, f64::min);
        max - min
    };
    let range = spread(|point| point.1).max(spread(|point| point.0));
    let zoom = match range {
        range if range > 10.0 => 4,
        range if range > 1.0 => 8,
        range if range > 0.1 => 10,
        _ => DEFAULT.2,
    };
    (latitude, longitude, zoom)
}

/// Adds to `points` every `[longitude, latitude]` pair in the nested lists
/// of `coordinates`: a list of exactly two numbers is a pair, and any other
/// list is searched.
fn collect_points(coordinates: &Value, points: &mut Vec<(f64, f64)>) {
    let Value::Array(items) = coordinates else {
        return;
    };
    if let [Value::Number(longitude), Value::Number(latitude)] = items.as_slice()
        && let (Some(longitude), Some(latitude)) = (longitude.as_f64(), latitude.as_f64())
    {
        points.push((longitude, latitude));
        return;
    }
    for item in items {
        collect_points(item, points);
    }
}

/// The property names that the tooltip and the popup of a map of the
/// GeoJSON `data` show, picked from its first feature: the common names
/// first, then the others, up to 3 for the tooltip and 5 for the popup. The
/// others come in name order, not the file's order: this crate's
/// `serde_json` maps sort their keys, and keeping the order would take its
/// `preserve_order` feature, which changes the order of every map in the
/// crate.
fn fields(data: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
    const COMMON: [&str; 6] = [
        "name",
        "title",
        "label",
        "id",
        "popupContent",
        "description",
    ];
    let properties = data
        .get("features")
        .and_then(Value::as_array)
        .and_then(|features| features.first())
        .and_then(|feature| feature.get("properties"))
        .and_then(Value::as_object);
    let Some(properties) = properties else {
        return (Vec::new(), Vec::new());
    };
    let common = COMMON.iter().filter(|name| properties.contains_key(**name));
    let mut tooltip: Vec<String> = common.map(|&name| name.to_owned()).collect();
    let mut popup = tooltip.clone();
    for name in properties.keys() {
        if !tooltip.contains(name) && tooltip.len() < 3 {
            tooltip.push(name.clone());
        }
        if !popup.contains(name) && popup.len() < 5 {
            popup.push(name.clone());
        }
    }
    (tooltip, popup)
}

/// The visual file names given so far, to make each one unique.
#[derive(Debug, Default)]
struct Names {
    /// The names, in lowercase, for file systems that ignore case.
    taken: HashSet<String>,
}

impl Names {
    /// `name`, or, if it is taken, `name` with the first free suffix of
    /// `_2`, `_3`, and so on.
    fn unique(&mut self, name: &str) -> String {
        let mut unique = name.to_owned();
        let mut suffix = 1;
        while !self.taken.insert(unique.to_ascii_lowercase()) {
            suffix += 1;
            unique = format!("{name}_{suffix}");
        }
        unique
    }
}

/// `label` as a file name: ASCII letters, digits, `.`, `_` and `-`, with
/// any other character as `_`, without leading dots, so that it is never
/// hidden, `.` or `..`, and at most [`MAX_NAME_LEN`] long. An empty one is
/// `visual`.
fn file_name(label: &str) -> String {
    let safe: String = label
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    let name: String = safe
        .trim_start_matches('.')
        .chars()
        .take(MAX_NAME_LEN)
        .collect();
    match name.is_empty() {
        true => "visual".to_owned(),
        false => name,
    }
}

/// `value` as JSON that can go inside a `<script>`: `<`, `>` and `&` are
/// written as escapes, so the text can never close the script or open a
/// comment, and so are U+2028 and U+2029, which end a line in older
/// JavaScript. They only occur in JSON strings, where the escapes mean the
/// same.
fn script_json(value: &impl serde::Serialize) -> String {
    // Maps with string keys and JSON values always serialize.
    let json = serde_json::to_string(value).unwrap_or_default();
    let mut escaped = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// `text` with the characters that mean something in HTML escaped.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A new empty directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let name = format!(
                "fico-sdk-visuals-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let dir = std::env::temp_dir().join(name);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const RUN_ID: &str = "local-3k9x0q2a";

    /// Writes `assets` as the assets of the run [`RUN_ID`] of the workspace
    /// in `src`.
    fn write_assets(src: &Path, assets: &Value) {
        let dir = run_dir::run_dir(src, RUN_ID).unwrap();
        let path = run_dir::assets_file(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(assets).unwrap()).unwrap();
    }

    fn asset(schema: &str, label: &str, content: Value) -> Value {
        json!({
            "name": label,
            "content_type": "json",
            "visual": { "schema": schema, "label": label, "type": "custom-tab" },
            "content": content,
        })
    }

    /// The JSON that `name` is set to in the `<script>` of `html`.
    fn script_value(html: &str, name: &str) -> Value {
        let start = format!("const {name} = ");
        let from = html.find(&start).unwrap() + start.len();
        let end = html[from..].find(";\n").unwrap();
        serde_json::from_str(&html[from..from + end]).unwrap()
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let names = paths.iter().map(|path| path.file_name().unwrap());
        names
            .map(|name| name.to_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn renders_plotly_and_geojson_assets() {
        let src = TempDir::new();
        let figure = json!({ "data": [{ "type": "bar", "x": [1, 2], "y": [3, 4] }] });
        let geojson = json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [-73.9, 40.7] },
                "properties": { "name": "depot" }
            }]
        });
        write_assets(
            &src.0,
            &json!({ "assets": [
                asset("plotly", "cost", figure.clone()),
                asset("geojson", "routes", json!([geojson.clone(), "not a map", geojson])),
                asset("chartjs", "chart", json!({ "type": "line" })),
                { "name": "raw", "content": { "a": 1 } },
                asset("plotly", "number", json!(3)),
            ]}),
        );
        let paths = render(&src.0, RUN_ID).await.unwrap();
        assert_eq!(
            names(&paths),
            ["cost.html", "routes_0.html", "routes_2.html"]
        );
        let visuals = src.0.join(".fico/runs").join(RUN_ID).join("visuals");
        for path in &paths {
            assert_eq!(path.parent().unwrap(), visuals);
        }

        let plotly = std::fs::read_to_string(&paths[0]).unwrap();
        assert!(plotly.starts_with("<!DOCTYPE html>"), "{plotly}");
        assert!(plotly.contains("<title>cost</title>"), "{plotly}");
        assert!(plotly.contains("https://cdn.plot.ly/plotly-3.1.0.min.js"));
        assert_eq!(script_value(&plotly, "figure"), figure);

        let map = std::fs::read_to_string(&paths[2]).unwrap();
        assert!(map.contains("https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"));
        let view = script_value(&map, "view");
        assert_eq!(
            view,
            json!({
                "center": [40.7, -73.9],
                "zoom": 12,
                "layer": "routes Layer 3",
                "tooltip": ["name"],
                "popup": ["name"],
            })
        );
        assert_eq!(
            script_value(&map, "data")["features"][0]["properties"]["name"],
            "depot"
        );

        // Again, the same files.
        let again = render(&src.0, RUN_ID).await.unwrap();
        assert_eq!(again, paths);
        let entries = std::fs::read_dir(&visuals).unwrap().count();
        assert_eq!(entries, 3);
    }

    #[tokio::test]
    async fn reads_geojson_given_as_a_string() {
        let src = TempDir::new();
        let geojson = r#"{"type": "FeatureCollection", "features": []}"#;
        write_assets(
            &src.0,
            &json!({ "assets": [
                asset("geojson", "map", json!(geojson)),
                asset("geojson", "broken", json!("{ not json")),
                // Only GeoJSON strings are parsed.
                asset("plotly", "chart", json!(r#"{"data": []}"#)),
            ]}),
        );
        let paths = render(&src.0, RUN_ID).await.unwrap();
        assert_eq!(names(&paths), ["map.html"]);
        let html = std::fs::read_to_string(&paths[0]).unwrap();
        assert_eq!(
            script_value(&html, "data"),
            serde_json::from_str::<Value>(geojson).unwrap()
        );
    }

    #[tokio::test]
    async fn keeps_visuals_whose_labels_clash() {
        let src = TempDir::new();
        let figure = || json!({ "data": [] });
        write_assets(
            &src.0,
            &json!({ "assets": [
                asset("plotly", "Cost", figure()),
                asset("plotly", "cost", figure()),
                asset("plotly", "cost", json!([figure()])),
                asset("plotly", "cost_0", figure()),
                asset("plotly", "../../etc/passwd", figure()),
                asset("plotly", "Ruta ñ/1", figure()),
                asset("plotly", "", figure()),
                asset("plotly", "...", figure()),
            ]}),
        );
        let paths = render(&src.0, RUN_ID).await.unwrap();
        assert_eq!(
            names(&paths),
            [
                "Cost.html",
                "cost_2.html",
                "cost_0.html",
                "cost_0_2.html",
                "_.._etc_passwd.html",
                "Ruta___1.html",
                "visual.html",
                "visual_2.html",
            ]
        );
        let visuals = paths[0].parent().unwrap();
        assert_eq!(std::fs::read_dir(visuals).unwrap().count(), paths.len());
    }

    #[tokio::test]
    async fn escapes_what_could_end_the_script() {
        let src = TempDir::new();
        let hostile = "</script><script>alert(1)</script><!-- & \u{2028}";
        let geojson = json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [0, 0] },
                "properties": { "name": hostile }
            }]
        });
        write_assets(
            &src.0,
            &json!({ "assets": [
                asset("geojson", "<b>__DATA__</b>", geojson.clone()),
                asset("plotly", "x", json!({ "layout": { "title": hostile } })),
            ]}),
        );
        let paths = render(&src.0, RUN_ID).await.unwrap();
        for path in &paths {
            let html = std::fs::read_to_string(path).unwrap();
            assert_eq!(html.matches("</script>").count(), 2, "{html}");
            assert!(
                !html.contains("<!--") && !html.contains('\u{2028}'),
                "{html}"
            );
            assert!(!html.contains("<b>"), "{html}");
        }
        let map = std::fs::read_to_string(&paths[0]).unwrap();
        assert!(
            map.contains("<title>&lt;b&gt;__DATA__&lt;/b&gt;</title>"),
            "{map}"
        );
        // The label's placeholder text is not filled in.
        assert_eq!(script_value(&map, "data"), geojson);
        assert_eq!(
            script_value(&map, "view")["layer"],
            "&lt;b&gt;__DATA__&lt;/b&gt; Layer"
        );
        let plotly = std::fs::read_to_string(&paths[1]).unwrap();
        assert_eq!(script_value(&plotly, "figure")["layout"]["title"], hostile);
    }

    #[tokio::test]
    async fn has_no_visuals_without_assets() {
        let src = TempDir::new();
        assert_eq!(render(&src.0, RUN_ID).await.unwrap(), Vec::<PathBuf>::new());
        write_assets(&src.0, &json!({ "assets": [] }));
        assert_eq!(render(&src.0, RUN_ID).await.unwrap(), Vec::<PathBuf>::new());
        let dir = run_dir::run_dir(&src.0, RUN_ID).unwrap();
        assert!(!run_dir::visuals_dir(&dir).exists());

        std::fs::write(run_dir::assets_file(&dir), "{ not json").unwrap();
        let err = render(&src.0, RUN_ID).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        let err = render(&src.0, "../x").await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    }

    #[test]
    fn centers_on_the_mean_and_zooms_out_by_the_spread() {
        let points = |coordinates: Value| {
            let data = json!({ "features": [
                { "geometry": { "type": "LineString", "coordinates": coordinates } },
                { "geometry": null },
                { "properties": {} }
            ]});
            center_and_zoom(data.as_object().unwrap())
        };
        assert_eq!(points(json!([[0, 0], [20, 10]])), (5.0, 10.0, 4));
        assert_eq!(points(json!([[0, 0], [2, 1]])), (0.5, 1.0, 8));
        assert_eq!(points(json!([[0, 0], [0.5, 0]])).2, 10);
        assert_eq!(points(json!([[[[1, 2]]], [3, 4]])), (3.0, 2.0, 8));
        // Not pairs of numbers: none, so New York City.
        assert_eq!(
            points(json!([[1, 2, 3], ["a", "b"]])),
            (40.7128, -74.006, 12)
        );
        assert_eq!(center_and_zoom(&Map::new()), (40.7128, -74.006, 12));
    }

    #[test]
    fn picks_common_fields_first_then_others_by_name() {
        let data = json!({ "features": [{ "properties": {
            "stop": 1, "id": "s1", "load": 3, "name": "Depot", "eta": "9:00", "kind": "x", "zone": 2
        }}]});
        let (tooltip, popup) = fields(data.as_object().unwrap());
        // The other names in name order: `eta`, not `stop`, the first in
        // the file.
        assert_eq!(tooltip, ["name", "id", "eta"]);
        assert_eq!(popup, ["name", "id", "eta", "kind", "load"]);
        assert_eq!(fields(&Map::new()), (Vec::new(), Vec::new()));
    }

    #[test]
    fn fills_only_the_template() {
        let filled = fill("a __X__ b __Y__ c", &[("__X__", "__Y__"), ("__Y__", "y")]);
        assert_eq!(filled, "a __Y__ b y c");
    }
}
