use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct Entry {
    kind: String,
    mood: String,
    color: String,
    text: String,
    ts: u64,
}

#[derive(Debug)]
struct AppState {
    data_path: PathBuf,
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    body: String,
}

const MAX_BODY: usize = 16 * 1024;

fn main() -> std::io::Result<()> {
    if env::args().any(|arg| arg == "--healthcheck") {
        std::process::exit(if healthcheck() { 0 } else { 1 });
    }

    let addr = env::var("SPARK_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let data_path = env::var("SPARK_DATA").unwrap_or_else(|_| "data/spark-garden.tsv".to_string());
    let data_path = PathBuf::from(data_path);

    if let Some(parent) = data_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let state = Arc::new(Mutex::new(AppState {
        entries: load_entries(&data_path),
        data_path,
    }));

    let listener = TcpListener::bind(&addr)?;
    println!("Spark Garden is growing at http://{addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, state) {
                        eprintln!("request failed: {err}");
                    }
                });
            }
            Err(err) => eprintln!("connection failed: {err}"),
        }
    }

    Ok(())
}

fn healthcheck() -> bool {
    let addr = env::var("SPARK_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let target = if let Some(port) = addr.strip_prefix("0.0.0.0:") {
        format!("127.0.0.1:{port}")
    } else {
        addr
    };

    let Ok(mut stream) = TcpStream::connect(target) else {
        return false;
    };
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }

    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200 OK")
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<AppState>>) -> std::io::Result<()> {
    let request = match read_request(&mut stream)? {
        Some(request) => request,
        None => return Ok(()),
    };

    let response = route(request, state);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut first_line = String::new();
    if reader.read_line(&mut first_line)? == 0 {
        return Ok(None);
    }

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let path = target.split('?').next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0).min(MAX_BODY);
        }
        if let Some(value) = trimmed.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0).min(MAX_BODY);
        }
    }

    let mut body = vec![0; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    }))
}

fn route(request: Request, state: Arc<Mutex<AppState>>) -> String {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => html_response(&render_home(&state.lock().expect("state lock"))),
        ("GET", "/health") => response("200 OK", "text/plain; charset=utf-8", "ok"),
        ("GET", "/styles.css") => css_response(STYLES),
        ("GET", "/app.js") => js_response(APP_JS),
        ("GET", "/api/state") => {
            let state = state.lock().expect("state lock");
            json_response(&state_json(&state.entries))
        }
        ("POST", "/api/mood") => {
            let params = parse_form(&request.body);
            let mood = clean_field(params.get("mood").map(String::as_str).unwrap_or("bright"), 24);
            let color = clean_field(params.get("color").map(String::as_str).unwrap_or("#ffcf5a"), 16);
            let text = clean_field(params.get("text").map(String::as_str).unwrap_or(""), 140);
            add_entry(state, "mood", &mood, &color, &text)
        }
        ("POST", "/api/note") => {
            let params = parse_form(&request.body);
            let text = clean_field(params.get("text").map(String::as_str).unwrap_or(""), 180);
            let color = clean_field(params.get("color").map(String::as_str).unwrap_or("#7bdff2"), 16);
            add_entry(state, "note", "kind", &color, &text)
        }
        ("POST", "/api/quest") => {
            let params = parse_form(&request.body);
            let text = clean_field(params.get("text").map(String::as_str).unwrap_or("tiny win"), 120);
            add_entry(state, "quest", "done", "#c3f584", &text)
        }
        _ => not_found_response(),
    }
}

fn add_entry(state: Arc<Mutex<AppState>>, kind: &str, mood: &str, color: &str, text: &str) -> String {
    if text.trim().is_empty() && kind != "mood" {
        return error_response("A little text makes this bloom.");
    }

    let mut state = state.lock().expect("state lock");
    let entry = Entry {
        kind: kind.to_string(),
        mood: mood.to_string(),
        color: color.to_string(),
        text: text.to_string(),
        ts: now(),
    };

    if let Err(err) = append_entry(&state.data_path, &entry) {
        return server_error_response(&format!("Could not save entry: {err}"));
    }

    state.entries.push(entry);
    if state.entries.len() > 500 {
        let excess = state.entries.len() - 500;
        state.entries.drain(0..excess);
    }

    json_response(&state_json(&state.entries))
}

