//! Dashboard de terminal que lê apenas o `messages.db` local.
//!
//! Não fala com o Telegram nem com a API CTI: o monitor grava no SQLite e o
//! dashboard mostra o que já foi gravado, atualizando por polling.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph, Sparkline, Wrap};

use crate::agent::log_action;
use crate::stats::{self, Stats, Window};
use crate::{MESSAGE_COLUMNS, MessageStore, env_or_dotenv, load_dotenv};

const POLL_EVERY: Duration = Duration::from_secs(2);
const STATS_EVERY: Duration = Duration::from_secs(5);
const FEED_LIMIT: usize = 200;
const LIST_LIMIT: usize = 1000;
const AGENT_LIMIT: usize = 500;

const TYPE_OPTIONS: &[&str] = &[
    "", "text", "photo", "document", "sticker", "link", "poll", "location", "contact", "other",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Feed,
    Stats,
    Agent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    Search,
    ChatPicker,
    TypePicker,
    Detail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Continue,
    Quit,
}

#[derive(Clone)]
struct ChatOption {
    id: i64,
    title: String,
}

#[derive(Clone, Default)]
struct Filters {
    chat: Option<ChatOption>,
    kind: Option<String>,
    links_only: bool,
    hide_ads: bool,
    search: Option<String>,
}

impl Filters {
    fn active(&self) -> bool {
        self.chat.is_some()
            || self.kind.is_some()
            || self.links_only
            || self.hide_ads
            || self.search.is_some()
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(chat) = &self.chat {
            parts.push(format!("chat={}", truncate(&chat.title, 24)));
        }
        if let Some(kind) = &self.kind {
            parts.push(format!("tipo={kind}"));
        }
        if self.links_only {
            parts.push("links".to_string());
        }
        if self.hide_ads {
            parts.push("sem anúncios".to_string());
        }
        if let Some(search) = &self.search {
            parts.push(format!("busca=\"{}\"", truncate(search, 24)));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("filtros: {} — c limpa", parts.join(" · "))
        }
    }
}

struct FeedRow {
    id: i64,
    time: String,
    chat: String,
    sender: String,
    kind: String,
    msg_kind: String,
    text: String,
}

#[derive(Clone)]
struct AgentRow {
    id: i64,
    link: String,
    status: String,
    verdict: String,
    target_title: String,
    source_chat: String,
    created_at: String,
    checked_at: String,
    score: Option<f64>,
    reason: String,
    error: String,
    model: String,
    tries: i64,
    manual_approved: bool,
    force_join: bool,
}

struct App {
    feed: Vec<FeedRow>, // ordem crescente de id; renderiza invertido
    last_id: i64,
    total: i64,
    chats: i64,
    senders: i64,
    rows_total: i64,
    paused: bool,
    status: String,
    view: View,
    window: Window,
    stats: Option<Stats>,
    stats_at: Instant,

    mode: Mode,
    filters: Filters,
    selected: usize,    // 0 = mensagem mais nova visível
    list_offset: usize, // 0 = topo (mais nova)
    list_height: usize,
    input: String,
    chats_catalog: Vec<ChatOption>,
    picker: usize,
    detail: Option<Vec<(String, String)>>,
    detail_scroll: u16,

    agent_rows: Vec<AgentRow>,
    agent_selected: usize,
    agent_offset: usize,
    agent_height: usize,
    agent_show_all: bool,
    agent_all_total: i64,
    agent_summary: String,
}

impl App {
    fn new() -> Self {
        Self {
            feed: Vec::new(),
            last_id: 0,
            total: 0,
            chats: 0,
            senders: 0,
            rows_total: 0,
            paused: false,
            status: String::new(),
            view: View::Feed,
            window: Window::Hour24,
            stats: None,
            stats_at: Instant::now(),
            mode: Mode::Browse,
            filters: Filters::default(),
            selected: 0,
            list_offset: 0,
            list_height: 1,
            input: String::new(),
            chats_catalog: Vec::new(),
            picker: 0,
            detail: None,
            detail_scroll: 0,
            agent_rows: Vec::new(),
            agent_selected: 0,
            agent_offset: 0,
            agent_height: 1,
            agent_show_all: false,
            agent_all_total: 0,
            agent_summary: String::new(),
        }
    }

    fn selected_row(&self) -> Option<&FeedRow> {
        self.feed.iter().rev().nth(self.selected)
    }

