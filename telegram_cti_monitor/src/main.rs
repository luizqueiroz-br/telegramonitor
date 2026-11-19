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
use grammers_client::peer::Peer;
use grammers_client::sender::UpdatesConfiguration;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
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

/// Dados de uma mensagem monitorada - salvar na API CTI
#[derive(Debug, Serialize, Deserialize, Clone)]
struct MonitoredMessage {
    message_id: i64,
    sender_id: i64,
    sender_username: Option<String>,
    chat_id: i64,
    chat_title: String,
    timestamp: String, // ISO 8601: 2026-11-14T14:30:00Z
    message_text: String,
    message_type: String, // "text", "photo", "document", "link", ...
    has_links: bool,
    has_hashes: bool,
    action_type: String, // "post", "reply", "forward"
}

// ============================================================
// BANCO LOCAL (SQLite)
// ============================================================

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
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
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
            )",
            (),
        )
        .await
        .map_err(|e| format!("Falha ao criar a tabela messages: {e}"))?;
        Ok(Self { conn })
    }

    /// Grava a mensagem. Mensagens repetidas (catch-up) são ignoradas.
    async fn insert(&self, msg: &MonitoredMessage) -> Result<(), String> {
        let username = match &msg.sender_username {
            Some(u) => libsql::Value::Text(u.clone()),
            None => libsql::Value::Null,
        };
        self.conn
            .execute(
                "INSERT OR IGNORE INTO messages (
                    message_id, sender_id, sender_username, chat_id, chat_title,
                    timestamp, message_text, message_type, has_links, has_hashes, action_type
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                libsql::params![
                    msg.message_id,
                    msg.sender_id,
                    username,
                    msg.chat_id,
                    msg.chat_title.clone(),
                    msg.timestamp.clone(),
                    msg.message_text.clone(),
                    msg.message_type.clone(),
                    msg.has_links,
                    msg.has_hashes,
                    msg.action_type.clone()
                ],
            )
            .await
            .map_err(|e| format!("Falha ao gravar no banco: {e}"))?;
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

async fn handle_message(config: &Config, store: &MessageStore, message: grammers_client::update::Message) {
    let chat_id = message
        .peer_id()
        .bot_api_dialog_id()
        .unwrap_or_default();

    if !config.monitored_chats.is_empty() && !config.monitored_chats.contains(&chat_id) {
        return;
    }

    let chat_title = message
        .peer()
        .and_then(|p| p.name())
        .unwrap_or("Desconhecido")
        .to_string();

    let text = message.text();
    let has_links =
        text.contains("http://") || text.contains("https://") || text.contains("t.me/");
    let message_type = media_type(message.media());

    let sender = message.sender();
    let sender_username = match sender {
        Some(Peer::User(user)) => user.username().map(|s| s.to_string()),
        _ => None,
    };
    let sender_first_name = match sender {
        Some(Peer::User(user)) => user.full_name(),
        _ => "Desconhecido".to_string(),
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

    let monitored_msg = MonitoredMessage {
        message_id: message.id() as i64,
        sender_id,
        sender_username: sender_username.clone(),
        chat_id,
        chat_title: chat_title.clone(),
        timestamp: message.date().to_string(),
        message_text: text.to_string(),
        message_type: message_type.to_string(),
        has_links,
        has_hashes: false, // TODO: extrair hashes de documentos/arquivos
        action_type: action_type.to_string(),
    };

    if let Err(e) = store.insert(&monitored_msg).await {
        eprintln!("[ERRO] {e}");
    }

    if let Some(cti_url) = &config.cti_url {
        if let Err(e) = send_to_cti_api(cti_url, &monitored_msg).await {
            eprintln!("[ERRO] Falha ao enviar para API CTI: {}", e);
        }
    }

    println!("[+] {} [{}]:", chat_title, message_type);
    match &sender_username {
        Some(un) => println!("    Remetente: {} (@{})", sender_first_name, un),
        None => println!("    Remetente: {}", sender_first_name),
    }
    println!("    ID do remetente: {}", sender_id);
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
                    handle_message(&config, &store, message).await;
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
            sender_id: 42,
            sender_username: Some("fulano".into()),
            chat_id: -100123,
            chat_title: "Grupo".into(),
            timestamp: "2026-11-19T12:00:00Z".into(),
            message_text: "texto http://exemplo.com".into(),
            message_type: "text".into(),
            has_links: true,
            has_hashes: false,
            action_type: "post".into(),
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
}
