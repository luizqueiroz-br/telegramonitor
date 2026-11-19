//! Estatísticas agregadas para o dashboard, calculadas no SQLite local.
//!
//! As funções recebem a janela de tempo e devolvem structs simples, sem
//! carregar a tabela inteira em memória (exceto os textos com link, que são
//! parseados em Rust para extrair domínios).

use std::collections::HashMap;

use crate::MessageStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Hour1,
    Hour24,
    Day7,
    All,
}

impl Window {
    pub fn hours(self) -> Option<i64> {
        match self {
            Self::Hour1 => Some(1),
            Self::Hour24 => Some(24),
            Self::Day7 => Some(24 * 7),
            Self::All => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hour1 => "1 h",
            Self::Hour24 => "24 h",
            Self::Day7 => "7 d",
            Self::All => "tudo",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Hour1 => Self::Hour24,
            Self::Hour24 => Self::Day7,
            Self::Day7 => Self::All,
            Self::All => Self::Hour1,
        }
    }
}

pub struct Overview {
    pub total: i64,
    pub chats: i64,
    pub senders: i64,
    pub links: i64,
    pub last_hour: i64,
    pub last_day: i64,
}

pub struct Row {
    pub label: String,
    pub count: i64,
    pub chats: i64, // usado só nos remetentes
}

pub struct TypeCount {
    pub kind: String,
    pub count: i64,
}

pub struct Stats {
    pub overview: Overview,
    pub top_chats: Vec<Row>,
    pub top_senders: Vec<Row>,
    pub types: Vec<TypeCount>,
    pub hours: Vec<i64>, // 24 posições, UTC
    pub domains: Vec<Row>,
}

/// ISO 8601 no mesmo formato gravado em `messages.timestamp` (UTC).
fn iso_cutoff(hours: i64) -> String {
    (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(hours)).to_string()
}

/// Corte ISO da janela (mesmo formato de `messages.timestamp`); `None` = tudo.
pub fn cutoff_iso(window: Window) -> Option<String> {
    window.hours().map(iso_cutoff)
}

fn window_filter(window: Window) -> (String, Vec<libsql::Value>) {
    match window.hours() {
        Some(hours) => (
            " WHERE timestamp >= ?".to_string(),
            vec![libsql::Value::Text(iso_cutoff(hours))],
        ),
        None => (String::new(), Vec::new()),
    }
}

pub async fn collect(store: &MessageStore, window: Window) -> Result<Stats, String> {
    let overview = overview(store).await?;
    let (filter, values) = window_filter(window);

    let top_chats = store
        .conn
        .query(
            &format!(
                "SELECT COALESCE(NULLIF(chat_title, ''), CAST(chat_id AS TEXT)), COUNT(*)
                 FROM messages{filter}
                 GROUP BY chat_id ORDER BY COUNT(*) DESC LIMIT 10"
            ),
            libsql::params_from_iter(values.clone()),
        )
        .await
        .map_err(|e| format!("Falha nas estatísticas de chats: {e}"))?;
    let top_chats = collect_rows(top_chats, false).await?;

    let top_senders = store
        .conn
        .query(
            &format!(
                "SELECT COALESCE(
                    NULLIF(sender_username, ''),
                    NULLIF(TRIM(COALESCE(sender_first_name, '') || ' ' || COALESCE(sender_last_name, '')), ''),
                    CAST(sender_id AS TEXT)
                 ), COUNT(*), COUNT(DISTINCT chat_id)
                 FROM messages{filter}
                 GROUP BY sender_id ORDER BY COUNT(*) DESC LIMIT 10"
            ),
            libsql::params_from_iter(values.clone()),
        )
        .await
        .map_err(|e| format!("Falha nas estatísticas de remetentes: {e}"))?;
    let top_senders = collect_rows(top_senders, true).await?;

    let mut type_rows = store
        .conn
        .query(
            &format!(
                "SELECT COALESCE(NULLIF(message_type, ''), 'other'), COUNT(*)
                 FROM messages{filter}
                 GROUP BY 1 ORDER BY COUNT(*) DESC"
            ),
            libsql::params_from_iter(values),
        )
        .await
        .map_err(|e| format!("Falha nas estatísticas de tipos: {e}"))?;
    let mut types = Vec::new();
    while let Some(row) = type_rows
        .next()
        .await
        .map_err(|e| format!("Falha nas estatísticas de tipos: {e}"))?
    {
        types.push(TypeCount {
            kind: row.get(0).unwrap_or_else(|_| "other".to_string()),
            count: row.get(1).unwrap_or(0),
        });
    }

    Ok(Stats {
        overview,
        top_chats,
        top_senders,
        types,
        hours: hours_24h(store).await?,
        domains: domains(store, window).await?,
    })
}

