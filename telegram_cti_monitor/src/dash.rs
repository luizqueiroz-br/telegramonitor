//! Dashboard de terminal que lê apenas o `messages.db` local.
//!
//! Não fala com o Telegram nem com a API CTI: o monitor grava no SQLite e o
//! dashboard mostra o que já foi gravado, atualizando por polling.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::{env_or_dotenv, load_dotenv, MessageStore};

const POLL_EVERY: Duration = Duration::from_secs(2);
const FEED_LIMIT: usize = 200;

struct FeedRow {
    id: i64,
    time: String,
    chat: String,
    sender: String,
    kind: String,
    text: String,
}

struct App {
    feed: Vec<FeedRow>, // ordem crescente de id; renderiza invertido
    last_id: i64,
    total: i64,
    chats: i64,
    senders: i64,
    paused: bool,
    status: String,
}

impl App {
    fn new() -> Self {
        Self {
            feed: Vec::new(),
            last_id: 0,
            total: 0,
            chats: 0,
            senders: 0,
            paused: false,
            status: String::new(),
        }
    }
}

/// Garante a restauração do terminal em qualquer saída (inclusive erro).
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub async fn run() -> Result<(), String> {
    let dotenv = load_dotenv(".env");
    let path = env_or_dotenv(&dotenv, "DB_PATH").unwrap_or_else(|| "messages.db".to_string());
    let store = MessageStore::open(&path).await?;

    let mut app = App::new();
    reload(&mut app, &store).await?;

    let mut terminal = ratatui::init();
    let _restore = Restore;
    let mut events = spawn_events();
    // O primeiro tick do interval é imediato; depois espera POLL_EVERY.
    let mut ticker = tokio::time::interval(POLL_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_poll = Instant::now();

    loop {
        terminal
            .draw(|frame| ui(frame, &app))
            .map_err(|e| format!("Falha ao desenhar o dashboard: {e}"))?;

        tokio::select! {
            _ = ticker.tick() => {
                if !app.paused && last_poll.elapsed() >= POLL_EVERY {
                    last_poll = Instant::now();
                    if let Err(e) = refresh(&mut app, &store).await {
                        app.status = format!("erro: {e}");
                    }
                }
            }
            maybe_event = events.recv() => {
                match maybe_event {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('p') => {
                            app.paused = !app.paused;
                            last_poll = Instant::now();
                        }
                        KeyCode::Char('r') => {
                            if let Err(e) = reload(&mut app, &store).await {
                                app.status = format!("erro: {e}");
                            }
                        }
                        _ => {}
                    },
                    Some(_) => {} // resize etc.: o próximo draw já usa a área nova
                    None => break, // thread de eventos encerrou
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    Ok(())
}

/// Lê eventos do terminal numa thread própria (a leitura é bloqueante).
fn spawn_events() -> tokio::sync::mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(event) => {
                if tx.send(event).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });
    rx
}

/// Recarrega do zero: as últimas FEED_LIMIT mensagens e os totais.
async fn reload(app: &mut App, store: &MessageStore) -> Result<(), String> {
    app.feed = recent_rows(store, FEED_LIMIT as u32).await?;
    app.last_id = app.feed.last().map(|row| row.id).unwrap_or(0);
    let (total, chats, senders) = totals(store).await?;
    app.total = total;
    app.chats = chats;
    app.senders = senders;
    app.status.clear();
    Ok(())
}

/// Busca só o que chegou depois de `last_id` e mantém a janela do feed.
async fn refresh(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let rows = live_rows(store, app.last_id, FEED_LIMIT as u32).await?;
    for row in rows {
        app.last_id = app.last_id.max(row.id);
        app.feed.push(row);
    }
    if app.feed.len() > FEED_LIMIT {
        let excess = app.feed.len() - FEED_LIMIT;
        app.feed.drain(..excess);
    }
    let (total, chats, senders) = totals(store).await?;
    app.total = total;
    app.chats = chats;
    app.senders = senders;
    app.status.clear();
    Ok(())
}

const ROW_SELECT: &str = "SELECT id, timestamp, COALESCE(chat_title, ''),
        COALESCE(sender_first_name, ''), COALESCE(sender_username, ''),
        COALESCE(message_type, ''), COALESCE(message_text, '')
    FROM messages";

async fn live_rows(store: &MessageStore, after_id: i64, limit: u32) -> Result<Vec<FeedRow>, String> {
    let mut rows = store
        .conn
        .query(
            &format!("{ROW_SELECT} WHERE id > ? ORDER BY id LIMIT ?"),
            libsql::params![after_id, limit],
        )
        .await
        .map_err(|e| format!("Falha ao ler mensagens: {e}"))?;
    collect_rows(&mut rows).await
}

async fn recent_rows(store: &MessageStore, limit: u32) -> Result<Vec<FeedRow>, String> {
    let mut rows = store
        .conn
        .query(
            &format!("SELECT * FROM ({ROW_SELECT} ORDER BY id DESC LIMIT ?) ORDER BY id ASC"),
            libsql::params![limit],
        )
        .await
        .map_err(|e| format!("Falha ao ler mensagens: {e}"))?;
    collect_rows(&mut rows).await
}

async fn collect_rows(rows: &mut libsql::Rows) -> Result<Vec<FeedRow>, String> {
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao ler mensagens: {e}"))?
    {
        let text = |i: i32| row.get::<String>(i).unwrap_or_default();
        let first_name = text(3);
        let username = text(4);
        let sender = match (first_name.is_empty(), username.is_empty()) {
            (false, false) => format!("{first_name} (@{username})"),
            (false, true) => first_name,
            (true, false) => format!("@{username}"),
            (true, true) => "—".to_string(),
        };
        out.push(FeedRow {
            id: row.get(0).map_err(|e| format!("id inválido: {e}"))?,
            time: short_time(&text(1)),
            chat: text(2),
            sender,
            kind: text(5),
            text: text(6),
        });
    }
    Ok(out)
}

async fn totals(store: &MessageStore) -> Result<(i64, i64, i64), String> {
    let mut rows = store
        .conn
        .query(
            "SELECT COUNT(*), COUNT(DISTINCT chat_id), COUNT(DISTINCT sender_id) FROM messages",
            (),
        )
        .await
        .map_err(|e| format!("Falha ao contar mensagens: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao contar mensagens: {e}"))?
        .ok_or("Contagem sem resultado")?;
    Ok((
        row.get(0).unwrap_or(0),
        row.get(1).unwrap_or(0),
        row.get(2).unwrap_or(0),
    ))
}

/// `2026-11-19T14:30:00Z` → `14:30`.
fn short_time(timestamp: &str) -> String {
    timestamp.get(11..16).unwrap_or(timestamp).to_string()
}

fn ui(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).split(area);

    let mode = if app.paused { "⏸ pausado" } else { "● ao vivo" };
    let header = vec![
        Line::from(format!(
            "Mensagens {}  ·  Chats {}  ·  Remetentes {}",
            app.total, app.chats, app.senders
        )),
        Line::from(vec![
            Span::styled(mode, Style::new().fg(if app.paused { Color::Yellow } else { Color::Green })),
            Span::raw("   q sair · p pausar · r recarregar"),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(header).block(Block::bordered().title(" Telegram CTI Monitor ")),
        chunks[0],
    );

    let block = Block::bordered().title(" Ao vivo ");
    if app.total == 0 {
        frame.render_widget(
            Paragraph::new("Nenhuma mensagem ainda. Aguardando o monitor gravar no banco…")
                .alignment(Alignment::Center)
                .block(block),
            chunks[1],
        );
        return;
    }
    if app.feed.is_empty() {
        frame.render_widget(
            Paragraph::new(format!("Mensagens: {} — nada nas últimas consultas.", app.total))
                .block(block),
            chunks[1],
        );
        return;
    }

    let visible = chunks[1].height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .feed
        .iter()
        .rev()
        .take(visible)
        .map(feed_line)
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), chunks[1]);
}

fn feed_line(row: &FeedRow) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{} ", row.time), Style::new().fg(Color::DarkGray)),
        Span::styled(
            format!("{:<18} ", truncate(&row.chat, 18)),
            Style::new().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{:<16} ", truncate(&row.sender, 16)),
            Style::new().fg(Color::Green),
        ),
        Span::styled(format!("[{:<8}] ", row.kind), Style::new().fg(Color::Magenta)),
    ];
    spans.extend(text_spans(&row.text));
    Line::from(spans)
}