fn render_home(state: &AppState) -> String {
    let stats = stats(&state.entries);
    let note = latest_note(&state.entries).map(|note| escape_html(&note)).unwrap_or_else(|| {
        "Leave a kind note and the next person gets to find it.".to_string()
    });
    let quest = quest_for_day();
    let petals = garden_petals(&state.entries);

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Spark Garden</title>
  <link rel="stylesheet" href="/styles.css">
</head>
<body>
  <main class="shell">
    <section class="hero">
      <div class="hero-copy">
        <p class="kicker">No accounts. No ads. Just a tiny shared ritual.</p>
        <h1>Spark Garden</h1>
        <p class="subtitle">Plant today's mood, finish one small quest, and leave a kind note for whoever arrives next.</p>
      </div>
      <div class="garden" id="garden" aria-label="A living garden built from recent moods">
        {petals}
      </div>
    </section>

    <section class="panels" aria-label="Daily actions">
      <form class="panel mood-panel" data-action="/api/mood">
        <div>
          <p class="eyebrow">Plant a mood</p>
          <h2>How's your weather?</h2>
        </div>
        <div class="mood-grid">
          <button name="mood" value="bright" data-color="#ffcf5a" type="submit">Sunny</button>
          <button name="mood" value="soft" data-color="#f7a8b8" type="submit">Soft</button>
          <button name="mood" value="wild" data-color="#7bdff2" type="submit">Wild</button>
          <button name="mood" value="quiet" data-color="#b8b5ff" type="submit">Quiet</button>
        </div>
        <input type="hidden" name="color" value="#ffcf5a">
        <label>
          <span>Optional note</span>
          <input name="text" maxlength="140" placeholder="A sentence for future you">
        </label>
      </form>

      <form class="panel quest-panel" data-action="/api/quest">
        <div>
          <p class="eyebrow">Tiny quest</p>
          <h2>{quest}</h2>
        </div>
        <input type="hidden" name="text" value="{quest}">
        <button class="primary" type="submit">I did this</button>
      </form>

      <form class="panel note-panel" data-action="/api/note">
        <div>
          <p class="eyebrow">Message bottle</p>
          <h2>For the next visitor</h2>
        </div>
        <blockquote id="note">"{note}"</blockquote>
        <label>
          <span>Your note</span>
          <textarea name="text" maxlength="180" placeholder="Something true, kind, or oddly specific"></textarea>
        </label>
        <input type="hidden" name="color" value="#7bdff2">
        <button class="primary" type="submit">Float it forward</button>
      </form>
    </section>

    <section class="pulse" aria-label="Garden pulse">
      <div><strong id="moods">{}</strong><span>moods planted</span></div>
      <div><strong id="quests">{}</strong><span>tiny quests done</span></div>
      <div><strong id="notes">{}</strong><span>notes shared</span></div>
    </section>
  </main>
  <script src="/app.js"></script>
</body>
</html>"#,
        stats.moods, stats.quests, stats.notes
    )
}

#[derive(Default)]
struct Stats {
    moods: usize,
    notes: usize,
    quests: usize,
}

fn stats(entries: &[Entry]) -> Stats {
    let mut stats = Stats::default();
    for entry in entries {
        match entry.kind.as_str() {
            "mood" => stats.moods += 1,
            "note" => stats.notes += 1,
            "quest" => stats.quests += 1,
            _ => {}
        }
    }
    stats
}

fn latest_note(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.kind == "note" && !entry.text.trim().is_empty())
        .map(|entry| entry.text.clone())
}

