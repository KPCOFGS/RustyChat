use dioxus::prelude::*;
use reqwest::Client;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use uuid::Uuid;

const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!("/assets/main.css");

// Maximum number of messages to keep / load per chat (history limit)
const MAX_HISTORY_MESSAGES: i64 = 10000;
// Maximum title length for chat rename
const MAX_TITLE_LEN: usize = 255;

fn main() {
    dioxus::launch(App);
}

/* ================= DATABASE ================= */

fn init_db() -> Connection {
    let conn = Connection::open("chat.db").unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS chats (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            model TEXT NOT NULL,
            system_prompt TEXT,
            temperature REAL,
            top_p REAL,
            max_tokens INTEGER,
            zoom INTEGER,
            maximized INTEGER,
            window_width INTEGER,
            window_height INTEGER
        )",
        [],
    )
    .unwrap();

    let exists: bool = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM settings WHERE id = 1)")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap_or(false);

    if !exists {
        conn.execute(
            "INSERT INTO settings (id, model, system_prompt, temperature, top_p, max_tokens, zoom, maximized, window_width, window_height)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "", // no default model — user must pick one
                "",
                0.7_f64,
                0.95_f64,
                512_i32,
                100_i32, // zoom %
                1_i32,   // maximized true by default (kept in DB, but user cannot change)
                1024_i32,
                768_i32
            ],
        )
        .unwrap();
    }

    conn
}

// clamp helper to ensure DB integer values respect Rust i32 bounds
fn clamp_to_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

#[derive(Clone, Debug)]
struct Settings {
    model: String,
    system_prompt: String,
    temperature: f64,
    top_p: f64,
    max_tokens: i32,
    zoom: i32,
    maximized: bool,
    window_width: i32,
    window_height: i32,
}

fn load_settings(conn: &Connection) -> Settings {
    conn.query_row(
        "SELECT model, system_prompt, temperature, top_p, max_tokens, zoom, maximized, window_width, window_height FROM settings WHERE id = 1",
        [],
        |row: &Row| {
            Ok(Settings {
                model: row.get::<_, String>(0)?,
                system_prompt: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                temperature: row.get::<_, Option<f64>>(2)?.unwrap_or(0.7),
                top_p: row.get::<_, Option<f64>>(3)?.unwrap_or(0.95),
                max_tokens: clamp_to_i32(row.get::<_, Option<i64>>(4)?.unwrap_or(512)),
                zoom: clamp_to_i32(row.get::<_, Option<i64>>(5)?.unwrap_or(100)),
                // always treat maximized as true on start (we still read DB value for compatibility)
                maximized: true,
                window_width: clamp_to_i32(row.get::<_, Option<i64>>(7)?.unwrap_or(1024)),
                window_height: clamp_to_i32(row.get::<_, Option<i64>>(8)?.unwrap_or(768)),
            })
        },
    )
    .unwrap()
}

fn save_settings(conn: &Connection, s: &Settings) {
    // ensure fields are within i32 bounds
    let max_tokens: i64 = s.max_tokens.into();
    let zoom: i64 = s.zoom.into();
    let width: i64 = s.window_width.into();
    let height: i64 = s.window_height.into();

    conn.execute(
        "UPDATE settings SET model = ?1, system_prompt = ?2, temperature = ?3, top_p = ?4, max_tokens = ?5, zoom = ?6, maximized = ?7, window_width = ?8, window_height = ?9 WHERE id = 1",
        params![
            s.model,
            s.system_prompt,
            s.temperature,
            s.top_p,
            clamp_to_i32(max_tokens),
            clamp_to_i32(zoom),
            if s.maximized { 1 } else { 0 },
            clamp_to_i32(width),
            clamp_to_i32(height)
        ],
    )
    .unwrap();
}