    fn selected_agent(&self) -> Option<&AgentRow> {
        self.agent_rows.get(self.agent_selected)
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.feed.len().saturating_sub(1));
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let height = self.list_height.max(1);
        if self.selected < self.list_offset {
            self.list_offset = self.selected;
        }
        if self.selected >= self.list_offset + height {
            self.list_offset = self.selected + 1 - height;
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
    app.chats_catalog = chat_options(&store).await?;
    reload(&mut app, &store).await?;
    reload_agents(&mut app, &store).await?;

    let mut terminal = ratatui::init();
    let _restore = Restore;
    let mut events = spawn_events();
    let mut ticker = tokio::time::interval(POLL_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        terminal
            .draw(|frame| ui(frame, &mut app))
            .map_err(|e| format!("Falha ao desenhar o dashboard: {e}"))?;

        tokio::select! {
            _ = ticker.tick() => {
                if !app.paused {
                    // Na aba do agente o feed não aparece; pular evita apagar a
                    // mensagem de status (aprovação/rejeição) a cada 2 s.
                    if app.view != View::Agent
                        && let Err(e) = refresh(&mut app, &store).await
                    {
                        app.status = format!("erro: {e}");
                    }
                    if app.view == View::Stats && app.stats_at.elapsed() >= STATS_EVERY {
                        if let Err(e) = refresh_stats(&mut app, &store).await {
                            app.status = format!("erro: {e}");
                        }
                    }
                    if app.view == View::Agent
                        && let Err(e) = reload_agents(&mut app, &store).await
                    {
                        app.status = format!("erro: {e}");
                    }
                }
            }
            maybe_event = events.recv() => {
                match maybe_event {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        match handle_key(&mut app, key.code, key.modifiers, &store).await {
                            Ok(Action::Quit) => break,
                            Ok(Action::Continue) => {}
                            Err(e) => app.status = format!("erro: {e}"),
                        }
                    }
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
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

async fn handle_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    store: &MessageStore,
) -> Result<Action, String> {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Ok(Action::Quit);
    }

    match app.mode {
        Mode::Browse => browse_key(app, code, store).await,
        Mode::Search => {
            match code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Enter => {
                    let term = app.input.trim();
                    app.filters.search = (!term.is_empty()).then(|| term.to_string());
                    app.mode = Mode::Browse;
                    reload(app, store).await?;
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Char(c) => app.input.push(c),
                _ => {}
            }
            Ok(Action::Continue)
        }
        Mode::ChatPicker => {
            match code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Enter => {
                    let chosen = filtered_chats(app).get(app.picker).cloned();
                    if let Some(chat) = chosen {
                        app.filters.chat = Some(chat);
                        app.mode = Mode::Browse;
                        reload(app, store).await?;
                    }
                }
                KeyCode::Backspace => {
                    app.input.pop();
                    app.picker = 0;
                }
                KeyCode::Up => app.picker = app.picker.saturating_sub(1),
                KeyCode::Down => {
                    let len = filtered_chats(app).len();
                    app.picker = (app.picker + 1).min(len.saturating_sub(1));
                }
                KeyCode::Char(c) => {
                    app.input.push(c);
                    app.picker = 0;
                }
                _ => {}
            }
            Ok(Action::Continue)
        }
        Mode::TypePicker => {
            match code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Enter => {
                    let value = TYPE_OPTIONS[app.picker.min(TYPE_OPTIONS.len() - 1)];
                    app.filters.kind = (!value.is_empty()).then(|| value.to_string());
                    app.mode = Mode::Browse;
                    reload(app, store).await?;
                }
                KeyCode::Up => app.picker = app.picker.saturating_sub(1),
                KeyCode::Down => {
                    app.picker = (app.picker + 1).min(TYPE_OPTIONS.len() - 1);
                }
                _ => {}
            }
            Ok(Action::Continue)
        }
        Mode::Detail => {
            match code {
                KeyCode::Esc | KeyCode::Enter => app.mode = Mode::Browse,
                KeyCode::Char('q') => return Ok(Action::Quit),
                KeyCode::Up => app.detail_scroll = app.detail_scroll.saturating_sub(1),
                KeyCode::Down => app.detail_scroll = app.detail_scroll.saturating_add(1),
                KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(10),
                KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(10),
                KeyCode::Home => app.detail_scroll = 0,
                KeyCode::End => {
                    let len = app.detail.as_ref().map(|d| d.len()).unwrap_or(0);
                    app.detail_scroll = len.saturating_sub(1) as u16;
                }
                _ => {}
            }
            let max = app.detail.as_ref().map(|d| d.len()).unwrap_or(0);
            if max > 0 {
                app.detail_scroll = app.detail_scroll.min((max - 1) as u16);
            }
            Ok(Action::Continue)
        }
    }
}

async fn browse_key(app: &mut App, code: KeyCode, store: &MessageStore) -> Result<Action, String> {
    match code {
        KeyCode::Char('q') => return Ok(Action::Quit),
        KeyCode::Char('a') => {
            app.view = if app.view == View::Agent {
                View::Feed
            } else {
                View::Agent
            };
            if app.view == View::Agent {
                reload_agents(app, store).await?;
            }
            return Ok(Action::Continue);
        }
        _ => {}
    }
    if app.view == View::Agent {
        return agent_key(app, code, store).await;
    }

    match code {
        KeyCode::Char('p') => app.paused = !app.paused,
        KeyCode::Char('s') => {
            app.view = if app.view == View::Feed {
                View::Stats
            } else {
                View::Feed
            };
            if app.view == View::Stats {
                refresh_stats(app, store).await?;
            }
        }
        KeyCode::Char('t') => {
            app.window = app.window.next();
            app.stats_at = Instant::now();
            reload(app, store).await?;
            if app.view == View::Stats {
                refresh_stats(app, store).await?;
            }
        }
        KeyCode::Char('l') => {
            app.filters.links_only = !app.filters.links_only;
            reload(app, store).await?;
        }
        KeyCode::Char('k') => {
            app.filters.hide_ads = !app.filters.hide_ads;
            reload(app, store).await?;
        }
        KeyCode::Char('/') => {
            app.mode = Mode::Search;
            app.input = app.filters.search.clone().unwrap_or_default();
        }
        KeyCode::Char('g') => {
            app.chats_catalog = chat_options(store).await?;
            app.input.clear();
            app.picker = 0;
            app.mode = Mode::ChatPicker;
        }
        KeyCode::Char('m') => {
            app.picker = match &app.filters.kind {
                Some(kind) => TYPE_OPTIONS
                    .iter()
                    .position(|option| *option == kind.as_str())
                    .unwrap_or(0),
                None => 0,
            };
            app.mode = Mode::TypePicker;
        }
        KeyCode::Char('c') => {
            app.filters = Filters::default();
            reload(app, store).await?;
        }
        KeyCode::Char('r') => {
            reload(app, store).await?;
            if app.view == View::Stats {
                refresh_stats(app, store).await?;
            }
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End => navigate(app, code),
        KeyCode::Enter if app.view == View::Feed => open_detail(app, store).await?,
        _ => {}
    }
    Ok(Action::Continue)
}

async fn agent_key(app: &mut App, code: KeyCode, store: &MessageStore) -> Result<Action, String> {
    match code {
        KeyCode::Char('p') => app.paused = !app.paused,
        KeyCode::Char('s') => {
            app.view = View::Stats;
            refresh_stats(app, store).await?;
        }
        KeyCode::Char('r') => reload_agents(app, store).await?,
        KeyCode::Char('f') => {
            app.agent_show_all = !app.agent_show_all;
            reload_agents(app, store).await?;
        }
        KeyCode::Char('y') => decide_agent(app, store, true).await?,
        KeyCode::Char('n') => decide_agent(app, store, false).await?,
        KeyCode::Char('x') => retry_agent(app, store).await?,
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End => navigate_agents(app, code),
        KeyCode::Enter => open_agent_detail(app, store).await?,
        _ => {}
    }
    Ok(Action::Continue)
}

async fn decide_agent(app: &mut App, store: &MessageStore, approve: bool) -> Result<(), String> {
    let Some(row) = app.selected_agent().cloned() else {
        return Ok(());
    };
    if matches!(row.status.as_str(), "joined" | "already" | "duplicate") {
        app.status = format!("'{}' não pode ser alterado", row.status);
        return Ok(());
    }
    if approve {
        let changed = store
            .conn
            .execute(
                "UPDATE group_candidates SET status = 'approved', manual_approved = 1,
                    force_join = 0, error = NULL, tries = 0
                 WHERE id = ? AND status NOT IN ('joined','already','duplicate')",
                libsql::params![row.id],
            )
            .await
            .map_err(|e| format!("Falha ao aprovar: {e}"))?;
        if changed == 0 {
            app.status = "status mudou no agente; recarregando".into();
            return reload_agents(app, store).await;
        }
        log_action(
            store,
            &row.link,
            "manual_approve",
            "aprovado no TUI",
            "manual",
        )
        .await?;
        app.status =
            "aprovado: o agente entra na próxima varredura (make run, dry-run=false)".into();
    } else {
        let changed = store
            .conn
            .execute(
                "UPDATE group_candidates SET status = 'skipped', manual_approved = 0,
                    force_join = 0, error = 'rejeitado manualmente', tries = 0
                 WHERE id = ? AND status NOT IN ('joined','already','duplicate')",
                libsql::params![row.id],
            )
            .await
            .map_err(|e| format!("Falha ao rejeitar: {e}"))?;
        if changed == 0 {
            app.status = "status mudou no agente; recarregando".into();
            return reload_agents(app, store).await;
        }
        log_action(
            store,
            &row.link,
            "manual_skip",
            "rejeitado no TUI",
            "manual",
        )
        .await?;
        app.status = "rejeitado".into();
    }
    reload_agents(app, store).await
}

/// Reenfileira o convite ignorando o orçamento de entradas por hora/dia.
/// FLOOD_WAIT e `AGENT_DRY_RUN` continuam valendo no agente.
async fn retry_agent(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let Some(row) = app.selected_agent().cloned() else {
        return Ok(());
    };
    if matches!(row.status.as_str(), "joined" | "already" | "duplicate") {
        app.status = format!("'{}' não pode ser alterado", row.status);
        return Ok(());
    }
    if row.status == "pending" {
        // Sem veredito ainda: entrar à força pularia a classificação.
        app.status = "pendente: aguarde a classificação antes do retry".into();
        return Ok(());
    }
    let changed = store
        .conn
        .execute(
            "UPDATE group_candidates SET status = 'approved', manual_approved = 1,
                force_join = 1, error = NULL, tries = 0
             WHERE id = ? AND status NOT IN ('joined','already','duplicate','pending')",
            libsql::params![row.id],
        )
        .await
        .map_err(|e| format!("Falha ao reenfileirar: {e}"))?;
    if changed == 0 {
        app.status = "status mudou no agente; recarregando".into();
        return reload_agents(app, store).await;
    }
    log_action(
        store,
        &row.link,
        "manual_retry",
        "retry forçado no TUI (ignora limites)",
        "manual",
    )
    .await?;
    app.status =
        "retry forçado: o agente tenta na próxima varredura, ignorando o orçamento".into();
    reload_agents(app, store).await
}

async fn open_agent_detail(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let Some(id) = app.selected_agent().map(|row| row.id) else {
        return Ok(());
    };
    app.detail = Some(load_agent_detail(store, id).await?);
    app.detail_scroll = 0;
    app.mode = Mode::Detail;
    Ok(())
}

fn navigate_agents(app: &mut App, code: KeyCode) {
    let len = app.agent_rows.len();
    if len == 0 {
        return;
    }
    let page = app.agent_height.max(1);
    match code {
        KeyCode::Up => app.agent_selected = app.agent_selected.saturating_sub(1),
        KeyCode::Down => app.agent_selected = (app.agent_selected + 1).min(len - 1),
        KeyCode::PageUp => app.agent_selected = app.agent_selected.saturating_sub(page),
        KeyCode::PageDown => app.agent_selected = (app.agent_selected + page).min(len - 1),
        KeyCode::Home => app.agent_selected = 0,
        KeyCode::End => app.agent_selected = len - 1,
        _ => {}
    }
    clamp_agent_selection(app);
}

fn clamp_agent_selection(app: &mut App) {
    app.agent_selected = app
        .agent_selected
        .min(app.agent_rows.len().saturating_sub(1));
    if app.agent_rows.is_empty() {
        app.agent_offset = 0;
        return;
    }
    let height = app.agent_height.max(1);
    if app.agent_selected < app.agent_offset {
        app.agent_offset = app.agent_selected;
    }
    if app.agent_selected >= app.agent_offset + height {
        app.agent_offset = app.agent_selected + 1 - height;
    }
}

fn navigate(app: &mut App, code: KeyCode) {
    let len = app.feed.len();
    if len == 0 {
        return;
    }
    let page = app.list_height.max(1);
    app.selected = match code {
        KeyCode::Up => app.selected.saturating_sub(1),
        KeyCode::Down => (app.selected + 1).min(len - 1),
        KeyCode::PageUp => app.selected.saturating_sub(page),
        KeyCode::PageDown => (app.selected + page).min(len - 1),
        KeyCode::Home => 0,
        KeyCode::End => len - 1,
        _ => app.selected,
    };
    app.ensure_visible();
}

async fn open_detail(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let Some(id) = app.selected_row().map(|row| row.id) else {
        return Ok(());
    };
    app.detail = Some(load_detail(store, id).await?);
    app.detail_scroll = 0;
    app.mode = Mode::Detail;
    Ok(())
}

/// Recarrega do zero mantendo a linha selecionada (por id).
async fn reload(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let keep_id = app.selected_row().map(|row| row.id);
    if app.filters.active() {
        app.feed = query_rows(store, &app.filters, app.window, LIST_LIMIT as u32).await?;
    } else {
        let cutoff = stats::cutoff_iso(app.window);
        app.feed = recent_rows(store, FEED_LIMIT as u32, cutoff.as_deref()).await?;
    }
    app.last_id = app.feed.last().map(|row| row.id).unwrap_or(0);
    app.rows_total = count_rows(store, &app.filters, app.window).await?;
    app.selected = keep_id
        .and_then(|id| app.feed.iter().rev().position(|row| row.id == id))
        .unwrap_or(0);
    app.clamp_selection();

    let (total, chats, senders) = totals(store).await?;
    app.total = total;
    app.chats = chats;
    app.senders = senders;
    app.status.clear();
    Ok(())
}

/// Atualiza a cada 2 s: feed ao vivo ou lista filtrada.
async fn refresh(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let prev_last = app.last_id;
    let mut new_last = prev_last;
    let mut pending: Vec<FeedRow> = Vec::new();
    if app.filters.active() {
        let keep_id = app.selected_row().map(|row| row.id);
        app.feed = query_rows(store, &app.filters, app.window, LIST_LIMIT as u32).await?;
        // O feed só traz o que casa com o filtro; MAX(id) (O(1) na PK) detecta
        // qualquer insert para o gate de count/totals valer também aqui.
        new_last = new_last.max(max_id(store).await?);
        app.selected = keep_id
            .and_then(|id| app.feed.iter().rev().position(|row| row.id == id))
            .unwrap_or(0);
        app.clamp_selection();
    } else {
        let cutoff = stats::cutoff_iso(app.window);
        pending = live_rows(store, prev_last, FEED_LIMIT as u32, cutoff.as_deref()).await?;
        for row in &pending {
            new_last = new_last.max(row.id);
        }
    }

    // Débito (sem filtro): janela deslize — linhas que saem da janela sem insert novo deixam rows_total defasado até o próximo insert ou `r`.
    // #28: com filtro o count roda a cada tick — UPDATE pós-insert não muda max_id, e um sinal de UPDATE custaria estado novo.
    if app.filters.active() || new_last != prev_last {
        app.rows_total = count_rows(store, &app.filters, app.window).await?;
    }
    if new_last != prev_last {
        let (total, chats, senders) = totals(store).await?;
        app.total = total;
        app.chats = chats;
        app.senders = senders;
    }
    // #33: cursor e feed só depois do recompute OK; falha deixa o gate aberto e o próximo tick re-tenta.
    app.last_id = new_last;
    let added = pending.len();
    if added > 0 {
        app.feed.extend(pending);
        if app.feed.len() > FEED_LIMIT {
            let excess = app.feed.len() - FEED_LIMIT;
            app.feed.drain(..excess);
        }
        if app.selected > 0 {
            app.selected += added; // mantém a linha em foco enquanto chegam novas
        }
        app.clamp_selection();
    }
    app.status.clear();
    Ok(())
}

async fn refresh_stats(app: &mut App, store: &MessageStore) -> Result<(), String> {
    app.stats_at = Instant::now();
    app.stats = Some(stats::collect(store, app.window).await?);
    Ok(())
}

async fn load_agents(store: &MessageStore, show_all: bool) -> Result<Vec<AgentRow>, String> {
    let filter = if show_all {
        ""
    } else {
        " WHERE status NOT IN ('joined','already','duplicate')"
    };
    let mut rows = store
        .conn
        .query(
            &format!(
                "SELECT id, link, status, COALESCE(verdict, ''),
                        COALESCE(target_title, ''), COALESCE(source_chat, ''),
                        COALESCE(created_at, ''), score, COALESCE(manual_approved, 0),
                        COALESCE(force_join, 0), COALESCE(checked_at, ''),
                        COALESCE(reason, ''), COALESCE(error, ''), COALESCE(model, ''),
                        COALESCE(tries, 0)
                 FROM group_candidates{filter}
                 ORDER BY id DESC LIMIT {AGENT_LIMIT}"
            ),
            (),
        )
        .await
        .map_err(|e| format!("Falha ao listar convites: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao ler convite: {e}"))?
    {
        let score = match row.get_value(7) {
            Ok(libsql::Value::Real(n)) => Some(n),
            Ok(libsql::Value::Integer(n)) => Some(n as f64),
            _ => None,
        };
        out.push(AgentRow {
            id: row.get::<i64>(0).unwrap_or(0),
            link: row.get::<String>(1).unwrap_or_default(),
            status: row.get::<String>(2).unwrap_or_default(),
            verdict: row.get::<String>(3).unwrap_or_default(),
            target_title: row.get::<String>(4).unwrap_or_default(),
            source_chat: row.get::<String>(5).unwrap_or_default(),
            created_at: row.get::<String>(6).unwrap_or_default(),
            checked_at: row.get::<String>(10).unwrap_or_default(),
            score,
            reason: row.get::<String>(11).unwrap_or_default(),
            error: row.get::<String>(12).unwrap_or_default(),
            model: row.get::<String>(13).unwrap_or_default(),
            tries: row.get::<i64>(14).unwrap_or(0),
            manual_approved: row.get::<i64>(8).unwrap_or(0) != 0,
            force_join: row.get::<i64>(9).unwrap_or(0) != 0,
        });
    }
    Ok(out)
}

async fn agent_summary(store: &MessageStore) -> Result<(String, i64), String> {
    let mut rows = store
        .conn
        .query(
            "SELECT status, COUNT(*) FROM group_candidates GROUP BY status",
            (),
        )
        .await
        .map_err(|e| format!("Falha ao resumir convites: {e}"))?;
    let (mut total, mut pending, mut approved, mut joined, mut duplicate) = (0, 0, 0, 0, 0);
    let (mut skipped, mut failed) = (0, 0);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao resumir convites: {e}"))?
    {
        let status: String = row.get(0).unwrap_or_default();
        let count: i64 = row.get(1).unwrap_or(0);
        total += count;
        match status.as_str() {
            "pending" => pending += count,
            "approved" => approved += count,
            "joined" | "already" => joined += count,
            "duplicate" => duplicate += count,
            "skipped" => skipped += count,
            "failed" | "error" => failed += count,
            _ => {}
        }
    }
    Ok((
        format!(
            "Convites {total} · pend. {pending} · aprov. {approved} · \
             rejeit. {skipped} · falha {failed} · entrou/já {joined} · rep. {duplicate}"
        ),
        total,
    ))
}

async fn reload_agents(app: &mut App, store: &MessageStore) -> Result<(), String> {
    let keep_id = app.selected_agent().map(|row| row.id);
    app.agent_rows = load_agents(store, app.agent_show_all).await?;
    let (summary, all_total) = agent_summary(store).await?;
    app.agent_summary = summary;
    app.agent_all_total = all_total;
    app.agent_selected = keep_id
        .and_then(|id| app.agent_rows.iter().position(|row| row.id == id))
        .unwrap_or(0)
        .min(app.agent_rows.len().saturating_sub(1));
    if app.agent_rows.is_empty() {
        app.agent_offset = 0;
    }
    Ok(())
}

const ROW_SELECT: &str = "SELECT id, timestamp, COALESCE(chat_title, ''),
        COALESCE(sender_first_name, ''), COALESCE(sender_username, ''),
        COALESCE(message_type, ''), COALESCE(message_text, ''), COALESCE(msg_kind, '')
    FROM messages";

fn build_where(filters: &Filters, window: Window) -> (String, Vec<libsql::Value>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if let Some(cutoff) = stats::cutoff_iso(window) {
        clauses.push("timestamp >= ?".to_string());
        values.push(libsql::Value::Text(cutoff));
    }
    if let Some(chat) = &filters.chat {
        clauses.push("chat_id = ?".to_string());
        values.push(libsql::Value::Integer(chat.id));
    }
    if let Some(kind) = &filters.kind {
        clauses.push("message_type = ?".to_string());
        values.push(libsql::Value::Text(kind.clone()));
    }
    if filters.links_only {
        clauses.push("has_links = 1".to_string());
    }
    if filters.hide_ads {
        clauses.push("COALESCE(msg_kind, '') != 'ad'".to_string());
    }
    if let Some(search) = &filters.search {
        // LIKE é case-insensitive para ASCII; ESCAPE evita que % e _ virem curinga.
        clauses.push("message_text LIKE ? ESCAPE '\\'".to_string());
        values.push(libsql::Value::Text(format!("%{}%", escape_like(search))));
    }
    // shortcut: predicados sem índice (LIKE/tipo/links/msg_kind) ainda fazem scan; timestamp/chat_id já cobertos por idx_messages_timestamp e o autoindex do UNIQUE.
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, values)
}

fn escape_like(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

async fn live_rows(
    store: &MessageStore,
    after_id: i64,
    limit: u32,
    cutoff: Option<&str>,
) -> Result<Vec<FeedRow>, String> {
    let (filter, mut values) = match cutoff {
        Some(cutoff) => (
            " AND timestamp >= ?",
            vec![
                libsql::Value::Integer(after_id),
                libsql::Value::Text(cutoff.to_string()),
                libsql::Value::Integer(limit as i64),
            ],
        ),
        None => (
            "",
            vec![
                libsql::Value::Integer(after_id),
                libsql::Value::Integer(limit as i64),
            ],
        ),
    };
    let mut rows = store
        .conn
        .query(
            &format!("{ROW_SELECT} WHERE id > ?{filter} ORDER BY id LIMIT ?"),
            libsql::params_from_iter(values.drain(..)),
        )
        .await
        .map_err(|e| format!("Falha ao ler mensagens: {e}"))?;
    collect_rows(&mut rows).await
}

async fn recent_rows(
    store: &MessageStore,
    limit: u32,
    cutoff: Option<&str>,
) -> Result<Vec<FeedRow>, String> {
    let (filter, values) = match cutoff {
        Some(cutoff) => (
            " WHERE timestamp >= ?",
            vec![
                libsql::Value::Text(cutoff.to_string()),
                libsql::Value::Integer(limit as i64),
            ],
        ),
        None => ("", vec![libsql::Value::Integer(limit as i64)]),
    };
    let mut rows = store
        .conn
        .query(
            &format!(
                "SELECT * FROM ({ROW_SELECT}{filter} ORDER BY id DESC LIMIT ?) ORDER BY id ASC"
            ),
            libsql::params_from_iter(values),
        )
        .await
        .map_err(|e| format!("Falha ao ler mensagens: {e}"))?;
    collect_rows(&mut rows).await
}

async fn query_rows(
    store: &MessageStore,
    filters: &Filters,
    window: Window,
    limit: u32,
) -> Result<Vec<FeedRow>, String> {
    let (where_sql, mut values) = build_where(filters, window);
    values.push(libsql::Value::Integer(limit as i64));
    // Pega as N mais novas que casam com o filtro e devolve em ordem crescente.
    let mut rows = store
        .conn
        .query(
            &format!(
                "SELECT * FROM ({ROW_SELECT}{where_sql} ORDER BY id DESC LIMIT ?) ORDER BY id ASC"
            ),
            libsql::params_from_iter(values),
        )
        .await
        .map_err(|e| format!("Falha ao filtrar mensagens: {e}"))?;
    collect_rows(&mut rows).await
}

async fn count_rows(
    store: &MessageStore,
    filters: &Filters,
    window: Window,
) -> Result<i64, String> {
    let (where_sql, values) = build_where(filters, window);
    let mut rows = store
        .conn
        .query(
            &format!("SELECT COUNT(*) FROM messages{where_sql}"),
            libsql::params_from_iter(values),
        )
        .await
        .map_err(|e| format!("Falha ao contar mensagens filtradas: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao contar mensagens filtradas: {e}"))?
        .ok_or("Contagem sem resultado")?;
    Ok(row.get(0).unwrap_or(0))
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
            msg_kind: text(7),
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

async fn max_id(store: &MessageStore) -> Result<i64, String> {
    let mut rows = store
        .conn
        .query("SELECT MAX(id) FROM messages", ())
        .await
        .map_err(|e| format!("Falha ao ler último id: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao ler último id: {e}"))?
        .ok_or("Leitura sem resultado")?;
    Ok(row.get(0).unwrap_or(0))
}

async fn chat_options(store: &MessageStore) -> Result<Vec<ChatOption>, String> {
    // `messages` cobre chats antigos que o catálogo pode não ter (ou estar vazio).
    let mut rows = store
        .conn
        .query(
            "SELECT chat_id, COALESCE(NULLIF(chat_title, ''), CAST(chat_id AS TEXT))
             FROM messages GROUP BY chat_id ORDER BY 2 COLLATE NOCASE",
            (),
        )
        .await
        .map_err(|e| format!("Falha ao listar chats: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao listar chats: {e}"))?
    {
        out.push(ChatOption {
            id: row.get(0).unwrap_or(0),
            title: row.get(1).unwrap_or_default(),
        });
    }
    Ok(out)
}

fn filtered_chats(app: &App) -> Vec<ChatOption> {
    let term = app.input.trim().to_lowercase();
    app.chats_catalog
        .iter()
        .filter(|chat| {
            term.is_empty()
                || chat.title.to_lowercase().contains(&term)
                || chat.id.to_string().contains(&term)
        })
        .cloned()
        .collect()
}

/// Converte a foto (JPEG) em linhas de arte ASCII.
fn avatar_ascii(bytes: &[u8], width: u32, height: u32) -> Option<Vec<String>> {
    let img = image::load_from_memory(bytes).ok()?.to_luma8();
    Some(ascii_from_luma(&img, width, height))
}

/// A célula do terminal é ~2x mais alta que larga: 2 linhas de pixel por linha de texto.
fn ascii_from_luma(img: &image::GrayImage, width: u32, height: u32) -> Vec<String> {
    const RAMP: &[u8] = b" .:-=+*#%@";
    let img = image::imageops::resize(img, width, height * 2, image::imageops::FilterType::Triangle);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    let top = img.get_pixel(x, y * 2)[0] as u16;
                    let bottom = img.get_pixel(x, y * 2 + 1)[0] as u16;
                    let lum = ((top + bottom) / 2) as usize;
                    RAMP[lum * (RAMP.len() - 1) / 255] as char
                })
                .collect()
        })
        .collect()
}

