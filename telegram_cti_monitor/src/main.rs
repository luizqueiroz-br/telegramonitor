/**
 * Telegram CTI Monitor - Rust (conta de usuário via MTProto)
 *
 * Autentica como a sua conta do Telegram (telefone + código), monitora os
 * chats dela e envia as mensagens para uma plataforma CTI.
 *
 * Configuração via .env:
 *   TELEGRAM_API_ID      id do app em https://my.telegram.org (obrigatório)
 *   TELEGRAM_API_HASH    hash do app (obrigatório)
 *   TELEGRAM_PHONE       telefone internacional, usado só no primeiro login
 *   SESSION_PATH         arquivo de sessão (padrão: monitor.session)
 *   CTI_API_URL          URL da API CTI (opcional; vazio = só console)
 *   MONITORED_CHATS      ids de chat separados por vírgula (vazio = todos)
 *
 * Uso: `make setup`, depois `make run`.
 */

use std::collections::HashMap;
use std::io::{self, BufRead as _, Write as _};
use std::process::Command;
use std::sync::Arc;

use grammers_client::media::Media;
use grammers_client::peer::{Peer, User};
use grammers_client::sender::UpdatesConfiguration;
use grammers_client::tl;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerRef;
use serde::{Deserialize, Serialize};

// ============================================================
// CONFIGURAÇÃO (.env)
// ============================================================

struct Config {
    api_id: Option<i32>,
    api_hash: Option<String>,
    phone: Option<String>,
    session_path: String,
    db_path: String,
    cti_url: Option<String>,
    monitored_chats: Vec<i64>,
}

/// Valor de uma variável: ambiente real tem prioridade sobre o .env.
fn env_or_dotenv(file: &HashMap<String, String>, key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| file.get(key).cloned().filter(|v| !v.trim().is_empty()))
}

/// Lê pares KEY=VALUE de um .env. Variáveis reais do ambiente têm prioridade.
fn load_dotenv(path: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return vars;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            vars.insert(
                key.trim().to_string(),
                value.trim().trim_matches(['"', '\'']).to_string(),
            );
        }
    }
    vars
}

impl Config {
    fn from_env(file: &HashMap<String, String>) -> Result<Self, String> {
        let lookup = |key: &str| env_or_dotenv(file, key);

        let api_id = match lookup("TELEGRAM_API_ID") {
            Some(value) => Some(
                value
                    .trim()
                    .parse::<i32>()
                    .map_err(|_| "TELEGRAM_API_ID deve ser um número (ex.: 1234567).".to_string())?,
            ),
            None => None,
        };

        let api_hash = lookup("TELEGRAM_API_HASH");

        let monitored_chats = lookup("MONITORED_CHATS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<i64>().ok())
            .collect();

        Ok(Self {
            api_id,
            api_hash,
            phone: lookup("TELEGRAM_PHONE"),
            session_path: lookup("SESSION_PATH").unwrap_or_else(|| "monitor.session".to_string()),
            db_path: lookup("DB_PATH").unwrap_or_else(|| "messages.db".to_string()),
            cti_url: lookup("CTI_API_URL"),
            monitored_chats,
        })
    }
}

/// Atualiza (ou adiciona) pares KEY=VALUE no .env, preservando o resto.
fn save_env(pairs: &[(&str, &str)]) -> Result<(), String> {
    let existing = std::fs::read_to_string(".env").unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

    for (key, value) in pairs {
        let prefix = format!("{key}=");
        if let Some(line) = lines.iter_mut().find(|l| l.trim_start().starts_with(&prefix)) {
            *line = format!("{key}={value}");
        } else {
            lines.push(format!("{key}={value}"));
        }
    }

    let mut content = lines.join("\n");
    content.push('\n');
    std::fs::write(".env", content).map_err(|e| format!("Falha ao salvar o .env: {e}"))
}

/// Pergunta e salva no .env as credenciais do app do Telegram, se faltarem.
fn ensure_api_credentials(config: &mut Config) -> Result<(), String> {
    if config.api_id.is_some() && config.api_hash.is_some() {
        return Ok(());
    }

    println!("\nPrimeiro uso: configure o app do Telegram.");
    println!("Acesse https://my.telegram.org -> API development tools e crie um app.\n");

    if config.api_id.is_none() {
        loop {
            let value = prompt("API_ID: ");
            match value.parse::<i32>() {
                Ok(id) => {
                    save_env(&[("TELEGRAM_API_ID", &value)])?;
                    config.api_id = Some(id);
                    break;
                }
                Err(_) => println!("Digite apenas números (ex.: 1234567)."),
            }
        }
    }

    if config.api_hash.is_none() {
        let value = loop {
            let value = prompt("API_HASH: ");
            if value.is_empty() {
                println!("Não pode ficar vazio.");
            } else {
                break value;
            }
        };
        save_env(&[("TELEGRAM_API_HASH", &value)])?;
        config.api_hash = Some(value);
    }

    Ok(())
}

// ============================================================
// MODELO DE DADOS
// ============================================================

/// Dados de uma mensagem monitorada - salvar na API CTI.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct MonitoredMessage {
    message_id: i64,
    chat_id: i64,
    chat_title: String,
    chat_username: Option<String>,
    chat_kind: String, // "user", "group", "channel", "community", "unknown"
    timestamp: String, // ISO 8601: 2026-11-14T14:30:00Z
    message_text: String,
    message_type: String, // "text", "photo", "document", "link", ...
    action_type: String,  // "post", "reply", "forward"
    has_links: bool,
    has_hashes: bool,