fn garden_petals(entries: &[Entry]) -> String {
    let mut html = String::new();
    let moods: Vec<&Entry> = entries.iter().filter(|entry| entry.kind == "mood").rev().take(42).collect();

    if moods.is_empty() {
        for i in 0..18 {
            let x = 8 + (i * 19) % 84;
            let y = 14 + (i * 29) % 72;
            let delay = (i % 7) as f32 * 0.18;
            html.push_str(&format!(
                r#"<span class="petal ghost" style="--x:{}%;--y:{}%;--c:#d8f3dc;--d:{}s"></span>"#,
                x, y, delay
            ));
        }
        return html;
    }

    for (i, entry) in moods.iter().enumerate() {
        let x = 7 + (i * 17 + entry.ts as usize % 11) % 86;
        let y = 10 + (i * 23 + entry.ts as usize % 13) % 76;
        let size = 18 + (entry.mood.len() * 3 + i) % 28;
        let delay = (i % 9) as f32 * 0.12;
        html.push_str(&format!(
            r#"<span class="petal" title="{}" style="--x:{}%;--y:{}%;--s:{}px;--c:{};--d:{}s"></span>"#,
            escape_html(&entry.mood),
            x,
            y,
            size,
            escape_html(&entry.color),
            delay
        ));
    }
    html
}

fn quest_for_day() -> &'static str {
    const QUESTS: &[&str] = &[
        "Send one sincere compliment",
        "Drink water like you mean it",
        "Take a ten minute no-phone walk",
        "Play one song and move around",
        "Text someone a tiny thank-you",
        "Put one annoying thing back where it belongs",
        "Look outside until you notice three colors",
        "Write one sentence you want to remember",
    ];
    let day = now() / 86_400;
    QUESTS[(day as usize) % QUESTS.len()]
}

fn load_entries(path: &Path) -> Vec<Entry> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    contents
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 5 {
                return None;
            }
            Some(Entry {
                ts: parts[0].parse().ok()?,
                kind: unescape_tsv(parts[1]),
                mood: unescape_tsv(parts[2]),
                color: unescape_tsv(parts[3]),
                text: unescape_tsv(parts[4]),
            })
        })
        .collect()
}

fn append_entry(path: &Path, entry: &Entry) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        file,
        "{}\t{}\t{}\t{}\t{}",
        entry.ts,
        escape_tsv(&entry.kind),
        escape_tsv(&entry.mood),
        escape_tsv(&entry.color),
        escape_tsv(&entry.text)
    )
}

fn parse_form(body: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in body.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = url_decode(parts.next().unwrap_or_default());
        let value = url_decode(parts.next().unwrap_or_default());
        if !key.is_empty() {
            map.insert(key, value);
        }
    }
    map
}

fn clean_field(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .take(max_chars)
        .collect()
}