/// Reutiliza `MESSAGE_COLUMNS` para mostrar todos os campos não nulos.
async fn load_detail(store: &MessageStore, row_id: i64) -> Result<Vec<(String, String)>, String> {
    let columns = MESSAGE_COLUMNS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    // O avatar não está em `messages`: vem dos catálogos users/chats.
    let mut rows = store
        .conn
        .query(
            &format!(
                "SELECT {columns},
                    (SELECT avatar_blob FROM users WHERE users.id = messages.sender_id),
                    (SELECT avatar_blob FROM chats WHERE chats.id = messages.chat_id)
                 FROM messages WHERE id = ?"
            ),
            libsql::params![row_id],
        )
        .await
        .map_err(|e| format!("Falha ao abrir o detalhe: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao abrir o detalhe: {e}"))?
        .ok_or("Mensagem não encontrada")?;

    let mut fields = vec![("id no banco".to_string(), row_id.to_string())];
    for (index, (name, _)) in MESSAGE_COLUMNS.iter().enumerate() {
        let value = row
            .get_value(index as i32)
            .map_err(|e| format!("Falha ao ler o campo {name}: {e}"))?;
        let text = match value {
            libsql::Value::Null => continue,
            libsql::Value::Text(text) if text.is_empty() => continue,
            libsql::Value::Text(text) => text,
            libsql::Value::Integer(number) => number.to_string(),
            libsql::Value::Real(number) => number.to_string(),
            libsql::Value::Blob(blob) => format!("<{} bytes>", blob.len()),
        };
        fields.push(((*name).to_string(), text));
    }
    for (index, label) in [
        (MESSAGE_COLUMNS.len() as i32, "avatar do remetente"),
        (MESSAGE_COLUMNS.len() as i32 + 1, "avatar do chat"),
    ] {
        if let Ok(libsql::Value::Blob(blob)) = row.get_value(index)
            && !blob.is_empty()
            && let Some(rows) = avatar_ascii(&blob, 24, 12)
        {
            fields.push((label.to_string(), rows.join("\n")));
            break;
        }
    }
    Ok(fields)
}