    sender_id: i64,
    sender_username: Option<String>,
    sender_first_name: Option<String>,
    sender_last_name: Option<String>,
    sender_phone: Option<String>,
    sender_is_bot: bool,
    sender_is_verified: bool,
    sender_is_scam: bool,
    sender_is_deleted: bool,

    media_name: Option<String>,
    media_mime: Option<String>,
    media_size: Option<i64>,
    media_duration: Option<f64>,

    geo_lat: Option<f64>,
    geo_lon: Option<f64>,
    venue_title: Option<String>,
    venue_address: Option<String>,
    contact_name: Option<String>,
    contact_phone: Option<String>,
    poll_question: Option<String>,
    poll_answers: Option<String>,

    forward_from_id: Option<i64>,
    forward_from_name: Option<String>,
    forward_date: Option<String>,
    reply_to_message_id: Option<i64>,
    reply_quote: Option<String>,

    view_count: Option<i64>,
    forward_count: Option<i64>,
    reply_count: Option<i64>,
    reaction_count: Option<i64>,
    grouped_id: Option<i64>,
    post_author: Option<String>,
    edit_date: Option<String>,
}

/// Colunas da tabela `messages`, na mesma ordem de [`MonitoredMessage::row_values`].
/// A lista única gera o CREATE e migra bancos antigos.
const MESSAGE_COLUMNS: &[(&str, &str)] = &[
    ("message_id", "INTEGER NOT NULL"),
    ("chat_id", "INTEGER NOT NULL"),
    ("chat_title", "TEXT"),
    ("chat_username", "TEXT"),
    ("chat_kind", "TEXT"),
    ("timestamp", "TEXT"),
    ("message_text", "TEXT"),
    ("message_type", "TEXT"),
    ("action_type", "TEXT"),
    ("has_links", "INTEGER"),
    ("has_hashes", "INTEGER"),
    ("sender_id", "INTEGER"),
    ("sender_username", "TEXT"),
    ("sender_first_name", "TEXT"),
    ("sender_last_name", "TEXT"),
    ("sender_phone", "TEXT"),
    ("sender_is_bot", "INTEGER"),
    ("sender_is_verified", "INTEGER"),
    ("sender_is_scam", "INTEGER"),
    ("sender_is_deleted", "INTEGER"),
    ("media_name", "TEXT"),
    ("media_mime", "TEXT"),
    ("media_size", "INTEGER"),
    ("media_duration", "REAL"),
    ("geo_lat", "REAL"),
    ("geo_lon", "REAL"),
    ("venue_title", "TEXT"),
    ("venue_address", "TEXT"),
    ("contact_name", "TEXT"),
    ("contact_phone", "TEXT"),
    ("poll_question", "TEXT"),
    ("poll_answers", "TEXT"),
    ("forward_from_id", "INTEGER"),
    ("forward_from_name", "TEXT"),
    ("forward_date", "TEXT"),
    ("reply_to_message_id", "INTEGER"),
    ("reply_quote", "TEXT"),
    ("view_count", "INTEGER"),
    ("forward_count", "INTEGER"),
    ("reply_count", "INTEGER"),
    ("reaction_count", "INTEGER"),
    ("grouped_id", "INTEGER"),
    ("post_author", "TEXT"),
    ("edit_date", "TEXT"),
];

fn opt_text(value: &Option<String>) -> libsql::Value {
    match value {
        Some(text) => libsql::Value::Text(text.clone()),
        None => libsql::Value::Null,
    }
}

fn opt_int(value: Option<i64>) -> libsql::Value {
    match value {
        Some(number) => libsql::Value::Integer(number),
        None => libsql::Value::Null,
    }
}

fn opt_real(value: Option<f64>) -> libsql::Value {
    match value {
        Some(number) => libsql::Value::Real(number),
        None => libsql::Value::Null,
    }
}

fn opt_str(value: Option<&str>) -> Option<String> {
    value.map(str::to_string)
}

impl MonitoredMessage {
    fn row_values(&self) -> Vec<libsql::Value> {
        vec![
            libsql::Value::Integer(self.message_id),
            libsql::Value::Integer(self.chat_id),
            libsql::Value::Text(self.chat_title.clone()),
            opt_text(&self.chat_username),
            libsql::Value::Text(self.chat_kind.clone()),
            libsql::Value::Text(self.timestamp.clone()),
            libsql::Value::Text(self.message_text.clone()),
            libsql::Value::Text(self.message_type.clone()),
            libsql::Value::Text(self.action_type.clone()),
            libsql::Value::Integer(self.has_links as i64),
            libsql::Value::Integer(self.has_hashes as i64),
            libsql::Value::Integer(self.sender_id),
            opt_text(&self.sender_username),
            opt_text(&self.sender_first_name),
            opt_text(&self.sender_last_name),
            opt_text(&self.sender_phone),
            libsql::Value::Integer(self.sender_is_bot as i64),
            libsql::Value::Integer(self.sender_is_verified as i64),
            libsql::Value::Integer(self.sender_is_scam as i64),
            libsql::Value::Integer(self.sender_is_deleted as i64),
            opt_text(&self.media_name),
            opt_text(&self.media_mime),
            opt_int(self.media_size),
            opt_real(self.media_duration),
            opt_real(self.geo_lat),
            opt_real(self.geo_lon),
            opt_text(&self.venue_title),
            opt_text(&self.venue_address),
            opt_text(&self.contact_name),
            opt_text(&self.contact_phone),
            opt_text(&self.poll_question),
            opt_text(&self.poll_answers),
            opt_int(self.forward_from_id),
            opt_text(&self.forward_from_name),
            opt_text(&self.forward_date),
            opt_int(self.reply_to_message_id),
            opt_text(&self.reply_quote),
            opt_int(self.view_count),
            opt_int(self.forward_count),
            opt_int(self.reply_count),
            opt_int(self.reaction_count),
            opt_int(self.grouped_id),
            opt_text(&self.post_author),
            opt_text(&self.edit_date),
        ]
    }
}