async fn overview(store: &MessageStore) -> Result<Overview, String> {
    let mut rows = store
        .conn
        .query(
            "SELECT COUNT(*), COUNT(DISTINCT chat_id), COUNT(DISTINCT sender_id),
                    COALESCE(SUM(has_links), 0),
                    COALESCE(SUM(CASE WHEN timestamp >= ? THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN timestamp >= ? THEN 1 ELSE 0 END), 0)
             FROM messages",
            libsql::params![iso_cutoff(1), iso_cutoff(24)],
        )
        .await
        .map_err(|e| format!("Falha na visão geral: {e}"))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Falha na visão geral: {e}"))?
        .ok_or("Visão geral sem resultado")?;
    Ok(Overview {
        total: row.get(0).unwrap_or(0),
        chats: row.get(1).unwrap_or(0),
        senders: row.get(2).unwrap_or(0),
        links: row.get(3).unwrap_or(0),
        last_hour: row.get(4).unwrap_or(0),
        last_day: row.get(5).unwrap_or(0),
    })
}

async fn collect_rows(mut rows: libsql::Rows, with_chats: bool) -> Result<Vec<Row>, String> {
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha ao ler estatísticas: {e}"))?
    {
        out.push(Row {
            label: row.get(0).unwrap_or_default(),
            count: row.get(1).unwrap_or(0),
            chats: if with_chats { row.get(2).unwrap_or(0) } else { 0 },
        });
    }
    Ok(out)
}

/// Contagem por hora UTC nas últimas 24 h, sempre com 24 posições.
async fn hours_24h(store: &MessageStore) -> Result<Vec<i64>, String> {
    let mut rows = store
        .conn
        .query(
            "SELECT CAST(substr(timestamp, 12, 2) AS INTEGER), COUNT(*)
             FROM messages
             WHERE timestamp >= ? AND length(timestamp) >= 13
             GROUP BY 1 ORDER BY 1",
            libsql::params![iso_cutoff(24)],
        )
        .await
        .map_err(|e| format!("Falha no histórico por hora: {e}"))?;
    let mut hours = vec![0i64; 24];
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha no histórico por hora: {e}"))?
    {
        let hour = row.get::<i64>(0).unwrap_or(0);
        if (0..24).contains(&hour) {
            hours[hour as usize] = row.get(1).unwrap_or(0);
        }
    }
    Ok(hours)
}

/// Top domínios da janela. Só os textos com link saem do SQL.
async fn domains(store: &MessageStore, window: Window) -> Result<Vec<Row>, String> {
    let (filter, values) = window_filter(window);
    let filter = if filter.is_empty() {
        " WHERE has_links = 1".to_string()
    } else {
        format!("{filter} AND has_links = 1")
    };
    let mut rows = store
        .conn
        .query(
            &format!("SELECT message_text FROM messages{filter}"),
            libsql::params_from_iter(values),
        )
        .await
        .map_err(|e| format!("Falha nos domínios: {e}"))?;

    let mut counts: HashMap<String, i64> = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Falha nos domínios: {e}"))?
    {
        let text: String = row.get(0).unwrap_or_default();
        for token in text.split_whitespace() {
            if let Some(host) = extract_host(token) {
                *counts.entry(host).or_default() += 1;
            }
        }
    }

    let mut list: Vec<Row> = counts
        .into_iter()
        .map(|(label, count)| Row {
            label,
            count,
            chats: 0,
        })
        .collect();
    list.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)));
    list.truncate(10);
    Ok(list)
}