async fn load_agent_detail(store: &MessageStore, id: i64) -> Result<Vec<(String, String)>, String> {
    let mut rows = store
        .conn
        .query(
            "SELECT id, link, kind, target, target_kind, target_id, target_title, target_about,
                    participants, flagged, score, verdict, reason, model, status,
                    manual_approved, error, COALESCE(source_chat, ''),
                    COALESCE(source_text, ''), created_at, checked_at, attempted_at, joined_at,
                    COALESCE(tries, 0), COALESCE(force_join, 0)
             FROM group_candidates WHERE id = ?",
            libsql::params![id],
        )
        .await
        .map_err(|e| format!("Falha ao abrir o convite: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao abrir o convite: {e}"))?
        .ok_or("Convite não encontrado")?;
    let labels = [
        "id",
        "link",
        "tipo do link",
        "alvo",
        "tipo do alvo",
        "id do chat",
        "título",
        "descrição",
        "participantes",
        "flag scam/fake",
        "score",
        "veredito",
        "motivo",
        "modelo",
        "status",
        "aprovado manualmente",
        "erro",
        "chat de origem",
        "texto de origem",
        "criado",
        "analisado",
        "tentado",
        "entrou",
        "tentativas",
        "retry forçado",
    ];
    let mut fields = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        let value = row
            .get_value(index as i32)
            .map_err(|e| format!("Falha ao ler {label}: {e}"))?;
        let text = match value {
            libsql::Value::Null => continue,
            libsql::Value::Text(text) if text.is_empty() => continue,
            libsql::Value::Text(text) => text,
            libsql::Value::Integer(number) => number.to_string(),
            libsql::Value::Real(number) => number.to_string(),
            libsql::Value::Blob(blob) => format!("<{} bytes>", blob.len()),
        };
        let text = match *label {
            "flag scam/fake" | "aprovado manualmente" | "retry forçado" => {
                if text == "1" {
                    "sim".to_string()
                } else {
                    "não".to_string()
                }
            }
            "erro" => explain_error(&text),
            _ => text,
        };
        fields.push(((*label).to_string(), text));
    }
    drop(rows);

    let mut actions = store
        .conn
        .query(
            "SELECT created_at, actor, action, COALESCE(detail, '') FROM group_actions
             WHERE link = (SELECT link FROM group_candidates WHERE id = ?)
             ORDER BY id DESC LIMIT 30",
            libsql::params![id],
        )
        .await
        .map_err(|e| format!("Falha ao ler o histórico: {e}"))?;
    let mut history = Vec::new();
    while let Some(row) = actions
        .next()
        .await
        .map_err(|e| format!("Falha ao ler o histórico: {e}"))?
    {
        let created: String = row.get(0).unwrap_or_default();
        let actor: String = row.get(1).unwrap_or_default();
        let action: String = row.get(2).unwrap_or_default();
        let detail: String = row.get(3).unwrap_or_default();
        history.push(format!("{created} · {actor} · {action} · {detail}"));
    }
    if !history.is_empty() {
        fields.push(("histórico".to_string(), history.join("\n")));
    }
    Ok(fields)
}