/// Detalhes extraídos do objeto de mídia (sem download).
#[derive(Default)]
struct MediaDetails {
    name: Option<String>,
    mime: Option<String>,
    size: Option<i64>,
    duration: Option<f64>,
    geo_lat: Option<f64>,
    geo_lon: Option<f64>,
    venue_title: Option<String>,
    venue_address: Option<String>,
    contact_name: Option<String>,
    contact_phone: Option<String>,
    poll_question: Option<String>,
    poll_answers: Option<String>,
}

fn text_with_entities(value: &tl::enums::TextWithEntities) -> String {
    match value {
        tl::enums::TextWithEntities::Entities(text) => text.text.clone(),
    }
}

fn media_details(media: Option<&Media>) -> MediaDetails {
    let mut details = MediaDetails::default();
    match media {
        Some(Media::Photo(photo)) => details.size = photo.size().map(|s| s as i64),
        Some(Media::Document(document)) => {
            details.name = opt_str(document.name());
            details.mime = opt_str(document.mime_type());
            details.size = document.size().map(|s| s as i64);
            details.duration = document.duration();
        }
        Some(Media::Contact(contact)) => {
            let name = format!("{} {}", contact.first_name(), contact.last_name())
                .trim()
                .to_string();
            details.contact_name = (!name.is_empty()).then_some(name);
            details.contact_phone = Some(contact.phone_number().to_string());
        }
        Some(Media::Geo(geo)) => {
            details.geo_lat = Some(geo.latitue());
            details.geo_lon = Some(geo.longitude());
        }
        Some(Media::Venue(venue)) => {
            details.venue_title = Some(venue.title().to_string());
            details.venue_address = Some(venue.address().to_string());
        }
        Some(Media::Poll(poll)) => {
            details.poll_question = Some(text_with_entities(poll.question()));
            let answers: Vec<String> = poll
                .iter_answers()
                .map(|answer| text_with_entities(&answer.text()))
                .collect();
            details.poll_answers = (!answers.is_empty()).then(|| answers.join(" | "));
        }
        _ => {}
    }
    details
}

/// Converte um Peer do TL para o id no formato Bot API (mesmo dos campos chat_id/sender_id).
fn raw_peer_dialog_id(peer: &tl::enums::Peer) -> i64 {
    match peer {
        tl::enums::Peer::User(user) => user.user_id,
        tl::enums::Peer::Chat(chat) => -chat.chat_id,
        tl::enums::Peer::Channel(channel) => -(1_000_000_000_000 + channel.channel_id),
    }
}

fn unix_to_iso(seconds: i32) -> Option<String> {
    jiff::Timestamp::from_second(seconds as i64)
        .ok()
        .map(|timestamp| timestamp.to_string())
}

// ============================================================
// BANCO LOCAL (SQLite)
// ============================================================

const AVATAR_DIR: &str = "avatars";

/// Adiciona colunas que faltam em bancos criados por versões anteriores.
async fn ensure_columns(
    conn: &libsql::Connection,
    table: &str,
    columns: &[(&str, &str)],
) -> Result<(), String> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .map_err(|e| format!("Falha ao ler o esquema de '{table}': {e}"))?;
    let mut existing = std::collections::HashSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao ler o esquema de '{table}': {e}"))?
    {
        if let Ok(name) = row.get::<String>(1) {
            existing.insert(name);
        }
    }
    drop(rows);

    for (name, kind) in columns {
        if !existing.contains(*name) {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {name} {kind}"),
                (),
            )
            .await
            .map_err(|e| format!("Falha ao migrar '{table}': {e}"))?;
        }
    }
    Ok(())
}

/// Baixa a foto de perfil indicada para `path`. Devolve `false` se não achar.
async fn download_avatar(
    client: &Client,
    peer_ref: PeerRef,
    wanted_photo_id: i64,
    path: &str,
) -> Result<bool, String> {
    let mut photos = client.iter_profile_photos(peer_ref);
    while let Some(photo) = photos
        .next()
        .await
        .map_err(|e| format!("consulta de fotos: {e}"))?
    {
        if photo.id() != wanted_photo_id {
            continue;
        }
        let mut download = client.iter_download(&photo);
        let mut bytes = Vec::new();
        while let Some(chunk) = download
            .next()
            .await
            .map_err(|e| format!("download da foto: {e}"))?
        {
            bytes.extend_from_slice(&chunk);
        }
        if let Some(dir) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("criar pasta '{}': {e}", dir.display()))?;
        }
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| format!("gravar '{path}': {e}"))?;
        return Ok(true);
    }
    Ok(false)
}

fn chat_photo_id(peer: &Peer) -> Option<i64> {
    match peer {
        Peer::User(user) => user.photo().map(|photo| photo.photo_id),
        Peer::Group(group) => group.photo().map(|photo| photo.photo_id),
        Peer::Channel(channel) => channel.photo().map(|photo| photo.photo_id),
        Peer::Community(community) => community.photo().map(|photo| photo.photo_id),
    }
}