/* Helper to enforce history length in DB per chat - deletes oldest messages beyond MAX_HISTORY_MESSAGES */
fn enforce_history_limit(conn: &Connection, chat_id: &str) {
    // count messages first
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id = ?1",
            params![chat_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if count <= MAX_HISTORY_MESSAGES {
        return;
    }

    // get cutoff id (the id at position MAX_HISTORY_MESSAGES from newest)
    if let Ok(cutoff_id) = conn.query_row(
        "SELECT id FROM messages WHERE chat_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2",
        params![chat_id, MAX_HISTORY_MESSAGES - 1],
        |r| r.get::<_, i64>(0),
    ) {
        let _ = conn.execute(
            "DELETE FROM messages WHERE chat_id = ?1 AND id <= ?2",
            params![chat_id, cutoff_id],
        );
    }
}

/* ================= SETTINGS MODAL (moved above App to ensure it's in scope) ================= */

#[component]
fn SettingsModal(
    settings: Signal<Settings>,
    show_settings: Signal<bool>,
    chats: Signal<Vec<(String, String)>>,
    messages: Signal<Vec<(String, String)>>,
    current_chat_id: Signal<Option<String>>,
) -> Element {
    // local editable copies using signals
    let mut local_model = use_signal(|| settings().model.clone());
    let mut local_system = use_signal(|| settings().system_prompt.clone());
    let mut local_temp = use_signal(|| settings().temperature);
    let mut local_top_p = use_signal(|| settings().top_p);
    let mut local_max_tokens = use_signal(|| settings().max_tokens);
    let mut local_zoom = use_signal(|| settings().zoom);
    let local_width = use_signal(|| settings().window_width);
    let local_height = use_signal(|| settings().window_height);

    // list of available models from Ollama
    let available_models = use_signal(|| Vec::<String>::new());

    // fetch available models when modal mounts
    {
        let mut models_sig = available_models.clone();
        use_effect(move || {
            spawn(async move {
                let client = Client::new();
                // Try the common Ollama models endpoint; tolerate different shapes.
                let url = "http://localhost:11434/api/tags";
                if let Ok(resp) = client.get(url).send().await {
                    if let Ok(json) = resp.json::<Value>().await {
                        let mut names: Vec<String> = Vec::new();

                        // Newer Ollama returns {"models":[{...}]}
                        if let Some(models_arr) = json.get("models").and_then(|v| v.as_array()) {
                            for item in models_arr {
                                if let Some(m) = item
                                    .get("model")
                                    .or(item.get("name"))
                                    .and_then(|v| v.as_str())
                                {
                                    names.push(m.to_string());
                                }
                            }
                        } else if let Some(arr) = json.as_array() {
                            // older shape: plain array
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    names.push(s.to_string());
                                } else if let Some(n) = item.get("name").and_then(|v| v.as_str()) {
                                    names.push(n.to_string());
                                } else if let Some(n) = item.get("model").and_then(|v| v.as_str()) {
                                    names.push(n.to_string());
                                }
                            }
                        }

                        // dedupe preserving order
                        let mut seen = std::collections::HashSet::new();
                        names.retain(|n| seen.insert(n.clone()));

                        models_sig.set(names);
                    }
                }
            });

            // no cleanup required
        });
    }

    // When the settings modal opens, ensure local edit fields reflect the persisted settings.
    {
        let show_settings_sig = show_settings.clone();
        let settings_sig = settings.clone();
        let mut local_model_sig = local_model.clone();
        let mut local_system_sig = local_system.clone();
        let mut local_temp_sig = local_temp.clone();
        let mut local_top_p_sig = local_top_p.clone();
        let mut local_max_tokens_sig = local_max_tokens.clone();
        let mut local_zoom_sig = local_zoom.clone();
        let mut local_width_sig = local_width.clone();
        let mut local_height_sig = local_height.clone();
        use_effect(move || {
            if show_settings_sig() {
                let s = settings_sig();
                local_model_sig.set(s.model.clone());
                local_system_sig.set(s.system_prompt.clone());
                local_temp_sig.set(s.temperature);
                local_top_p_sig.set(s.top_p);
                local_max_tokens_sig.set(s.max_tokens);
                local_zoom_sig.set(s.zoom);
                local_width_sig.set(s.window_width);
                local_height_sig.set(s.window_height);
            }
        });
    }

    // build a local options list that includes the persisted model (so it displays as selected)
    let options_vec = {
        let mut v = available_models().clone();
        let selected = local_model().clone();
        if !selected.is_empty() && !v.iter().any(|s| s == &selected) {
            // put selected at the top so it is visible in the dropdown and selectable
            v.insert(0, selected);
        }
        v
    };

    let apply = {
        to_owned![
            local_model,
            local_system,
            local_temp,
            local_top_p,
            local_max_tokens,
            local_zoom,
            local_width,
            local_height,
            settings,
            show_settings
        ];
        move |_| {
            // ensure integer fields are clamped to i32
            let mut model_str = local_model().clone();
            // sanitize model string and trim
            model_str = model_str.trim().to_string();

            let new_settings = Settings {
                model: model_str,
                system_prompt: local_system().clone(),
                temperature: local_temp(),
                top_p: local_top_p(),
                max_tokens: clamp_to_i32(local_max_tokens().into()),
                zoom: clamp_to_i32(local_zoom().into()),
                // always start maximized; user cannot change this setting in the UI
                maximized: true,
                window_width: clamp_to_i32(local_width().into()),
                window_height: clamp_to_i32(local_height().into()),
            };
            let conn = init_db();
            save_settings(&conn, &new_settings);
            settings.set(new_settings);
            show_settings.set(false);
        }
    };

    let delete_all = {
        to_owned![chats, messages, current_chat_id, show_settings];
        move |_| {
            let conn = init_db();
            conn.execute("DELETE FROM messages", []).ok();
            conn.execute("DELETE FROM chats", []).ok();

            chats.set(vec![]);
            messages.set(vec![]);
            current_chat_id.set(None);
            show_settings.set(false);
        }
    };

    let cancel = {
        to_owned![show_settings];
        move |_| {
            show_settings.set(false);
        }
    };

    rsx! {
        div { class: "settings-overlay",
            div { class: "settings-modal",
                h3 { "Settings" }

                label { "Model (choose one of the available Ollama models)" }
                select {
                    class: "input",
                    // bind visible value to the local signal (so what the user sees is the persisted/selected model)
                    value: "{local_model}",
                    onchange: move |e| local_model.set(e.value()),
                    // explicit default option that will be selected when local_model is empty
                    option { selected: local_model().is_empty(), value: "", "- Select a model -" }
                    // render options and explicitly mark the selected option so the browser doesn't fallback to the first option
                    {options_vec.iter().map(|m| rsx!( option { selected: (m == &local_model()), value: "{m}", "{m}" } ))}
                }

                // show a brief warning if model is empty
                if local_model().is_empty() {
                    p { class: "dim-text warning-text", "No model selected - pick a model to allow sending messages." }
                }

                label { "System prompt (optional)" }
                textarea {
                    class: "textarea",
                    value: "{local_system}",
                    oninput: move |e| local_system.set(e.value()),
                }

                label { "Temperature" }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.05",
                    min: "0.0",
                    max: "2.0",
                    value: "{local_temp}",
                    oninput: move |e| local_temp.set(e.value().parse::<f64>().unwrap_or(0.7))
                }

                label { "Top-p" }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.01",
                    min: "0.0",
                    max: "1.0",
                    value: "{local_top_p}",
                    oninput: move |e| local_top_p.set(e.value().parse::<f64>().unwrap_or(0.95))
                }

                label { "Max tokens (clamped to Rust/i32 limits)" }
                input {
                    class: "input",
                    r#type: "number",
                    step: "1",
                    min: "1",
                    max: { format!("{}", i32::MAX) },
                    value: "{local_max_tokens}",
                    oninput: move |e| {
                        let parsed = e.value().parse::<i64>().unwrap_or(512);
                        local_max_tokens.set(clamp_to_i32(parsed));
                    }
                }

                label { "Zoom (%) — applied globally (50 - 200)" }
                div { class: "zoom-row",
                    button { onclick: move |_| { local_zoom.set((local_zoom() - 10).max(50)); }, "−" }
                    span { "{local_zoom}%" }
                    button { onclick: move |_| { local_zoom.set((local_zoom() + 10).min(200)); }, "+" }
                }

                /* Window behavior removed from UI — always starts maximized */

                div { class: "modal-actions",
                    button { onclick: apply, "Apply" }
                    button { onclick: delete_all, class: "delete-all", "Delete All History" }
                    button { onclick: cancel, "Cancel" }
                }
            }
        }
    }
}