fn url_decode(value: &str) -> String {
    let mut out = Vec::new();
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = bytes.next().and_then(hex);
                let lo = bytes.next().and_then(hex);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn state_json(entries: &[Entry]) -> String {
    let stats = stats(entries);
    let note = latest_note(entries).unwrap_or_default();
    let petals = garden_petals(entries);
    format!(
        r#"{{"moods":{},"notes":{},"quests":{},"note":"{}","garden":"{}"}}"#,
        stats.moods,
        stats.notes,
        stats.quests,
        escape_json(&note),
        escape_json(&petals)
    )
}

fn html_response(body: &str) -> String {
    response("200 OK", "text/html; charset=utf-8", body)
}

fn css_response(body: &str) -> String {
    response("200 OK", "text/css; charset=utf-8", body)
}

fn js_response(body: &str) -> String {
    response("200 OK", "text/javascript; charset=utf-8", body)
}

fn json_response(body: &str) -> String {
    response("200 OK", "application/json; charset=utf-8", body)
}

fn error_response(message: &str) -> String {
    response(
        "422 Unprocessable Entity",
        "application/json; charset=utf-8",
        &format!(r#"{{"error":"{}"}}"#, escape_json(message)),
    )
}

fn server_error_response(message: &str) -> String {
    response(
        "500 Internal Server Error",
        "application/json; charset=utf-8",
        &format!(r#"{{"error":"{}"}}"#, escape_json(message)),
    )
}

fn not_found_response() -> String {
    response("404 Not Found", "text/plain; charset=utf-8", "Not found")
}

fn response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.as_bytes().len()
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_json(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

fn escape_tsv(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn unescape_tsv(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

const APP_JS: &str = r#"
const garden = document.querySelector('#garden');
const moodCount = document.querySelector('#moods');
const noteCount = document.querySelector('#notes');
const questCount = document.querySelector('#quests');
const note = document.querySelector('#note');

function update(data) {
  moodCount.textContent = data.moods;
  noteCount.textContent = data.notes;
  questCount.textContent = data.quests;
  if (data.note) note.textContent = `"${data.note}"`;
  garden.innerHTML = data.garden;
}

document.querySelectorAll('form[data-action]').forEach((form) => {
  form.addEventListener('click', (event) => {
    const button = event.target.closest('button[data-color]');
    if (!button) return;
    form.querySelector('input[name="color"]').value = button.dataset.color;
  });

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const submitter = event.submitter;
    const body = new URLSearchParams(new FormData(form));
    if (submitter && submitter.name) body.set(submitter.name, submitter.value);

    form.classList.add('saving');
    const response = await fetch(form.dataset.action, { method: 'POST', body });
    const data = await response.json();
    form.classList.remove('saving');
    if (data.error) {
      form.animate([{ transform: 'translateX(-4px)' }, { transform: 'translateX(4px)' }, { transform: 'translateX(0)' }], 180);
      return;
    }
    update(data);
    form.reset();
  });
});
"#;

const STYLES: &str = r#"
:root {
  color-scheme: light;
  --ink: #202124;
  --muted: #6b6862;
  --paper: #fffaf0;
  --line: rgba(32, 33, 36, .14);
  --green: #2f7d5c;
  --blue: #276b96;
  --rose: #c75173;
  --gold: #d7951d;
}

* { box-sizing: border-box; }

body {
  margin: 0;
  min-height: 100vh;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  color: var(--ink);
  background:
    radial-gradient(circle at 20% 10%, rgba(255, 207, 90, .28), transparent 30%),
    radial-gradient(circle at 88% 12%, rgba(123, 223, 242, .24), transparent 28%),
    linear-gradient(135deg, #fffaf0 0%, #f6fbf4 48%, #eef7ff 100%);
}

button, input, textarea {
  font: inherit;
}

.shell {
  width: min(1180px, calc(100% - 32px));
  margin: 0 auto;
  padding: 32px 0;
}

.hero {
  min-height: 52vh;
  display: grid;
  grid-template-columns: minmax(0, .85fr) minmax(320px, 1.15fr);
  gap: 28px;
  align-items: stretch;
}

.hero-copy {
  display: flex;
  flex-direction: column;
  justify-content: center;
}

.kicker, .eyebrow {
  margin: 0 0 10px;
  color: var(--green);
  font-size: .78rem;
  font-weight: 800;
  letter-spacing: 0;
  text-transform: uppercase;
}

h1 {
  margin: 0;
  font-family: ui-serif, Georgia, Cambria, "Times New Roman", serif;
  font-size: clamp(4rem, 9vw, 8.8rem);
  line-height: .86;
  letter-spacing: 0;
}

.subtitle {
  max-width: 40rem;
  margin: 24px 0 0;
  color: #4d4942;
  font-size: clamp(1.05rem, 2vw, 1.35rem);
  line-height: 1.55;
}

.garden {
  position: relative;
  min-height: 420px;
  overflow: hidden;
  border: 1px solid var(--line);
  border-radius: 8px;
  background:
    linear-gradient(180deg, rgba(255,255,255,.64), rgba(255,255,255,.2)),
    repeating-linear-gradient(90deg, rgba(47, 125, 92, .08) 0 1px, transparent 1px 72px),
    linear-gradient(180deg, #dff6ff 0%, #fbffe8 54%, #d7f0c4 100%);
  box-shadow: 0 22px 70px rgba(32, 33, 36, .12);
}

.garden::before {
  content: "";
  position: absolute;
  inset: auto 0 0;
  height: 35%;
  background: linear-gradient(180deg, transparent, rgba(47, 125, 92, .28));
}

.petal {
  position: absolute;
  left: var(--x);
  top: var(--y);
  width: var(--s, 28px);
  aspect-ratio: 1;
  border-radius: 55% 45% 55% 45%;
  background: var(--c);
  box-shadow: 0 10px 20px rgba(32,33,36,.16), inset -5px -7px 0 rgba(0,0,0,.08);
  transform: rotate(20deg);
  animation: bob 3.4s ease-in-out infinite;
  animation-delay: var(--d);
}

.petal::after {
  content: "";
  position: absolute;
  left: 48%;
  top: 75%;
  width: 2px;
  height: 62px;
  background: rgba(47, 125, 92, .5);
  transform: rotate(-18deg);
  transform-origin: top;
  z-index: -1;
}

.petal.ghost {
  opacity: .45;
  filter: saturate(.65);
}

@keyframes bob {
  50% { translate: 0 -9px; rotate: 8deg; }
}

.panels {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr;
  gap: 16px;
  margin-top: 18px;
}

.panel {
  min-height: 330px;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  gap: 18px;
  padding: 22px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: rgba(255,255,255,.66);
  box-shadow: 0 14px 44px rgba(32, 33, 36, .08);
  backdrop-filter: blur(14px);
}

.panel h2 {
  margin: 0;
  font-size: 1.55rem;
  line-height: 1.1;
  letter-spacing: 0;
}

.mood-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.mood-grid button, .primary {
  min-height: 48px;
  border: 1px solid rgba(32,33,36,.18);
  border-radius: 8px;
  color: #171717;
  background: #ffffff;
  cursor: pointer;
  transition: transform .16s ease, box-shadow .16s ease, background .16s ease;
}

.mood-grid button:nth-child(1) { background: #ffdf7d; }
.mood-grid button:nth-child(2) { background: #ffc4d1; }
.mood-grid button:nth-child(3) { background: #a7edf8; }
.mood-grid button:nth-child(4) { background: #cbc8ff; }

.mood-grid button:hover, .primary:hover {
  transform: translateY(-2px);
  box-shadow: 0 10px 22px rgba(32,33,36,.13);
}

.primary {
  width: 100%;
  color: white;
  background: #202124;
}

label {
  display: grid;
  gap: 8px;
  color: var(--muted);
  font-size: .9rem;
}

input, textarea {
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 12px 13px;
  color: var(--ink);
  background: rgba(255,255,255,.78);
  outline: none;
}

textarea {
  min-height: 92px;
  resize: vertical;
}

input:focus, textarea:focus {
  border-color: rgba(39, 107, 150, .72);
  box-shadow: 0 0 0 4px rgba(123, 223, 242, .24);
}

blockquote {
  margin: 0;
  padding: 16px;
  border-left: 4px solid var(--rose);
  color: #413d37;
  background: rgba(255,255,255,.58);
  line-height: 1.5;
}

.pulse {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 14px;
  margin-top: 16px;
}

.pulse div {
  padding: 18px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: rgba(255,255,255,.52);
}

.pulse strong {
  display: block;
  font-size: 2.4rem;
  line-height: 1;
}

.pulse span {
  color: var(--muted);
}

.saving {
  opacity: .72;
  pointer-events: none;
}

@media (max-width: 900px) {
  .hero, .panels, .pulse {
    grid-template-columns: 1fr;
  }

  .hero {
    min-height: auto;
  }

  .garden {
    min-height: 360px;
  }
}

@media (max-width: 520px) {
  .shell {
    width: min(100% - 20px, 1180px);
    padding: 18px 0;
  }

  h1 {
    font-size: 4rem;
  }

  .panel {
    min-height: 300px;
    padding: 18px;
  }

  .mood-grid {
    grid-template-columns: 1fr;
  }
}
"#;