/// Baixa a foto só quando ela ainda não está no catálogo (uma vez por entidade).
async fn ensure_user_avatar(
    client: &Client,
    store: &MessageStore,
    user: &User,
    peer_ref: Option<PeerRef>,
) {
    let id = user.id().bot_api_dialog_id_unchecked();
    let wanted = user.photo().map(|photo| photo.photo_id).unwrap_or(0);
    if store.avatar_photo_id("users", id).await == Some(wanted) {
        return;
    }
    if wanted == 0 {
        let _ = store.set_avatar("users", id, 0, None).await;
        return;
    }
    let Some(peer_ref) = peer_ref else { return };
    let path = format!("{AVATAR_DIR}/user-{id}.jpg");
    match download_avatar(client, peer_ref, wanted, &path).await {
        Ok(true) => {
            let _ = store.set_avatar("users", id, wanted, Some(&path)).await;
        }
        Ok(false) => {
            let _ = store.set_avatar("users", id, 0, None).await;
        }
        Err(e) => eprintln!("[aviso] foto do usuário {id}: {e}"),
    }
}

/// Mesma ideia de [`ensure_user_avatar`], para grupos/canais.
async fn ensure_chat_avatar(
    client: &Client,
    store: &MessageStore,
    peer: &Peer,
    peer_ref: Option<PeerRef>,
) {
    let id = peer.id().bot_api_dialog_id_unchecked();
    let wanted = chat_photo_id(peer).unwrap_or(0);
    if store.avatar_photo_id("chats", id).await == Some(wanted) {
        return;
    }
    if wanted == 0 {
        let _ = store.set_avatar("chats", id, 0, None).await;
        return;
    }
    let Some(peer_ref) = peer_ref else { return };
    let path = format!("{AVATAR_DIR}/chat-{id}.jpg");
    match download_avatar(client, peer_ref, wanted, &path).await {
        Ok(true) => {
            let _ = store.set_avatar("chats", id, wanted, Some(&path)).await;
        }
        Ok(false) => {
            let _ = store.set_avatar("chats", id, 0, None).await;
        }
        Err(e) => eprintln!("[aviso] foto do chat {id}: {e}"),
    }
}

struct MessageStore {
    conn: libsql::Connection,
}

impl MessageStore {
    async fn open(path: &str) -> Result<Self, String> {
        let db = libsql::Builder::new_local(path)
            .build()
            .await
            .map_err(|e| format!("Falha ao abrir o banco '{}': {e}", path))?;
        let conn = db
            .connect()
            .map_err(|e| format!("Falha ao conectar no banco '{}': {e}", path))?;
        // WAL deixa leitura e escrita simultâneas; busy_timeout evita "database is locked".
        conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")
            .await
            .map_err(|e| format!("Falha ao configurar o banco '{}': {e}", path))?;
        let columns = MESSAGE_COLUMNS
            .iter()
            .map(|(name, kind)| format!("{name} {kind}"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, {columns},
                    saved_at TEXT DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(chat_id, message_id)
                )"
            ),
            (),
        )
        .await
        .map_err(|e| format!("Falha ao criar a tabela messages: {e}"))?;
        ensure_columns(&conn, "messages", MESSAGE_COLUMNS).await?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                username TEXT,
                first_name TEXT,
                last_name TEXT,
                phone TEXT,
                is_bot INTEGER,
                is_verified INTEGER,
                is_scam INTEGER,
                is_deleted INTEGER,
                avatar_photo_id INTEGER,
                avatar_path TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS chats (
                id INTEGER PRIMARY KEY,
                title TEXT,
                username TEXT,
                kind TEXT,
                avatar_photo_id INTEGER,
                avatar_path TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .await
        .map_err(|e| format!("Falha ao criar as tabelas de catálogo: {e}"))?;

        Ok(Self { conn })
    }

    /// Grava a mensagem. Mensagens repetidas (catch-up) são ignoradas.
    async fn insert(&self, msg: &MonitoredMessage) -> Result<(), String> {
        let columns = MESSAGE_COLUMNS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ");
        let marks = MESSAGE_COLUMNS
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        self.conn
            .execute(
                &format!("INSERT OR IGNORE INTO messages ({columns}) VALUES ({marks})"),
                libsql::params_from_iter(msg.row_values()),
            )
            .await
            .map_err(|e| format!("Falha ao gravar no banco: {e}"))?;
        Ok(())
    }