/// `2026-11-19T14:30:00Z` → `14:30`.
fn short_time(timestamp: &str) -> String {
    timestamp.get(11..16).unwrap_or(timestamp).to_string()
}

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).split(area);

    let mode = if app.paused {
        "⏸ pausado"
    } else {
        "● ao vivo"
    };
    let hint = match app.view {
        View::Feed => "s estatísticas · a agente",
        View::Stats => "s ao vivo · a agente",
        View::Agent => "a mensagens",
    };
    let filter_line = match app.view {
        View::Agent => {
            let show = if app.agent_show_all {
                "f filtra já entrou/rep."
            } else {
                "f mostra já entrou/rep."
            };
            format!("y aprova · n rejeita · x retry · {show} · Enter detalhe · r recarrega")
        }
        _ if app.filters.active() => app.filters.summary(),
        _ => "/ busca · g chat · m tipo · l links · k anúncios · c limpa · a agente · q sair"
            .to_string(),
    };
    let first_line = if app.view == View::Agent {
        app.agent_summary.clone()
    } else {
        format!(
            "Mensagens {} · Chats {} · Remetentes {} · exibindo {} de {}",
            app.total,
            app.chats,
            app.senders,
            app.feed.len(),
            app.rows_total
        )
    };
    let second_line = if app.view == View::Agent {
        format!(" · {hint} · p pausa · r recarrega")
    } else {
        format!(
            " · janela {} · {hint} · t janela · p pausa · r recarrega",
            app.window.label()
        )
    };
    let third_line = if app.status.is_empty() {
        filter_line
    } else {
        app.status.clone()
    };
    let header = vec![
        Line::from(first_line),
        Line::from(vec![
            Span::styled(
                mode,
                Style::new().fg(if app.paused {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
            Span::raw(second_line),
        ]),
        Line::from(Span::styled(third_line, Style::new().fg(Color::Yellow))),
    ];
    frame.render_widget(
        Paragraph::new(header).block(Block::bordered().title(" Telegram CTI Monitor ")),
        chunks[0],
    );

    match app.view {
        View::Stats => ui_stats(frame, app, chunks[1]),
        View::Agent => ui_agent(frame, app, chunks[1]),
        View::Feed => ui_feed(frame, app, chunks[1]),
    }

    match app.mode {
        Mode::Search => render_search(frame, app, area),
        Mode::ChatPicker => render_chat_picker(frame, app, area),
        Mode::TypePicker => render_type_picker(frame, app, area),
        Mode::Detail => render_detail(frame, app, area),
        Mode::Browse => {}
    }
}

fn ui_feed(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.filters.active() {
        " Filtradas "
    } else {
        " Ao vivo "
    };
    let block = Block::bordered().title(title);

    if app.total == 0 {
        frame.render_widget(
            Paragraph::new("Nenhuma mensagem ainda. Aguardando o monitor gravar no banco…")
                .alignment(Alignment::Center)
                .block(block),
            area,
        );
        return;
    }
    if app.feed.is_empty() {
        let message = if app.filters.active() {
            "Nenhuma mensagem casa com os filtros."
        } else {
            "Nenhuma mensagem nesta janela. Pressione t para ampliar."
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .block(block),
            area,
        );
        return;
    }

    app.list_height = area.height.saturating_sub(2) as usize;
    app.clamp_selection();

    let lines: Vec<Line> = app
        .feed
        .iter()
        .rev()
        .enumerate()
        .skip(app.list_offset)
        .take(app.list_height)
        .map(|(index, row)| {
            let mut line = feed_line(row);
            if index == app.selected {
                line.spans.insert(0, Span::raw("▶ "));
                line = line.style(Style::new().bg(Color::DarkGray));
            } else {
                line.spans.insert(0, Span::raw("  "));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn ui_agent(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(" Agente ");
    if app.agent_rows.is_empty() {
        let message = if app.agent_all_total == 0 {
            "Nenhum convite ainda. O agente precisa de OPENROUTER_API_KEY e de links t.me."
        } else {
            "Nenhum convite visível. Pressione f para mostrar já entrou/repetidos."
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .block(block),
            area,
        );
        return;
    }

    // Com altura suficiente, reserva o rodapé para a decisão do selecionado.
    let (list_area, decision_area) = if area.height >= 14 {
        let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(8)]).split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };

    app.agent_height = list_area.height.saturating_sub(2) as usize;
    clamp_agent_selection(app);

    let lines: Vec<Line> = app
        .agent_rows
        .iter()
        .enumerate()
        .skip(app.agent_offset)
        .take(app.agent_height)
        .map(|(index, row)| {
            let mut line = agent_line(row);
            if index == app.agent_selected {
                line.spans.insert(0, Span::raw("▶ "));
                line = line.style(Style::new().bg(Color::DarkGray));
            } else {
                line.spans.insert(0, Span::raw("  "));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    if let Some(decision_area) = decision_area
        && let Some(row) = app.agent_rows.get(app.agent_selected)
    {
        frame.render_widget(
            Paragraph::new(agent_decision_lines(row))
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Decisão ")),
            decision_area,
        );
    }
}

/// Resumo legível da triagem do convite selecionado.
fn agent_decision_lines(row: &AgentRow) -> Vec<Line<'static>> {
    let score = row
        .score
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "-".to_string());
    let verdict = match row.verdict.as_str() {
        "fraud" => "fraude",
        "benign" => "ok",
        _ => "sem veredito",
    };
    let model = if row.model.is_empty() {
        "modelo não informado"
    } else {
        row.model.as_str()
    };
    let checked = if row.checked_at.is_empty() {
        "não analisado"
    } else {
        "analisado"
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            status_label(&row.status).to_string(),
            Style::new().fg(status_color(&row.status)).bold(),
        ),
        Span::raw(format!(
            " · score {score} ({verdict}) · {model} · {checked} {}",
            short_time(&row.checked_at)
        )),
        if row.manual_approved {
            Span::styled(" ✋ manual", Style::new().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
        if row.force_join {
            Span::styled(" ⚡ retry", Style::new().fg(Color::Red))
        } else {
            Span::raw("")
        },
    ])];
    if !row.reason.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("motivo: ", Style::new().fg(Color::DarkGray)),
            Span::raw(row.reason.clone()),
        ]));
    }
    if !row.error.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("erro: ", Style::new().fg(Color::Red)),
            Span::styled(explain_error(&row.error), Style::new().fg(Color::Red)),
        ]));
    }
    if row.reason.is_empty() && row.error.is_empty() {
        lines.push(Line::from(Span::styled(
            "sem análise ainda: aguardando o agente",
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(format!(
        "tentativas {} · origem {} · criado {}",
        row.tries,
        if row.source_chat.is_empty() {
            "-"
        } else {
            row.source_chat.as_str()
        },
        short_time(&row.created_at)
    )));
    lines.push(Line::from(Span::styled(
        truncate(&row.link, 100),
        Style::new().fg(Color::Cyan),
    )));
    lines
}

/// Traduz os erros mais comuns do Telegram para uma frase curta.
fn explain_error(raw: &str) -> String {
    let known = [
        ("PEER_FLOOD", "conta limitada por spam; entradas pausadas"),
        ("FLOOD_WAIT", "Telegram pediu pausa (flood)"),
        ("INVITE_HASH_EXPIRED", "convite expirado"),
        ("INVITE_HASH_INVALID", "convite inválido"),
        ("USER_ALREADY_PARTICIPANT", "a conta já era membro"),
        ("CHANNEL_PRIVATE", "grupo privado ou apagado"),
        ("CHANNEL_INVALID", "grupo não existe mais"),
        ("CHAT_FORBIDDEN", "entrada proibida nesse grupo"),
        ("USER_CHANNELS_TOO_MUCH", "limite de canais/grupos da conta"),
        ("USERS_TOO_MUCH", "limite de grupos da conta"),
        ("USER_BANNED_IN_CHANNEL", "conta banida nesse grupo"),
        ("USERNAME_NOT_OCCUPIED", "@ não existe"),
        ("USERNAME_INVALID", "@ inválido"),
        ("TIMEOUT", "falha passageira de rede"),
        ("RPC_CALL_FAILED", "falha passageira do Telegram"),
        ("rejeitado manualmente", "rejeitado no TUI"),
    ];
    for (needle, text) in known {
        if raw.contains(needle) {
            return format!("{text} — {}", truncate(raw, 70));
        }
    }
    truncate(raw, 90)
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
    let left = Layout::vertical([Constraint::Length(7), Constraint::Min(3)]).split(cols[0]);
    let mid = Layout::vertical([Constraint::Length(12), Constraint::Min(3)]).split(cols[1]);
    let right = Layout::vertical([Constraint::Length(6), Constraint::Min(3)]).split(cols[2]);

    let o = &data.overview;
    let pct = if o.total > 0 {
        o.links * 100 / o.total
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Total {} · Chats {}", o.total, o.chats)),
            Line::from(format!("Remetentes {}", o.senders)),
            Line::from(format!("Links {} ({}%)", o.links, pct)),
            Line::from(format!("Anúncios {} · Conversas {}", o.ads, o.conversations)),
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

    let hours: Vec<u64> = data
        .hours
        .iter()
        .map(|&count| count.max(0) as u64)
        .collect();
    let peak = data
        .hours
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count);
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
        Span::styled(
            format!("[{:<8}] ", row.kind),
            Style::new().fg(Color::Magenta),
        ),
    ];
    if row.msg_kind == "ad" {
        spans.push(Span::styled(
            "anúncio ",
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    spans.extend(text_spans(&row.text));
    Line::from(spans)
}

fn status_label(status: &str) -> &str {
    match status {
        "pending" => "pendente",
        "approved" => "aprovado",
        "joined" => "entrou",
        "already" => "já",
        "duplicate" => "repetido",
        "skipped" => "rejeitado",
        "failed" => "falhou",
        "error" => "erro",
        _ => status,
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "joined" | "approved" => Color::Green,
        "already" => Color::Blue,
        "pending" => Color::Yellow,
        "failed" | "error" => Color::Red,
        "duplicate" => Color::DarkGray,
        "skipped" => Color::Gray,
        _ => Color::White,
    }
}

fn score_color(score: f64) -> Color {
    if score >= 0.8 {
        Color::Green
    } else if score >= 0.5 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn agent_line(row: &AgentRow) -> Line<'static> {
    let score = row
        .score
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "-".to_string());
    let verdict = match row.verdict.as_str() {
        "fraud" => "fraude",
        "benign" => "ok",
        _ => "-",
    };
    let title = if row.target_title.is_empty() {
        &row.link
    } else {
        &row.target_title
    };
    let mut spans = vec![
        Span::styled(
            format!("{:<10}", status_label(&row.status)),
            Style::new().fg(status_color(&row.status)),
        ),
        Span::styled(
            format!("{score:>5} "),
            Style::new().fg(row.score.map(score_color).unwrap_or(Color::DarkGray)),
        ),
        Span::styled(
            format!("{verdict:<6} "),
            Style::new().fg(match row.verdict.as_str() {
                "fraud" => Color::Red,
                "benign" => Color::Green,
                _ => Color::Gray,
            }),
        ),
        Span::raw(truncate(title, 30)),
        Span::raw(" · "),
        Span::styled(truncate(&row.link, 34), Style::new().fg(Color::Cyan)),
    ];
    if !row.source_chat.is_empty() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            truncate(&row.source_chat, 18),
            Style::new().fg(Color::Magenta),
        ));
    }
    if row.manual_approved {
        spans.push(Span::styled(" ✋", Style::new().fg(Color::Yellow)));
    }
    if row.force_join {
        spans.push(Span::styled(" ⚡", Style::new().fg(Color::Red)));
    }
    if row.tries > 0 {
        spans.push(Span::styled(
            format!(" t{}", row.tries),
            Style::new().fg(Color::Yellow),
        ));
    }
    if !row.created_at.is_empty() {
        spans.push(Span::raw(format!(" {}", short_time(&row.created_at))));
    }
    Line::from(spans)
}