/* ================= APP ================= */

#[component]
fn App() -> Element {
    let conn = init_db();

    let chats = use_signal(|| Vec::<(String, String)>::new());
    let current_chat_id = use_signal(|| Option::<String>::None);
    let messages = use_signal(|| Vec::<(String, String)>::new());

    // settings and modal visibility
    let settings = use_signal(|| load_settings(&conn));
    let show_settings = use_signal(|| false);

    // load chats once
    {
        let mut chats = chats.clone();
        use_effect(move || {
            let conn = init_db();
            let mut stmt = conn.prepare("SELECT id, title FROM chats").unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap();

            chats.set(rows.map(|r| r.unwrap()).collect());
        });
    }

    // always start maximized; user cannot change this in UI
    let container_style = "width: 100vw; height: 100vh;".to_string();

    // apply zoom using CSS 'zoom' so layout doesn't create blank transform area
    let zoom_style = format!("zoom: {}%;", settings().zoom);

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: MAIN_CSS }

        div { class: "outer-wrapper", style: "{container_style}",
            div { class: "app-container", style: "{zoom_style}",
                Sidebar {
                    chats: chats.clone(),
                    current_chat_id: current_chat_id.clone(),
                    messages: messages.clone(),
                    show_settings: show_settings.clone()
                }
                ChatWindow {
                    current_chat_id: current_chat_id.clone(),
                    messages: messages.clone(),
                    settings: settings.clone(),
                    chats: chats.clone() // pass chats so header can show title
                }
            }

            if show_settings() {
                SettingsModal {
                    settings: settings.clone(),
                    show_settings: show_settings.clone(),
                    chats: chats.clone(),
                    messages: messages.clone(),
                    current_chat_id: current_chat_id.clone()
                }
            }
        }
    }
}