    /// Catálogo de usuários: dados vindos dos próprios updates (sem chamada extra).
    async fn upsert_user(&self, user: &User) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO users (id, username, first_name, last_name, phone,
                    is_bot, is_verified, is_scam, is_deleted, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT(id) DO UPDATE SET
                    username = excluded.username,
                    first_name = excluded.first_name,
                    last_name = excluded.last_name,
                    phone = COALESCE(excluded.phone, users.phone),
                    is_bot = excluded.is_bot,
                    is_verified = excluded.is_verified,
                    is_scam = excluded.is_scam,
                    is_deleted = excluded.is_deleted,
                    updated_at = CURRENT_TIMESTAMP",
                libsql::params![
                    user.id().bot_api_dialog_id_unchecked(),
                    opt_text(&opt_str(user.username())),
                    opt_text(&opt_str(user.first_name())),
                    opt_text(&opt_str(user.last_name())),
                    opt_text(&opt_str(user.phone())),
                    user.is_bot() as i64,
                    user.verified() as i64,
                    user.scam() as i64,
                    user.deleted() as i64,
                ],
            )
            .await
            .map_err(|e| format!("Falha ao catalogar usuário: {e}"))?;
        Ok(())
    }

    /// Catálogo de grupos/canais. Usuários são ignorados (vão para `users`).
    async fn upsert_chat(&self, peer: &Peer) -> Result<(), String> {
        let (title, username, kind) = match peer {
            Peer::Group(group) => (group.title().unwrap_or("").to_string(), opt_str(group.username()), "group"),
            Peer::Channel(channel) => (channel.title().to_string(), opt_str(channel.username()), "channel"),
            Peer::Community(community) => (community.title().to_string(), None, "community"),
            Peer::User(_) => return Ok(()),
        };
        self.conn
            .execute(
                "INSERT INTO chats (id, title, username, kind, updated_at)
                 VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    username = excluded.username,
                    kind = excluded.kind,
                    updated_at = CURRENT_TIMESTAMP",
                libsql::params![
                    peer.id().bot_api_dialog_id_unchecked(),
                    title,
                    opt_text(&username),
                    kind,
                ],
            )
            .await
            .map_err(|e| format!("Falha ao catalogar chat: {e}"))?;
        Ok(())
    }

    /// Qual foto já foi baixada para a entidade (0 = verificado, sem foto).
    async fn avatar_photo_id(&self, table: &str, id: i64) -> Option<i64> {
        let mut rows = self
            .conn
            .query(
                &format!("SELECT avatar_photo_id FROM {table} WHERE id = ?"),
                libsql::params![id],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        match row.get::<libsql::Value>(0).ok()? {
            libsql::Value::Integer(value) => Some(value),
            _ => None,
        }
    }

    async fn set_avatar(&self, table: &str, id: i64, photo_id: i64, path: Option<&str>) -> Result<(), String> {
        self.conn
            .execute(
                &format!("UPDATE {table} SET avatar_photo_id = ?, avatar_path = ? WHERE id = ?"),
                libsql::params![photo_id, opt_text(&opt_str(path)), id],
            )
            .await
            .map_err(|e| format!("Falha ao salvar avatar: {e}"))?;
        Ok(())
    }

    async fn recent(&self, limit: u32) -> Result<Vec<(String, String, String, String)>, String> {        let mut rows = self
            .conn
            .query(
                "SELECT timestamp, chat_title, COALESCE(sender_username, ''), message_text
                 FROM messages ORDER BY id DESC LIMIT ?",
                libsql::params![limit],
            )
            .await
            .map_err(|e| format!("Falha ao ler o banco: {e}"))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Falha ao ler o banco: {e}"))?
        {
            let get = |i| -> Result<String, String> {
                row.get::<String>(i).map_err(|e| format!("Coluna inválida: {e}"))
            };
            out.push((get(0)?, get(1)?, get(2)?, get(3)?));
        }
        Ok(out)
    }

    /// Gera uma cópia consolidada sem WAL, para abrir em visualizadores
    /// SQLite (o WAL não funciona em caminhos como \\wsl.localhost).
    async fn export(&self, path: &str) -> Result<(), String> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .await
            .map_err(|e| format!("Falha ao consolidar o WAL: {e}"))?;

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }

        let escaped = path.replace('\'', "''");
        self.conn
            .execute(&format!("VACUUM INTO '{escaped}'"), ())
            .await
            .map_err(|e| format!("Falha ao exportar para '{}': {e}", path))?;

        let db = libsql::Builder::new_local(path)
            .build()
            .await
            .map_err(|e| format!("Falha ao abrir a cópia '{}': {e}", path))?;
        db.connect()
            .map_err(|e| format!("Falha ao abrir a cópia '{}': {e}", path))?
            .execute_batch("PRAGMA journal_mode=DELETE;")
            .await
            .map_err(|e| format!("Falha ao remover o WAL da cópia: {e}"))?;
        Ok(())
    }
}

// ============================================================
// LOGIN
// ============================================================

/// Pergunta algo no terminal e devolve a linha sem espaços nas pontas.
fn prompt(label: &str) -> String {
    print!("{label}");
    io::stdout().flush().ok();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("\nEntrada encerrada.");
        std::process::exit(1);
    }
    line.trim().to_string()
}

async fn ensure_logged_in(client: &Client, config: &mut Config) -> Result<(), String> {
    if client
        .is_authorized()
        .await
        .map_err(|e| format!("Falha ao verificar a sessão: {e}"))?
    {
        println!("Sessão existente carregada ({}).", config.session_path);
        return Ok(());
    }

    if config.phone.is_none() {
        println!("\nTelefone da conta para o primeiro login (formato internacional).");
        let phone = loop {
            let value = prompt("Telefone (ex.: +5511999999999): ");
            if value.starts_with('+') && value.len() > 5 {
                break value;
            }
            println!("Use o formato internacional, começando com +.");
        };
        save_env(&[("TELEGRAM_PHONE", &phone)])?;
        config.phone = Some(phone);
    }
    let phone = config.phone.clone().unwrap_or_default();
    let api_hash = config
        .api_hash
        .clone()
        .ok_or("API_HASH ausente, configure o app do Telegram.")?;

    let token = client
        .request_login_code(&phone, &api_hash)
        .await
        .map_err(|e| format!("Falha ao pedir o código de login: {e}"))?;

    println!("Código enviado para {phone}. Ele chega dentro do app Telegram.");
    let code = prompt("Código: ");

    let user = match client.sign_in(&token, &code).await {
        Ok(user) => user,
        Err(SignInError::PasswordRequired(password_token)) => {
            let hint = password_token
                .hint()
                .map(|h| format!(" (dica: {h})"))
                .unwrap_or_default();
            let mut password_token = password_token;
            loop {
                let password = prompt(&format!("Senha 2FA{hint}: "));
                match client.check_password(password_token, password).await {
                    Ok(user) => break user,
                    Err(SignInError::InvalidPassword(token)) => {
                        println!("Senha inválida, tente de novo.");
                        password_token = token;
                    }
                    Err(e) => return Err(format!("Falha no login 2FA: {e}")),
                }
            }
        }
        Err(SignInError::InvalidCode) => return Err("Código inválido. Rode novamente e digite o código mais recente.".into()),
        Err(SignInError::SignUpRequired) => {
            return Err("Esta conta ainda não existe no Telegram. Crie-a primeiro no app oficial.".into());
        }
        Err(e) => return Err(format!("Falha no login: {e}")),
    };

    let id = user.id().bot_api_dialog_id().unwrap_or_default();
    println!("Login OK: {} (id {id}).", user.full_name());
    Ok(())
}