/// Destaca links em amarelo; o resto do texto fica normal.
fn text_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, word) in text.split_whitespace().take(40).enumerate() {
        let link = word.contains("http://") || word.contains("https://") || word.contains("t.me/");
        let style = if link {
            Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::new()
        };
        spans.push(Span::styled(
            if i == 0 {
                word.to_string()
            } else {
                format!(" {word}")
            },
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

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn render_search(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, area.width.min(70), 3);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!("busca: {}█", app.input))
            .block(Block::bordered().title(" Busca (Enter aplica · Esc cancela) ")),
        popup,
    );
}

fn render_chat_picker(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, area.width.min(70), area.height.min(18));
    frame.render_widget(Clear, popup);
    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let rows = inner.height.saturating_sub(1) as usize;
    let chats = filtered_chats(app);
    let offset = if app.picker >= rows && rows > 0 {
        app.picker + 1 - rows
    } else {
        0
    };

    let mut lines = vec![Line::from(format!("busca: {}█", app.input))];
    if chats.is_empty() {
        lines.push(Line::from("(nenhum chat)"));
    } else {
        for (index, chat) in chats.iter().enumerate().skip(offset).take(rows) {
            let text = format!("{}  ({})", truncate(&chat.title, 40), chat.id);
            let line = if index == app.picker {
                Line::from(format!("▶ {text}")).style(Style::new().bg(Color::DarkGray))
            } else {
                Line::from(format!("  {text}"))
            };
            lines.push(line);
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" Escolher chat (Enter seleciona · Esc cancela) ")),
        popup,
    );
}