/* ================= SIDEBAR ================= */

#[component]
fn Sidebar(
    chats: Signal<Vec<(String, String)>>,
    current_chat_id: Signal<Option<String>>,
    messages: Signal<Vec<(String, String)>>,
    show_settings: Signal<bool>,
) -> Element {
    // state for inline renaming
    let mut editing_chat = use_signal(|| Option::<String>::None);
    let mut edit_text = use_signal(|| "".to_string());

    rsx! {
        div { class: "sidebar",
            h1 { class: "logo", "RustyChat" }

            button {
                class: "new-chat-btn big",
                onclick: move |_| {
                    let conn = init_db();
                    let new_id = Uuid::new_v4().to_string();
                    // on the surface all chats share the same visible name "New Chat"
                    let title = "New Chat".to_string();

                    conn.execute(
                        "INSERT INTO chats (id, title) VALUES (?1, ?2)",
                        params![new_id, title],
                    ).unwrap();

                    chats.push((new_id.clone(), title));
                    current_chat_id.set(Some(new_id));
                    messages.set(vec![]);
                },
                "➕ New Chat"
            }

            div { class: "chat-list",
                {chats().iter().map(|(id, title)| {
                    // clone once from the iterator values
                    let id_owned = id.clone();
                    let title_clone = title.clone();

                    // create separate clones for each closure so none of them move a shared variable
                    let id_for_open = id_owned.clone();
                    let id_for_save = id_owned.clone();
                    let id_for_rename_btn = id_owned.clone();
                    let id_for_delete = id_owned.clone();

                    // handles
                    let mut chats_handle = chats.clone();
                    let mut messages_handle = messages.clone();
                    let mut current_chat_handle = current_chat_id.clone();
                    let mut editing_chat_handle = editing_chat.clone();
                    let mut edit_text_handle = edit_text.clone();

                    rsx! {
                        div { class: "chat-item-row",
                            div {
                                class: "chat-item",
                                onclick: move |_| {
                                    // use the dedicated clone inside this closure
                                    let conn = init_db();
                                    // load only up to MAX_HISTORY_MESSAGES newest and then reverse to chronological order
                                    let mut stmt = conn.prepare(
                                        "SELECT role, content FROM messages
                                         WHERE chat_id = ? ORDER BY id DESC LIMIT ?"
                                    ).unwrap();

                                    let rows = stmt
                                        .query_map(params![&id_for_open, MAX_HISTORY_MESSAGES], |row| {
                                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                                        })
                                        .unwrap();

                                    let mut collected: Vec<(String, String)> = rows.map(|r| r.unwrap()).collect();
                                    collected.reverse(); // chronological
                                    messages_handle.set(collected);
                                    current_chat_handle.set(Some(id_for_open.clone()));
                                },

                                /* Conditional: either show renaming input or the title, and place actions inline */
                                {
                                    if editing_chat_handle().as_ref().map(|c| c == &id_for_save).unwrap_or(false) {
                                        rsx! {
                                            div { class: "rename-row",
                                                input {
                                                    class: "rename-input",
                                                    value: "{edit_text_handle}",
                                                    oninput: move |e| {
                                                        // enforce title length limit in the UI while typing
                                                        let mut v = e.value();
                                                        if v.len() > MAX_TITLE_LEN {
                                                            v.truncate(MAX_TITLE_LEN);
                                                        }
                                                        edit_text_handle.set(v);
                                                    },
                                                }
                                                button {
                                                    class: "rename-save",
                                                    onclick: move |_| {
                                                        let mut new_title = edit_text_handle().clone();
                                                        if new_title.len() > MAX_TITLE_LEN {
                                                            new_title.truncate(MAX_TITLE_LEN);
                                                        }
                                                        let trimmed = new_title;

                                                        let conn = init_db();
                                                        conn.execute(
                                                            "UPDATE chats SET title = ?1 WHERE id = ?2",
                                                            params![trimmed, id_for_save.clone()],
                                                        ).unwrap();

                                                        // update in-memory list — compare by reference to avoid moving id_for_save
                                                        chats_handle.set(
                                                            chats_handle().into_iter().map(|(cid, t)| {
                                                                if cid == id_for_save { (cid, trimmed.clone()) } else { (cid, t) }
                                                            }).collect()
                                                        );

                                                        editing_chat_handle.set(None);
                                                    },
                                                    "Save"
                                                }
                                                button {
                                                    class: "rename-cancel",
                                                    onclick: move |_| {
                                                        editing_chat_handle.set(None);
                                                    },
                                                    "Cancel"
                                                }
                                            }
                                        }
                                    } else {
                                        rsx! {
                                            Fragment {
                                                div { class: "chat-title", "{title_clone}" }
                                                div { class: "chat-actions",
                                                    button {
                                                        class: "rename-btn",
                                                        onclick: move |e| {
                                                            // stop propagation so clicking rename doesn't open the chat
                                                            e.stop_propagation();
                                                            editing_chat.set(Some(id_for_rename_btn.clone()));
                                                            // clamp initial edit text as well
                                                            let mut init = title_clone.clone();
                                                            if init.len() > MAX_TITLE_LEN {
                                                                init.truncate(MAX_TITLE_LEN);
                                                            }
                                                            edit_text.set(init);
                                                        },
                                                        "Rename"
                                                    }
                                                    button {
                                                        class: "delete-chat-btn big",
                                                        onclick: move |e| {
                                                            e.stop_propagation();
                                                            let conn = init_db();

                                                            conn.execute(
                                                                "DELETE FROM messages WHERE chat_id = ?1",
                                                                params![id_for_delete.clone()],
                                                            ).unwrap();

                                                            conn.execute(
                                                                "DELETE FROM chats WHERE id = ?1",
                                                                params![id_for_delete.clone()],
                                                            ).unwrap();

                                                            chats_handle.set(
                                                                chats_handle()
                                                                    .into_iter()
                                                                    .filter(|(cid, _)| cid != &id_for_delete)
                                                                    .collect()
                                                            );

                                                            if current_chat_handle() == Some(id_for_delete.clone()) {
                                                                current_chat_handle.set(None);
                                                                messages_handle.set(vec![]);
                                                            }
                                                        },
                                                        "Delete"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                })}
            }

            // Footer inside the sidebar (bottom-left area)
            div { class: "sidebar-footer",
                button {
                    class: "settings-btn big",
                    onclick: move |_| {
                        show_settings.set(!show_settings());
                    },
                    // icon + tooltip text shown on hover via CSS
                    span { class: "settings-icon", "⚙️" }
                    span { class: "settings-tooltip", "Settings" }
                }
                a { class: "repo-icon", href: "https://github.com/your-username/your-repo", target: "_blank", title: "click here to see the repository", "🔗" }
            }
        }
    }
}

/* ================= OLLAMA API STRUCTURES ================= */

#[derive(Serialize, Deserialize, Debug)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    #[serde(default = "default_stream")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Value>,
}

fn default_stream() -> bool {
    false
}

#[derive(Serialize, Deserialize, Debug)]
struct OllamaChatResponse {
    message: OllamaMessage,
    done: bool,
    #[serde(default)]
    response: String,
}

/* ================= CHAT WINDOW ================= */

#[component]
fn ChatWindow(
    current_chat_id: Signal<Option<String>>,
    messages: Signal<Vec<(String, String)>>,
    settings: Signal<Settings>,
    chats: Signal<Vec<(String, String)>>,
) -> Element {
    let mut input_text = use_signal(|| "".to_string());
    // track which chat (id) is currently producing a response (thinking)
    let mut loading_chat = use_signal(|| Option::<String>::None);
    // cancellation flag for the current in-flight request (if any)
    let mut current_cancel = use_signal(|| Option::<Arc<AtomicBool>>::None);
    let http_client = use_signal(|| Client::new());

    // compute header title outside rsx! to avoid let-binding in the macro context
    let header_title = {
        if let Some(id) = current_chat_id() {
            // find title from chats signal, fallback to id
            chats()
                .iter()
                .find(|(cid, _)| cid == &id)
                .map(|(_, t)| t.clone())
                .unwrap_or(id.clone())
        } else {
            "No Chat Selected".to_string()
        }
    };

    // compute model display for the header (show friendly notice when empty)
    let model_display = {
        let m = settings().model.clone();
        if m.trim().is_empty() {
            "No model selected".to_string()
        } else {
            m
        }
    };

    // send_to_ollama now respects a per-request cancellation flag and updates loading_chat/current_cancel
    let send_to_ollama = {
        // include current_chat_id so the async task can check whether the user is currently viewing the target chat
        to_owned![
            messages,
            http_client,
            loading_chat,
            current_cancel,
            current_chat_id
        ];
        move |chat_id: String,
              user_message: String,
              settings: Settings,
              cancel_flag: Arc<AtomicBool>| {
            async move {
                // If no model selected, inform the user and abort
                if settings.model.trim().is_empty() {
                    // store error in DB so it's visible when user returns to the chat
                    let conn = init_db();
                    let db_msg = "Error: No model selected. Please open Settings and choose a model before sending messages.";
                    conn.execute(
                        "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                        params![chat_id, db_msg],
                    ).ok();
                    enforce_history_limit(&conn, &chat_id);

                    // if user is currently viewing this chat, push into in-memory messages so it appears immediately
                    if current_chat_id()
                        .as_ref()
                        .map(|c| c == &chat_id)
                        .unwrap_or(false)
                    {
                        messages.push(("assistant".into(), db_msg.to_string()));
                    }

                    loading_chat.set(None);
                    current_cancel.set(None);
                    return;
                }

                let mut ollama_messages = Vec::new();

                if !settings.system_prompt.is_empty() {
                    ollama_messages.push(OllamaMessage {
                        role: "system".to_string(),
                        content: settings.system_prompt.clone(),
                    });
                }

                for (role, content) in messages().iter() {
                    ollama_messages.push(OllamaMessage {
                        role: role.clone(),
                        content: content.clone(),
                    });
                }

                ollama_messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: user_message.clone(),
                });

                let params_json = serde_json::json!({
                    "temperature": settings.temperature,
                    "top_p": settings.top_p,
                    "max_tokens": settings.max_tokens
                });

                let request = OllamaChatRequest {
                    model: settings.model.clone(),
                    messages: ollama_messages,
                    stream: false,
                    parameters: Some(params_json),
                };

                let ollama_url = "http://localhost:11434/api/chat";

                // perform request (we can't truly abort the underlying reqwest call easily here,
                // but we check the cancel_flag before committing the response into the chat)
                match http_client().post(ollama_url).json(&request).send().await {
                    Ok(response) => {
                        if response.status().is_success() {
                            match response.json::<OllamaChatResponse>().await {
                                Ok(api_response) => {
                                    // If cancelled, simply drop the response: do NOT insert DB message or push to UI.
                                    if cancel_flag.load(Ordering::Relaxed) {
                                        // no DB insert, no UI push — conversation just stops silently
                                    } else {
                                        // Normal success path: insert into DB first
                                        let conn = init_db();
                                        let _ = conn.execute(
                                            "INSERT INTO messages (chat_id, role, content)
                                             VALUES (?1, 'assistant', ?2)",
                                            params![chat_id, api_response.message.content],
                                        );
                                        enforce_history_limit(&conn, &chat_id);

                                        // Push into in-memory messages only if that chat is currently visible.
                                        if current_chat_id()
                                            .as_ref()
                                            .map(|c| c == &chat_id)
                                            .unwrap_or(false)
                                        {
                                            messages.push((
                                                "assistant".into(),
                                                api_response.message.content,
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Failed to parse Ollama response: {}", e);
                                    let err_text = "Error: Failed to parse response from Ollama";
                                    // store in DB
                                    let conn = init_db();
                                    let _ = conn.execute(
                                        "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                                        params![chat_id, err_text],
                                    );
                                    enforce_history_limit(&conn, &chat_id);

                                    if current_chat_id()
                                        .as_ref()
                                        .map(|c| c == &chat_id)
                                        .unwrap_or(false)
                                    {
                                        messages.push(("assistant".into(), err_text.to_string()));
                                    }
                                }
                            }
                        } else {
                            eprintln!("Ollama API error: {}", response.status());
                            let err_text =
                                format!("Error: Ollama API returned status {}", response.status());
                            let conn = init_db();
                            let _ = conn.execute(
                                "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                                params![chat_id, err_text],
                            );
                            enforce_history_limit(&conn, &chat_id);

                            if current_chat_id()
                                .as_ref()
                                .map(|c| c == &chat_id)
                                .unwrap_or(false)
                            {
                                messages.push((
                                    "assistant".into(),
                                    format!(
                                        "Error: Ollama API returned status {}",
                                        response.status()
                                    ),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to send request to Ollama: {}", e);
                        let err_text = "Error: Could not connect to Ollama. Make sure Ollama is running at http://localhost:11434";
                        let conn = init_db();
                        let _ = conn.execute(
                            "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                            params![chat_id, err_text],
                        );
                        enforce_history_limit(&conn, &chat_id);

                        if current_chat_id()
                            .as_ref()
                            .map(|c| c == &chat_id)
                            .unwrap_or(false)
                        {
                            messages.push(("assistant".into(), err_text.to_string()));
                        }
                    }
                }

                // clear loading and cancel flag (done for this request)
                loading_chat.set(None);
                current_cancel.set(None);
            }
        }
    };

    rsx! {
        div { class: "chat-window",

            div { class: "chat-header",
                h2 { "{header_title}" }
                // new model indicator under the chat title
                p { class: "model-indicator", "Model: {model_display}" }
            }

            div { class: "chat-messages",
                {messages().iter().map(|(role, content)| {
                    rsx! {
                        Message {
                            role: role.clone(),
                            content: content.clone()
                        }
                    }
                })}

                // show the "thinking" bubble only if the current chat is the one loading
                { if loading_chat().as_ref().map(|l| current_chat_id().as_ref().map(|c| c == l).unwrap_or(false)).unwrap_or(false) {
                    rsx! {
                        div { class: "message assistant-message loading-message",
                            p { "Thinking..." }
                            div { class: "loading-dots" }
                        }
                    }
                } else {
                    rsx!( Fragment {} )
                }}
            }

            div { class: "chat-input-area",
                textarea {
                    class: "chat-input",
                    placeholder: "Send a message...",
                    value: "{input_text}",
                    oninput: move |e| input_text.set(e.value()),
                    // disable input only for the chat that's currently loading (so user can switch to other chats)
                    disabled: loading_chat().as_ref().map(|l| current_chat_id().as_ref().map(|c| c == l).unwrap_or(false)).unwrap_or(false),
                }

                // If current chat has an in-flight request, show interrupt button
                { if current_chat_id().as_ref().and_then(|cid| loading_chat().as_ref().map(|l| if l == cid { Some(cid.clone()) } else { None })).flatten().is_some() {
                    rsx! {
                        button {
                            class: "interrupt-button big",
                            onclick: move |_| {
                                // set cancellation flag so the background task won't push the final reply
                                if let Some(cancel) = current_cancel() {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                                // immediately clear the UI loading indicator so the thinking bubble goes away
                                loading_chat.set(None);
                                // remove the stored cancel handle from signal (background task keeps its own Arc)
                                current_cancel.set(None);
                                // Do NOT push any "[Interrupted]" message — conversation simply stops.
                            },
                            "Interrupt"
                        }
                    }
                } else {
                    rsx!( Fragment {} )
                }}

                button {
                    class: "send-button big",
                    // disable send when some other chat is loading, or no current chat, or empty input
                    disabled: current_chat_id().is_none() ||
                              input_text().trim().is_empty() ||
                              loading_chat().as_ref().map(|l| current_chat_id().as_ref().map(|c| c != l).unwrap_or(false)).unwrap_or(false),
                    onclick: move |_| {
                        if let Some(chat_id) = current_chat_id() {
                            let text = input_text();

                            if text.trim().is_empty() {
                                return;
                            }

                            let conn = init_db();

                            // ensure we don't attempt to insert extremely long content: clamp to a reasonable max (e.g., 1_000_000 chars)
                            let mut user_text = text.clone();
                            const MAX_MESSAGE_LEN: usize = 1_000_000;
                            if user_text.len() > MAX_MESSAGE_LEN {
                                user_text.truncate(MAX_MESSAGE_LEN);
                            }

                            conn.execute(
                                "INSERT INTO messages (chat_id, role, content)
                                 VALUES (?1, 'user', ?2)",
                                params![chat_id, user_text.clone()],
                            ).unwrap();

                            // enforce history limit after user insert
                            enforce_history_limit(&conn, &chat_id);

                            // push the user's message into the visible messages buffer (it was the active chat when typed)
                            messages.push(("user".into(), user_text.clone()));
                            input_text.set("".to_string());

                            // prepare cancellation flag and mark which chat is loading
                            let cancel_flag = Arc::new(AtomicBool::new(false));
                            current_cancel.set(Some(cancel_flag.clone()));
                            loading_chat.set(Some(chat_id.clone()));

                            // spawn the request task with cancel_flag captured
                            spawn({
                                let chat_id = chat_id.clone();
                                let settings_snapshot = settings();
                                let cancel_flag = cancel_flag.clone();
                                send_to_ollama(chat_id, text, settings_snapshot, cancel_flag)
                            });
                        }
                    },
                    "➤ Send"
                }
            }
        }
    }
}

/* ================= MESSAGE ================= */

#[component]
fn Message(role: String, content: String) -> Element {
    let class_name = if role == "user" {
        "message user-message"
    } else {
        "message assistant-message"
    };

    if content.contains("<think>") && content.contains("</think>") {
        let think_start = content.find("<think>").unwrap() + "<think>".len();
        let think_end = content.find("</think>").unwrap();
        let think_content = &content[think_start..think_end].trim();

        let before_think = &content[..think_start - "<think>".len()];
        let after_think = &content[think_end + "</think>".len()..];

        rsx! {
            div { class: "{class_name}",
                {if !before_think.is_empty() {
                    rsx! { p { class: "dim-text", "{before_think}" } }
                } else {
                    rsx! { Fragment {} }
                }}

                div { class: "think-bubble",
                    p { class: "think-label", "🤔 Thinking..." }
                    div { class: "think-content dim-text",
                        "\n"
                        "{think_content}"
                        "\n"
                    }
                }

                {if !after_think.is_empty() {
                    rsx! { p { class: "dim-text", "{after_think}" } }
                } else {
                    rsx! { Fragment {} }
                }}
            }
        }
    } else {
        rsx! {
            div { class: "{class_name}",
                p { class: "dim-text", "{content}" }
            }
        }
    }
}