// ============================================================
// ENVIO PARA O CTI
// ============================================================

/// Enviar mensagem para a API CTI
async fn send_to_cti_api(cti_url: &str, msg: &MonitoredMessage) -> Result<(), String> {
    let json_body = serde_json::to_string(msg)
        .map_err(|e| format!("Erro ao serializar JSON: {}", e))?;

    // curl evita compilar TLS em Rust
    let output = Command::new("curl")
        .args(["-s", "-m", "30", "-X", "POST", "-H", "Content-Type: application/json", "-d", &json_body, cti_url])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !stderr.is_empty() {
                eprintln!("[curl] stderr: {}", stderr);
            }
            if !output.status.success() {
                return Err(format!("curl saiu com {}: {}", output.status, stderr));
            }
            if stdout.contains("error") {
                return Err(format!("API CTI retornou erro: {}", stdout));
            }
            Ok(())
        }
        Err(e) => Err(format!("Falha ao executar curl: {}", e)),
    }
}

// ============================================================
// PROCESSAMENTO DE MENSAGENS
// ============================================================

fn media_type(media: Option<Media>) -> &'static str {
    match media {
        None => "text",
        Some(Media::Photo(_)) => "photo",
        Some(Media::Document(_)) => "document",
        Some(Media::Sticker(_)) => "sticker",
        Some(Media::WebPage(_)) => "link",
        Some(Media::Poll(_)) => "poll",
        Some(Media::Geo(_)) | Some(Media::GeoLive(_)) | Some(Media::Venue(_)) => "location",
        Some(Media::Contact(_)) => "contact",
        Some(_) => "other",
    }
}

async fn handle_message(
    client: &Client,
    config: &Config,
    store: &MessageStore,
    message: grammers_client::update::Message,
) {
    let chat_id = message.peer_id().bot_api_dialog_id().unwrap_or_default();

    if !config.monitored_chats.is_empty() && !config.monitored_chats.contains(&chat_id) {
        return;
    }

    let peer = message.peer();
    let chat_title = peer
        .and_then(|p| p.name())
        .unwrap_or("Desconhecido")
        .to_string();
    let chat_username = peer.and_then(|p| p.username()).map(str::to_string);
    let chat_kind = match peer {
        Some(Peer::User(_)) => "user",
        Some(Peer::Group(_)) => "group",
        Some(Peer::Channel(_)) => "channel",
        Some(Peer::Community(_)) => "community",
        None => "unknown",
    }
    .to_string();

    let text = message.text();
    let has_links =
        text.contains("http://") || text.contains("https://") || text.contains("t.me/");
    let media = message.media();
    let message_type = media_type(media.clone());
    let details = media_details(media.as_ref());

    let sender = message.sender();
    let (
        sender_username,
        sender_first_name,
        sender_last_name,
        sender_phone,
        sender_is_bot,
        sender_is_verified,
        sender_is_scam,
        sender_is_deleted,
    ) = match sender {
        Some(Peer::User(user)) => (
            opt_str(user.username()),
            opt_str(user.first_name()),
            opt_str(user.last_name()),
            opt_str(user.phone()),
            user.is_bot(),
            user.verified(),
            user.scam(),
            user.deleted(),
        ),
        // Post de canal/grupo: o "remetente" é o próprio chat.
        Some(Peer::Channel(channel)) => (
            opt_str(channel.username()),
            Some(channel.title().to_string()),
            None,
            None,
            false,
            false,
            false,
            false,
        ),
        Some(Peer::Group(group)) => (
            opt_str(group.username()),
            group.title().map(str::to_string),
            None,
            None,
            false,
            false,
            false,
            false,
        ),
        Some(Peer::Community(community)) => (
            None,
            Some(community.title().to_string()),
            None,
            None,
            false,
            false,
            false,
            false,
        ),
        _ => (None, None, None, None, false, false, false, false),
    };
    let sender_id = message
        .sender_id()
        .and_then(|p| p.bot_api_dialog_id())
        .unwrap_or_default();

    let action_type = if message.reply_header().is_some() {
        "reply"
    } else if message.forward_header().is_some() {
        "forward"
    } else {
        "post"
    };

    let (forward_from_id, forward_from_name, forward_date) = match message.forward_header() {
        Some(tl::enums::MessageFwdHeader::Header(header)) => (
            header.from_id.as_ref().map(raw_peer_dialog_id),
            header.from_name.clone(),
            unix_to_iso(header.date),
        ),
        _ => (None, None, None),
    };

    let (reply_to_message_id, reply_quote) = match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header)) => (
            header.reply_to_msg_id.map(|id| id as i64),
            header.quote_text.clone(),
        ),
        _ => (None, None),
    };

    let monitored_msg = MonitoredMessage {
        message_id: message.id() as i64,
        chat_id,
        chat_title: chat_title.clone(),
        chat_username,
        chat_kind,
        timestamp: message.date().to_string(),
        message_text: text.to_string(),
        message_type: message_type.to_string(),
        action_type: action_type.to_string(),
        has_links,
        has_hashes: false, // TODO: extrair hashes de documentos/arquivos
        sender_id,
        sender_username,
        sender_first_name,
        sender_last_name,
        sender_phone,
        sender_is_bot,
        sender_is_verified,
        sender_is_scam,
        sender_is_deleted,
        media_name: details.name,
        media_mime: details.mime,
        media_size: details.size,
        media_duration: details.duration,
        geo_lat: details.geo_lat,
        geo_lon: details.geo_lon,
        venue_title: details.venue_title,
        venue_address: details.venue_address,
        contact_name: details.contact_name,
        contact_phone: details.contact_phone,
        poll_question: details.poll_question,
        poll_answers: details.poll_answers,
        forward_from_id,
        forward_from_name,
        forward_date,
        reply_to_message_id,
        reply_quote,
        view_count: message.view_count().map(|v| v as i64),
        forward_count: message.forward_count().map(|v| v as i64),
        reply_count: message.reply_count().map(|v| v as i64),
        reaction_count: message.reaction_count().map(|v| v as i64),
        grouped_id: message.grouped_id(),
        post_author: opt_str(message.post_author()),
        edit_date: message.edit_date().map(|date| date.to_string()),
    };

    if let Err(e) = store.insert(&monitored_msg).await {
        eprintln!("[ERRO] {e}");
    }

    if let Some(cti_url) = &config.cti_url {
        if let Err(e) = send_to_cti_api(cti_url, &monitored_msg).await {
            eprintln!("[ERRO] Falha ao enviar para API CTI: {}", e);
        }
    }

    // Catálogo: usa os objetos que já vieram no update (sem consulta extra).
    // A foto só é baixada uma vez por entidade e fica em `avatars/`.
    let sender_ref = message.sender_ref().await.ok().flatten();
    let peer_ref = message.peer_ref().await.ok().flatten();
    if let Some(Peer::User(user)) = message.sender() {
        if let Err(e) = store.upsert_user(user).await {
            eprintln!("[ERRO] {e}");
        }
        ensure_user_avatar(client, store, user, sender_ref).await;
    }
    if let Some(chat_peer) = message.peer() {
        if !matches!(chat_peer, Peer::User(_)) {
            if let Err(e) = store.upsert_chat(chat_peer).await {
                eprintln!("[ERRO] {e}");
            }
            ensure_chat_avatar(client, store, chat_peer, peer_ref).await;
        }
    }

    let sender_display = match (
        &monitored_msg.sender_first_name,
        &monitored_msg.sender_username,
    ) {
        (Some(name), Some(username)) => format!("{name} (@{username})"),
        (Some(name), None) => name.clone(),
        (None, Some(username)) => format!("@{username}"),
        (None, None) => "Desconhecido".to_string(),
    };
    println!("[+] {} [{}]:", chat_title, message_type);
    println!("    Remetente: {sender_display}");
    println!("    ID do remetente: {}", monitored_msg.sender_id);
    println!("    Conteúdo: {}", monitored_msg.message_text);
    if has_links {
        println!("    ⚠️  POSSÍVEL LINK SUSPEITO detectado!");
    }
    println!("    Timestamp: {}", monitored_msg.timestamp);
    println!();
}