/// Destaca links em amarelo; o resto do texto fica normal.
fn text_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, word) in text.split_whitespace().take(40).enumerate() {
        let link = word.contains("http://") || word.contains("https://") || word.contains("t.me/");
        let style = if link {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::UNDERLINED)
        } else {
            Style::new()
        };
        spans.push(Span::styled(
            if i == 0 { word.to_string() } else { format!(" {word}") },
            style,
        ));
    }
    spans
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn temp_db() -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir()
            .join(format!(
                "cti-dash-{}-{}.db",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    async fn insert(store: &MessageStore, message_id: i64, chat_id: i64, sender_id: i64, text: &str) {
        store
            .conn
            .execute(
                "INSERT INTO messages (message_id, chat_id, chat_title, timestamp,
                    message_text, message_type, has_links, sender_id,
                    sender_first_name, sender_username)
                 VALUES (?, ?, 'Grupo', '2026-11-19T12:00:00Z', ?, 'text', 0, ?, 'Fulano', 'fulano')",
                libsql::params![message_id, chat_id, text, sender_id],
            )
            .await
            .unwrap();
    }

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal"));
        let _ = std::fs::remove_file(format!("{path}-shm"));
    }

    #[tokio::test]
    async fn feed_le_so_o_que_chegou_depois_do_ultimo_id() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "primeira").await;
        insert(&store, 2, -100, 42, "segunda").await;

        assert_eq!(live_rows(&store, 0, 10).await.unwrap().len(), 2);
        let novas = live_rows(&store, 1, 10).await.unwrap();
        assert_eq!(novas.len(), 1);
        assert_eq!(novas[0].text, "segunda");

        let (total, chats, senders) = totals(&store).await.unwrap();
        assert_eq!((total, chats, senders), (2, 1, 1));

        cleanup(&path);
    }

    #[tokio::test]
    async fn recent_traz_as_ultimas_em_ordem_crescente() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        for id in 1..=5 {
            insert(&store, id, -100, 42, &format!("msg {id}")).await;
        }

        let rows = recent_rows(&store, 3).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["msg 3", "msg 4", "msg 5"]
        );

        cleanup(&path);
    }

    #[test]
    fn hora_curta_nao_quebra_timestamp_inesperado() {
        assert_eq!(short_time("2026-11-19T14:30:00Z"), "14:30");
        assert_eq!(short_time("sem hora"), "sem hora");
    }
}