fn render_type_picker(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, area.width.min(50), 14);
    frame.render_widget(Clear, popup);
    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let rows = inner.height as usize;
    let offset = if app.picker >= rows && rows > 0 {
        app.picker + 1 - rows
    } else {
        0
    };
    let lines: Vec<Line> = TYPE_OPTIONS
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
        .map(|(index, option)| {
            let label = if option.is_empty() { "todos" } else { option };
            if index == app.picker {
                Line::from(format!("▶ {label}")).style(Style::new().bg(Color::DarkGray))
            } else {
                Line::from(format!("  {label}"))
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" Tipo (Enter seleciona · Esc cancela) ")),
        popup,
    );
}

fn detail_lines(detail: &[(String, String)]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (name, value) in detail {
        if name.starts_with("avatar do ") {
            lines.push(Line::from(Span::styled(
                format!("{name} (ASCII)"),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            lines.extend(value.lines().map(|line| Line::from(Span::raw(line.to_string()))));
            continue;
        }
        // Forward/reply em amarelo; links no valor também ganham destaque.
        let color = if name.starts_with("forward_") || name.starts_with("reply_") {
            Color::Yellow
        } else {
            Color::Cyan
        };
        let mut parts = value.lines();
        let first = parts.next().unwrap_or("");
        let mut spans = vec![Span::styled(
            format!("{name}: "),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )];
        spans.extend(text_spans(first));
        lines.push(Line::from(spans));
        for extra in parts {
            let mut spans = vec![Span::raw("    ")];
            spans.extend(text_spans(extra));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let popup = centered(area, area.width * 9 / 10, area.height * 9 / 10);
    frame.render_widget(Clear, popup);
    let Some(detail) = &app.detail else {
        return;
    };
    let lines = detail_lines(detail);
    let max = lines.len().saturating_sub(1) as u16;
    app.detail_scroll = app.detail_scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(Block::bordered().title(" Detalhe (↑↓ rola · Esc/Enter volta) ")),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn avatar_vira_arte_ascii() {
        let mut img = image::GrayImage::new(8, 8);
        for (x, _, px) in img.enumerate_pixels_mut() {
            px[0] = if x < 4 { 0 } else { 255 };
        }
        let rows = ascii_from_luma(&img, 10, 5);
        assert_eq!(rows.len(), 5);
        assert!(rows[0].starts_with(' '), "{:?}", rows[0]);
        assert!(rows[0].ends_with('@'), "{:?}", rows[0]);
        assert!(rows.iter().all(|row| row.chars().count() == 10));
        assert!(avatar_ascii(b"nao e imagem", 10, 5).is_none());
    }

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

    async fn insert(
        store: &MessageStore,
        message_id: i64,
        chat_id: i64,
        sender_id: i64,
        text: &str,
    ) {
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

        assert_eq!(live_rows(&store, 0, 10, None).await.unwrap().len(), 2);
        let novas = live_rows(&store, 1, 10, None).await.unwrap();
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

        let rows = recent_rows(&store, 3, None).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["msg 3", "msg 4", "msg 5"]
        );

        cleanup(&path);
    }

    #[tokio::test]
    async fn filtros_combinam_e_busca_trata_curingas_como_texto() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "cpf 123").await;
        insert(&store, 2, -200, 43, "100% garantido").await;
        insert(&store, 3, -100, 43, "sem numero").await;

        let mut filters = Filters::default();
        filters.search = Some("cpf".to_string());
        assert_eq!(
            query_rows(&store, &filters, Window::All, 10)
                .await
                .unwrap()
                .len(),
            1
        );

        filters.search = Some("%".to_string());
        let rows = query_rows(&store, &filters, Window::All, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "100% garantido");

        filters.search = None;
        filters.chat = Some(ChatOption {
            id: -100,
            title: "Grupo".into(),
        });
        assert_eq!(count_rows(&store, &filters, Window::All).await.unwrap(), 2);

        filters.chat = None;
        filters.kind = Some("text".to_string());
        filters.links_only = false;
        assert_eq!(count_rows(&store, &filters, Window::All).await.unwrap(), 3);

        cleanup(&path);
    }

    #[tokio::test]
    async fn refresh_reconta_só_quando_entra_linha_nova() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "primeira").await;
        insert(&store, 2, -100, 42, "segunda").await;

        let mut app = App::new();
        app.window = Window::All;
        reload(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (2, 2));

        // Sentinelas: sem linha nova o refresh não pode recomputar (cache vale).
        app.total = 999;
        app.rows_total = 999;
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (999, 999));

        insert(&store, 3, -100, 42, "terceira").await;
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (3, 3));

        cleanup(&path);
    }

    #[tokio::test]
    async fn refresh_com_filtro_detecta_insert_global_pelo_max_id() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "cpf 123").await;
        insert(&store, 2, -100, 42, "sem numero").await;

        let mut app = App::new();
        app.window = Window::All;
        app.filters.search = Some("cpf".into());
        reload(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (2, 1));

        // Primeiro tick sincroniza last_id com o MAX(id) global (reload só vê o feed filtrado).
        refresh(&mut app, &store).await.unwrap();

        // Com filtro o count roda a cada tick (#28); totals global continua no gate.
        app.total = 999;
        app.rows_total = 999;
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (999, 1));

        // Insert que não casa com o filtro: o total global tem que subir.
        insert(&store, 3, -100, 42, "outra").await;
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (3, 1));

        // Insert que casa: sobe a contagem filtrada.
        insert(&store, 4, -100, 42, "cpf 456").await;
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (4, 2));

        cleanup(&path);
    }

    #[tokio::test]
    async fn refresh_com_filtro_reconta_apos_update_sem_insert() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "anuncio no meio").await;
        insert(&store, 2, -100, 42, "conversa").await;

        let mut app = App::new();
        app.window = Window::All;
        app.filters.hide_ads = true;
        reload(&mut app, &store).await.unwrap();
        assert_eq!(app.rows_total, 2);

        // Classificação pós-insert (#28): UPDATE msg_kind muda o conjunto
        // filtrado sem mexer em max_id, então o count não pode ficar no gate.
        store
            .conn
            .execute(
                "UPDATE messages SET msg_kind = 'ad' WHERE id = 1",
                (),
            )
            .await
            .unwrap();
        refresh(&mut app, &store).await.unwrap();
        assert_eq!(app.rows_total, 1);
        assert_eq!(app.total, 2); // totals global é imune a UPDATE

        cleanup(&path);
    }

    #[tokio::test]
    async fn refresh_falha_no_recompute_re_tenta_no_proximo_tick() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "primeira").await;

        let mut app = App::new();
        app.window = Window::All;
        reload(&mut app, &store).await.unwrap();
        assert_eq!((app.total, app.rows_total), (1, 1));

        insert(&store, 2, -100, 42, "segunda").await;
        // Quebra só o totals (coluna que só ele referencia); count/live seguem OK.
        store
            .conn
            .execute(
                "ALTER TABLE messages RENAME COLUMN sender_id TO sender_id_off",
                (),
            )
            .await
            .unwrap();

        app.status = "erro: injetado".into();
        assert!(refresh(&mut app, &store).await.is_err());
        // Cursor e feed intocados: sem isso o próximo tick fecha o gate e o cache fica defasado.
        assert_eq!(app.last_id, 1);
        assert_eq!(app.feed.len(), 1);
        assert_eq!(app.status, "erro: injetado"); // erro só some quando o retry dá certo

        store
            .conn
            .execute(
                "ALTER TABLE messages RENAME COLUMN sender_id_off TO sender_id",
                (),
            )
            .await
            .unwrap();
        refresh(&mut app, &store).await.unwrap();
        assert_eq!((app.last_id, app.total, app.rows_total), (2, 2, 2));
        assert_eq!(app.feed.len(), 2); // sem duplicar a linha que falhou antes
        assert!(app.status.is_empty());

        cleanup(&path);
    }

    #[tokio::test]
    async fn detalhe_mostra_campos_preenchidos() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        insert(&store, 1, -100, 42, "texto do detalhe").await;
        let row_id: i64 = store
            .conn
            .query("SELECT id FROM messages", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();

        let fields = load_detail(&store, row_id).await.unwrap();
        assert!(fields.iter().any(|(name, _)| name == "message_text"));
        assert!(fields.iter().any(|(_, value)| value == "texto do detalhe"));
        assert!(!fields.iter().any(|(name, _)| name == "media_name"));

        cleanup(&path);
    }

    #[test]
    fn escape_like_nao_deixa_curinga() {
        assert_eq!(escape_like("100%_a\\b"), "100\\%\\_a\\\\b");
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
                ads: 4,
                conversations: 6,
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
                stats::TypeCount {
                    kind: "text".into(),
                    count: 8,
                },
                stats::TypeCount {
                    kind: "link".into(),
                    count: 2,
                },
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
        app.rows_total = 1;
        app.feed.push(FeedRow {
            id: 1,
            time: "14:30".into(),
            chat: "Chat".into(),
            sender: "ana".into(),
            kind: "text".into(),
            msg_kind: "ad".into(),
            text: "promoção http://exemplo.com".into(),
        });
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("Ao vivo"));
        assert!(screen.contains("anúncio"));

        app.view = View::Stats;
        app.stats = Some(sample_stats());
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("Visão geral"));
        assert!(screen.contains("Top chats"));
        assert!(screen.contains("Top remetentes"));
        assert!(screen.contains("Por hora UTC"));
        assert!(screen.contains("Por tipo"));
        assert!(screen.contains("Domínios"));
    }

    #[test]
    fn renderiza_filtros_e_detalhe_em_80x24() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut app = App::new();
        app.total = 1;
        app.rows_total = 1;
        app.feed.push(FeedRow {
            id: 1,
            time: "14:30".into(),
            chat: "Chat".into(),
            sender: "ana".into(),
            kind: "text".into(),
            msg_kind: String::new(),
            text: "texto".into(),
        });
        app.filters.search = Some("texto".into());
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        assert!(screen_text(&terminal).contains("Filtradas"));

        app.mode = Mode::Detail;
        app.detail = Some(vec![("message_text".into(), "linha 1\nlinha 2".into())]);
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        assert!(screen_text(&terminal).contains("Detalhe"));

        app.mode = Mode::Search;
        app.input = "abc".into();
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        assert!(screen_text(&terminal).contains("busca: abc"));
    }

    #[test]
    fn explica_erros_conhecidos() {
        assert!(explain_error("RPC error 420: FLOOD_WAIT (300)").contains("pausa"));
        assert!(explain_error("PEER_FLOOD").contains("spam"));
        assert!(explain_error("INVITE_HASH_EXPIRED").contains("expirado"));
        assert_eq!(explain_error("algo estranho"), "algo estranho");
    }

    #[tokio::test]
    async fn agente_filtra_ja_entrou_repetido_e_aprova_manual() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        for (link, status) in [
            ("https://t.me/+entrou", "joined"),
            ("https://t.me/+pendente", "pending"),
            ("https://t.me/+repetido", "duplicate"),
        ] {
            store
                .conn
                .execute(
                    "INSERT INTO group_candidates (link, kind, target, status)
                     VALUES (?, 'invite', ?, ?)",
                    libsql::params![link, link, status],
                )
                .await
                .unwrap();
        }

        let mut app = App::new();
        reload_agents(&mut app, &store).await.unwrap();
        assert_eq!(app.agent_rows.len(), 1, "filtra joined e duplicate");
        assert_eq!(app.agent_rows[0].link, "https://t.me/+pendente");

        // Retry num link ainda não classificado não pode furar a triagem.
        retry_agent(&mut app, &store).await.unwrap();
        let mut rows = store
            .conn
            .query(
                "SELECT status, force_join FROM group_candidates
                 WHERE link = 'https://t.me/+pendente'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "pending");
        assert_eq!(row.get::<i64>(1).unwrap(), 0);

        decide_agent(&mut app, &store, true).await.unwrap();
        let mut rows = store
            .conn
            .query(
                "SELECT status, manual_approved FROM group_candidates
                 WHERE link = 'https://t.me/+pendente'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "approved");
        assert_eq!(row.get::<i64>(1).unwrap(), 1);

        retry_agent(&mut app, &store).await.unwrap();
        let mut rows = store
            .conn
            .query(
                "SELECT status, manual_approved, force_join FROM group_candidates
                 WHERE link = 'https://t.me/+pendente'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "approved");
        assert_eq!(row.get::<i64>(1).unwrap(), 1);
        assert_eq!(row.get::<i64>(2).unwrap(), 1, "retry força a entrada");

        app.agent_show_all = true;
        reload_agents(&mut app, &store).await.unwrap();
        assert_eq!(app.agent_rows.len(), 3);

        app.view = View::Agent;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| ui(frame, &mut app)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("Agente"));
        assert!(screen.contains("aprovado"));
        assert!(screen.contains("repetido"));
        assert!(screen.contains("Decisão"));
        cleanup(&path);
    }
}
