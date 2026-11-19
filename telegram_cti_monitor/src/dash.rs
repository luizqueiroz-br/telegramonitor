//! Dashboard de terminal que lê apenas o `messages.db` local.
//!
//! Não fala com o Telegram nem com a API CTI: o monitor grava no SQLite e o
//! dashboard mostra o que já foi gravado, atualizando por polling.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Sparkline};

use crate::stats::{self, Stats, Window};
use crate::{env_or_dotenv, load_dotenv, MessageStore};

const POLL_EVERY: Duration = Duration::from_secs(2);
const STATS_EVERY: Duration = Duration::from_secs(5);
const FEED_LIMIT: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Feed,
    Stats,
}

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
    view: View,
    window: Window,
    stats: Option<Stats>,
    stats_at: Instant,
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
            view: View::Feed,
            window: Window::Hour24,
            stats: None,
            stats_at: Instant::now(),
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
                    if app.view == View::Stats && app.stats_at.elapsed() >= STATS_EVERY {
                        if let Err(e) = refresh_stats(&mut app, &store).await {
                            app.status = format!("erro: {e}");
                        }
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
                        KeyCode::Char('s') => {
                            app.view = match app.view {
                                View::Feed => View::Stats,
                                View::Stats => View::Feed,
                            };
                            if app.view == View::Stats {
                                if let Err(e) = refresh_stats(&mut app, &store).await {
                                    app.status = format!("erro: {e}");
                                }
                            }
                        }
                        KeyCode::Char('t') => {
                            app.window = app.window.next();
                            app.stats_at = Instant::now();
                            if app.view == View::Stats {
                                if let Err(e) = refresh_stats(&mut app, &store).await {
                                    app.status = format!("erro: {e}");
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Err(e) = reload(&mut app, &store).await {
                                app.status = format!("erro: {e}");
                            }
                            if app.view == View::Stats {
                                if let Err(e) = refresh_stats(&mut app, &store).await {
                                    app.status = format!("erro: {e}");
                                }
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

async fn refresh_stats(app: &mut App, store: &MessageStore) -> Result<(), String> {
    app.stats_at = Instant::now();
    app.stats = Some(stats::collect(store, app.window).await?);
    Ok(())
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
    let hint = match app.view {
        View::Feed => "s estatísticas",
        View::Stats => "s ao vivo",
    };
    let header = vec![
        Line::from(format!(
            "Mensagens {}  ·  Chats {}  ·  Remetentes {}",
            app.total, app.chats, app.senders
        )),
        Line::from(vec![
            Span::styled(mode, Style::new().fg(if app.paused { Color::Yellow } else { Color::Green })),
            Span::raw(format!(
                " · janela {} · {hint} · t janela · q sair · p pausa · r recarrega",
                app.window.label()
            )),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(header).block(Block::bordered().title(" Telegram CTI Monitor ")),
        chunks[0],
    );

    if app.view == View::Stats {
        ui_stats(frame, app, chunks[1]);
        return;
    }

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

fn ui_stats(frame: &mut Frame, app: &App, area: Rect) {
    let Some(data) = &app.stats else {
        frame.render_widget(
            Paragraph::new("Calculando estatísticas…")
                .alignment(Alignment::Center)
                .block(Block::bordered().title(" Estatísticas ")),
            area,
        );
        return;
    };

    let cols = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(33),
        Constraint::Percentage(33),
    ])
    .split(area);
    let left = Layout::vertical([Constraint::Length(6), Constraint::Min(3)]).split(cols[0]);
    let mid = Layout::vertical([Constraint::Length(12), Constraint::Min(3)]).split(cols[1]);
    let right = Layout::vertical([Constraint::Length(6), Constraint::Min(3)]).split(cols[2]);

    let o = &data.overview;
    let pct = if o.total > 0 { o.links * 100 / o.total } else { 0 };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Total {} · Chats {}", o.total, o.chats)),
            Line::from(format!("Remetentes {}", o.senders)),
            Line::from(format!("Links {} ({}%)", o.links, pct)),
            Line::from(format!("1 h {} · 24 h {}", o.last_hour, o.last_day)),
        ])
        .block(Block::bordered().title(" Visão geral ")),
        left[0],
    );

    frame.render_widget(
        Paragraph::new(row_lines(&data.top_chats, inner_width(left[1]), false))
            .block(Block::bordered().title(format!(" Top chats ({}) ", app.window.label()))),
        left[1],
    );
    frame.render_widget(
        Paragraph::new(row_lines(&data.top_senders, inner_width(mid[0]), true))
            .block(Block::bordered().title(format!(" Top remetentes ({}) ", app.window.label()))),
        mid[0],
    );
    frame.render_widget(
        Paragraph::new(type_lines(&data.types, inner_width(mid[1])))
            .block(Block::bordered().title(" Por tipo ")),
        mid[1],
    );

    let hours: Vec<u64> = data.hours.iter().map(|&count| count.max(0) as u64).collect();
    let peak = data.hours.iter().enumerate().max_by_key(|(_, count)| **count);
    let subtitle = match peak {
        Some((hour, count)) if *count > 0 => format!(" — pico {hour:02}h ({count})"),
        _ => " — sem mensagens".to_string(),
    };
    frame.render_widget(
        Sparkline::default()
            .data(hours)
            .style(Style::new().fg(Color::Cyan))
            .block(Block::bordered().title(format!(" Por hora UTC (24 h){subtitle} "))),
        right[0],
    );

    frame.render_widget(
        Paragraph::new(row_lines(&data.domains, inner_width(right[1]), false))
            .block(Block::bordered().title(format!(" Domínios ({}) ", app.window.label()))),
        right[1],
    );
}

fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

fn row_lines(rows: &[stats::Row], width: usize, with_chats: bool) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from("—")];
    }
    rows.iter()
        .map(|row| {
            if with_chats {
                let label_w = width.saturating_sub(13).max(4);
                Line::from(format!(
                    "{:<label_w$} {:>5} {:>4}c",
                    truncate(&row.label, label_w),
                    row.count,
                    row.chats
                ))
            } else {
                let label_w = width.saturating_sub(8).max(4);
                Line::from(format!(
                    "{:<label_w$} {:>6}",
                    truncate(&row.label, label_w),
                    row.count
                ))
            }
        })
        .collect()
}