// ============================================================
// MAIN
// ============================================================

async fn run() -> Result<(), String> {
    let dotenv = load_dotenv(".env");
    let mut config = Config::from_env(&dotenv)?;
    ensure_api_credentials(&mut config)?;
    let api_id = config.api_id.ok_or("API_ID ausente.")?;
    let store = MessageStore::open(&config.db_path).await?;

    let session = Arc::new(
        SqliteSession::open(&config.session_path)
            .await
            .map_err(|e| format!("Falha ao abrir a sessão '{}': {e}", config.session_path))?,
    );

    let SenderPool {
        runner,
        updates,
        handle,
    } = SenderPool::new(session, api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    ensure_logged_in(&client, &mut config).await?;

    // Listar os chats da conta (mostra os ids e popula o cache de peers dos updates)
    println!("\nChats da conta:");
    let mut dialogs = client.iter_dialogs();
    while let Some(dialog) = dialogs
        .next()
        .await
        .map_err(|e| format!("Falha ao listar os chats: {e}"))?
    {
        let id = dialog.peer_id().bot_api_dialog_id().unwrap_or_default();
        let name = dialog.peer().name().unwrap_or("(sem nome)");
        println!("  {id}  {name}");
    }

    println!(
        "\nMonitorando {} (Ctrl+C para sair)...\n",
        if config.monitored_chats.is_empty() {
            "todos os chats".to_string()
        } else {
            format!("{:?}", config.monitored_chats)
        }
    );

    let mut stream = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: true })
        .await
        .map_err(|e| format!("Falha ao iniciar o stream de updates: {e}"))?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            update = stream.next() => {
                let update = update.map_err(|e| format!("Erro ao receber update: {e}"))?;
                if let Update::NewMessage(message) = update {
                    handle_message(&client, &config, &store, message).await;
                }
            }
        }
    }

    println!("Salvando sessão...");
    stream
        .sync_update_state()
        .await
        .map_err(|e| format!("Falha ao salvar o estado de updates: {e}"))?;
    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// Mostra as mensagens gravadas no banco local (comando `dump`).
async fn dump_messages() -> Result<(), String> {
    let dotenv = load_dotenv(".env");
    let path = env_or_dotenv(&dotenv, "DB_PATH").unwrap_or_else(|| "messages.db".to_string());
    let store = MessageStore::open(&path).await?;
    let messages = store.recent(50).await?;

    if messages.is_empty() {
        println!("Nenhuma mensagem em '{}'.", path);
        return Ok(());
    }

    println!("Últimas {} mensagem(ns) em '{}':\n", messages.len(), path);
    for (timestamp, chat, user, text) in messages {
        let user = if user.is_empty() {
            "-".to_string()
        } else {
            format!("@{user}")
        };
        println!("{timestamp} | {chat} | {user} | {text}");
    }
    Ok(())
}

