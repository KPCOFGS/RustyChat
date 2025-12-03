# RustyChat

RustyChat is a lightweight, desktop chat UI written in Rust using Dioxus. It provides a simple, focused chat interface that stores conversation history in a local SQLite database while sending requests to a local Ollama backend for model inference. The UI is dark-themed, supports per-chat naming, renaming, persistent history, model selection, and an interrupt button to stop an in-flight model response.

This project is intended as a compact, privacy-friendly GUI wrapper around local model hosting via Ollama.

---

## Features

- Desktop chat UI built with Dioxus.
- Persistent history stored in `chat.db` (SQLite).
- Model selection populated from Ollama's `/api/tags` endpoint.
- Settings modal to configure model, system prompt, temperature, top_p, max_tokens, and zoom.
- Dark theme with careful styling and responsive layout.

---

## Powered by

- Dioxus — desktop RUST UI framework
- Ollama — local model hosting backend used for inference

If you haven't installed Ollama, please visit https://ollama.com/ and follow their installation instructions. The app expects Ollama to be available at:

- http://localhost:11434

You can test the model list with:

```bash
curl http://localhost:11434/api/tags
```

---

## Crates used and what they do

- dioxus (and dioxus-desktop): UI framework used to build the desktop application and components.
- reqwest: HTTP client used to call Ollama's REST API.
- rusqlite: SQLite bindings to persist chats and messages locally (`chat.db`).
- serde, serde_json: Serialization and Deserialization for JSON payloads exchanged with Ollama and for internal data flows.
- uuid: Generate UUIDs for chat identifiers.
- tokio (indirect / runtime used by Dioxus): asynchronous runtime used by async networking.

These crates are chosen for their ergonomics and small, practical APIs for a local GUI chat app.

---

## How it works

- Chats are stored in `chats` table with columns `(id TEXT PRIMARY KEY, title TEXT NOT NULL)` where `id` is a UUID string and `title` is the visible name.
- Messages are stored in `messages` table with `(id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id TEXT, role TEXT, content TEXT, timestamp DATETIME)`.
- Settings are persisted in a `settings` table (single-row, id=1).
- The UI keeps a small in-memory buffer of the currently-viewed chat's messages for immediate responsiveness, but assistant responses are always written to the DB. Assistant replies are only pushed into the in-memory buffer if the user is still viewing that chat when the response arrives. This prevents replies from "appearing" in the wrong visible chat.
- When interrupting a running request, the in-flight HTTP call is allowed to complete, but the code marks the request as cancelled and simply discards the final assistant output (no interruption message is inserted). The UI removes the "Thinking..." indicator immediately for a responsive feel.

---

## Build & Run

Requirements:

- Rust
- Cargo
- Ollama running locally and hosting models
- Git

1. Clone the repository

```bash
git clone https://github.com/KPCOFGS/RustyChat.git
cd RustyChat
```

2. Run

```bash
dx build --release
```

3. Run

```bash
./target/dx/rusty-chat/release/linux/app/rusty-chat
```
Notes:

- The app creates/uses `chat.db` in the `./target/dx/rusty-chat/release/linux/app/` directory. Backup if necessary before deleting.

---

## Screenshots

[!screenshot1](./assets/Screenshot1.png)
[!screenshot2](./assets/Screenshot2.png)

---

## Contribution

Contributions welcome. Please open issues or PRs for bugs, feature requests, or improvements.

---

## License

This project is licensed under the MIT license. See [LICENSE](./LICENSE) file for more details.