fn type_lines(types: &[stats::TypeCount], width: usize) -> Vec<Line<'static>> {
    if types.is_empty() {
        return vec![Line::from("—")];
    }
    let max = types.iter().map(|t| t.count).max().unwrap_or(1).max(1);
    types
        .iter()
        .map(|kind| {
            let kind_w = (width / 3).clamp(4, 10);
            let bar_w = width.saturating_sub(kind_w + 9).max(1);
            let filled = ((kind.count as f64 / max as f64) * bar_w as f64).ceil() as usize;
            Line::from(vec![
                Span::styled(
                    format!("{:<kind_w$} ", truncate(&kind.kind, kind_w)),
                    Style::new().fg(Color::Magenta),
                ),
                Span::raw(format!("{:>5} ", kind.count)),
                Span::styled("█".repeat(filled.min(bar_w)), Style::new().fg(Color::Cyan)),
            ])
        })
        .collect()
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

    fn sample_stats() -> Stats {
        let mut hours = vec![0; 24];
        hours[14] = 7;
        Stats {
            overview: stats::Overview {
                total: 10,
                chats: 2,
                senders: 3,
                links: 2,
                last_hour: 5,
                last_day: 9,
            },
            top_chats: vec![stats::Row {
                label: "Chat A".into(),
                count: 6,
                chats: 0,
            }],
            top_senders: vec![stats::Row {
                label: "ana".into(),
                count: 4,
                chats: 2,
            }],
            types: vec![
                stats::TypeCount { kind: "text".into(), count: 8 },
                stats::TypeCount { kind: "link".into(), count: 2 },
            ],
            hours,
            domains: vec![stats::Row {
                label: "exemplo.com".into(),
                count: 2,
                chats: 0,
            }],
        }
    }

    fn screen_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn renderiza_os_dois_modos_em_80x24() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut app = App::new();
        app.total = 1;
        app.feed.push(FeedRow {
            id: 1,
            time: "14:30".into(),
            chat: "Chat".into(),
            sender: "ana".into(),
            kind: "text".into(),
            text: "oi http://exemplo.com".into(),
        });
        terminal.draw(|frame| ui(frame, &app)).unwrap();
        assert!(screen_text(&terminal).contains("Ao vivo"));

        app.view = View::Stats;
        app.stats = Some(sample_stats());
        terminal.draw(|frame| ui(frame, &app)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("Visão geral"));
        assert!(screen.contains("Top chats"));
        assert!(screen.contains("Top remetentes"));
        assert!(screen.contains("Por hora UTC"));
        assert!(screen.contains("Por tipo"));
        assert!(screen.contains("Domínios"));
    }
}