/// Gera uma cópia do banco pronta para visualizadores SQLite (comando `export`).
async fn export_messages(target: Option<String>) -> Result<(), String> {
    let dotenv = load_dotenv(".env");
    let path = env_or_dotenv(&dotenv, "DB_PATH").unwrap_or_else(|| "messages.db".to_string());
    let target = target.unwrap_or_else(|| "messages-export.db".to_string());

    let store = MessageStore::open(&path).await?;
    store.export(&target).await?;
    println!("Exportado para '{target}'. Pode abrir em qualquer visualizador SQLite.");
    Ok(())
}

#[tokio::main]
async fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        Some("dump") => dump_messages().await,
        Some("export") => export_messages(std::env::args().nth(2)).await,
        _ => run().await,
    };
    if let Err(e) = result {
        eprintln!("[erro] {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_db() -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir()
            .join(format!(
                "cti-test-{}-{}.db",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn sample() -> super::MonitoredMessage {
        super::MonitoredMessage {
            message_id: 7,
            chat_id: -100123,
            chat_title: "Grupo".into(),
            chat_username: Some("grupo".into()),
            chat_kind: "channel".into(),
            timestamp: "2026-11-19T12:00:00Z".into(),
            message_text: "texto http://exemplo.com".into(),
            message_type: "text".into(),
            action_type: "post".into(),
            has_links: true,
            has_hashes: false,
            sender_id: 42,
            sender_username: Some("fulano".into()),
            sender_first_name: Some("Fulano".into()),
            sender_last_name: Some("De Tal".into()),
            sender_phone: None,
            sender_is_bot: false,
            sender_is_verified: false,
            sender_is_scam: false,
            sender_is_deleted: false,
            media_name: None,
            media_mime: None,
            media_size: None,
            media_duration: None,
            geo_lat: None,
            geo_lon: None,
            venue_title: None,
            venue_address: None,
            contact_name: None,
            contact_phone: None,
            poll_question: None,
            poll_answers: None,
            forward_from_id: None,
            forward_from_name: None,
            forward_date: None,
            reply_to_message_id: None,
            reply_quote: None,
            view_count: Some(10),
            forward_count: Some(2),
            reply_count: Some(1),
            reaction_count: Some(3),
            grouped_id: None,
            post_author: None,
            edit_date: None,
        }
    }

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal"));
        let _ = std::fs::remove_file(format!("{path}-shm"));
    }

    #[tokio::test]
    async fn store_grava_e_ignora_duplicata() {
        let path = temp_db();
        let store = super::MessageStore::open(&path).await.unwrap();

        store.insert(&sample()).await.unwrap();
        store.insert(&sample()).await.unwrap(); // catch-up repetido não duplica

        let rows = store.recent(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "texto http://exemplo.com");

        let mut pragma = store.conn.query("PRAGMA journal_mode", ()).await.unwrap();
        let mode: String = pragma.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        cleanup(&path);
    }

    #[tokio::test]
    async fn duas_conexoes_leem_e_escrevem_juntas() {
        let path = temp_db();
        let writer = super::MessageStore::open(&path).await.unwrap();
        let reader = super::MessageStore::open(&path).await.unwrap();

        writer.insert(&sample()).await.unwrap();
        assert_eq!(reader.recent(1).await.unwrap().len(), 1);

        cleanup(&path);
    }

    #[tokio::test]
    async fn export_gera_copia_sem_wal() {
        let path = temp_db();
        let out = temp_db();
        let store = super::MessageStore::open(&path).await.unwrap();
        store.insert(&sample()).await.unwrap();
        store.export(&out).await.unwrap();

        let db = libsql::Builder::new_local(&out).build().await.unwrap();
        let conn = db.connect().unwrap();

        let mut rows = conn
            .query("SELECT message_text FROM messages", ())
            .await
            .unwrap();
        let text: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(text, "texto http://exemplo.com");

        let mut pragma = conn.query("PRAGMA journal_mode", ()).await.unwrap();
        let mode: String = pragma.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(mode.to_lowercase(), "delete");

        cleanup(&path);
        cleanup(&out);
    }

    #[tokio::test]
    async fn migra_banco_criado_pela_versao_antiga() {
        let path = temp_db();
        {
            let db = libsql::Builder::new_local(&path).build().await.unwrap();
            db.connect()
                .unwrap()
                .execute_batch(
                    "CREATE TABLE messages (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        message_id INTEGER NOT NULL,
                        sender_id INTEGER,
                        sender_username TEXT,
                        chat_id INTEGER NOT NULL,
                        chat_title TEXT,
                        timestamp TEXT,
                        message_text TEXT,
                        message_type TEXT,
                        has_links INTEGER,
                        has_hashes INTEGER,
                        action_type TEXT,
                        saved_at TEXT DEFAULT CURRENT_TIMESTAMP,
                        UNIQUE(chat_id, message_id)
                    );",
                )
                .await
                .unwrap();
        }

        let store = super::MessageStore::open(&path).await.unwrap();
        store.insert(&sample()).await.unwrap();

        let mut rows = store
            .conn
            .query("SELECT sender_first_name, view_count FROM messages", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "Fulano");
        assert_eq!(row.get::<i64>(1).unwrap(), 10);

        cleanup(&path);
    }

    #[test]
    fn colunas_e_valores_na_mesma_ordem() {
        assert_eq!(super::MESSAGE_COLUMNS.len(), sample().row_values().len());
    }
}