/// `https://www.Exemplo.com/x?y` → `exemplo.com`.
fn extract_host(token: &str) -> Option<String> {
    let start = token
        .find("http://")
        .or_else(|| token.find("https://"))
        .or_else(|| token.find("t.me/"))?;
    let rest = &token[start..];
    let rest = rest
        .strip_prefix("http://")
        .or_else(|| rest.strip_prefix("https://"))
        .unwrap_or(rest);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .trim_end_matches(['.', ',', ')', ']'])
        .to_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn temp_db() -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir()
            .join(format!(
                "cti-stats-{}-{}.db",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal"));
        let _ = std::fs::remove_file(format!("{path}-shm"));
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert(
        store: &MessageStore,
        message_id: i64,
        chat_id: i64,
        chat_title: &str,
        sender_id: i64,
        username: &str,
        kind: &str,
        text: &str,
        links: bool,
        timestamp: &str,
    ) {
        store
            .conn
            .execute(
                "INSERT INTO messages (message_id, chat_id, chat_title, timestamp,
                    message_text, message_type, has_links, sender_id,
                    sender_first_name, sender_username)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'Fulano', ?)",
                libsql::params![
                    message_id,
                    chat_id,
                    chat_title,
                    timestamp,
                    text,
                    kind,
                    links as i64,
                    sender_id,
                    username
                ],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn agrega_janelas_e_dominios() {
        let path = temp_db();
        let store = MessageStore::open(&path).await.unwrap();
        let now = jiff::Timestamp::now().to_string();
        let old = "2020-01-01T10:00:00Z";

        insert(&store, 1, -100, "Chat A", 7, "ana", "text", "oi http://www.Exemplo.com/x", true, &now).await;
        insert(&store, 2, -100, "Chat A", 8, "bia", "text", "sem link", false, &now).await;
        insert(&store, 3, -200, "Chat B", 7, "ana", "link", "veja https://outro.org/y, e t.me/fulano", true, &now).await;
        insert(&store, 4, -200, "Chat B", 7, "ana", "photo", "antiga", false, old).await;

        let stats = collect(&store, Window::Hour24).await.unwrap();
        assert_eq!(stats.overview.total, 4);
        assert_eq!(stats.overview.last_hour, 3);
        assert_eq!(stats.overview.last_day, 3);
        assert_eq!(stats.overview.links, 2);

        assert_eq!(stats.top_chats[0].label, "Chat A");
        assert_eq!(stats.top_chats[0].count, 2);
        assert_eq!(stats.top_senders[0].label, "ana");
        assert_eq!(stats.top_senders[0].count, 2);
        assert_eq!(stats.top_senders[0].chats, 2);

        // Na janela de 24 h o domínio antigo não aparece; os dois atuais sim.
        let labels: Vec<&str> = stats.domains.iter().map(|d| d.label.as_str()).collect();
        assert!(labels.contains(&"exemplo.com"));
        assert!(labels.contains(&"outro.org"));
        assert!(labels.contains(&"t.me"));

        let all = collect(&store, Window::All).await.unwrap();
        assert_eq!(all.top_chats[0].count, 2); // 2 e 2: ordenado por nome em empate
        assert_eq!(all.types.iter().map(|t| t.count).sum::<i64>(), 4);
        assert_eq!(all.hours.len(), 24);

        cleanup(&path);
    }

    #[test]
    fn extrai_host_de_urls() {
        assert_eq!(extract_host("http://www.Exemplo.com/x?y=1").as_deref(), Some("exemplo.com"));
        assert_eq!(extract_host("(https://outro.org/y),").as_deref(), Some("outro.org"));
        assert_eq!(extract_host("t.me/canal").as_deref(), Some("t.me"));
        assert_eq!(extract_host("sem-url"), None);
    }

    #[test]
    fn janela_cicla() {
        assert_eq!(Window::Hour1.next(), Window::Hour24);
        assert_eq!(Window::All.next(), Window::Hour1);
    }
}
