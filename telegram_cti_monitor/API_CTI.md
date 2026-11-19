# API que recebe os dados do monitor

O `telegram_cti_monitor` envia **uma requisição HTTP por mensagem** nova do
Telegram para a URL configurada em `CTI_API_URL`. Este documento descreve o
contrato que essa API precisa cumprir. A API pode ser escrita em qualquer
linguagem/stack; o exemplo abaixo usa FastAPI só como referência.

## Fluxo

```
Telegram (MTProto) --update--> monitor --POST JSON--> CTI_API_URL
                                   |
                                   +--> messages.db (SQLite local)
```

## Requisição

| Item | Valor |
|---|---|
| Método | `POST` |
| URL | exatamente o valor de `CTI_API_URL` (o monitor não adiciona path) |
| Cabeçalho | `Content-Type: application/json` |
| Corpo | um objeto JSON (a mensagem), sem envelope e sem lote |
| Autenticação | **nenhuma** enviada hoje (ver [Autenticação](#autenticação)) |
| Timeout | 30 s (`curl -m 30`) |

O monitor processa as mensagens **uma de cada vez** (aguarda a resposta antes
da próxima). Responda rápido; trabalho pesado deve ser assíncrono.

## Campos do JSON

Todas as chaves sempre aparecem; valores desconhecidos vêm como `null`.

### Mensagem e chat

| Campo | Tipo | Descrição |
|---|---|---|
| `message_id` | inteiro | id da mensagem dentro do chat |
| `chat_id` | inteiro | id no formato Bot API: usuário positivo; grupo `-123456`; supergrupo/canal `-100...` |
| `chat_title` | string | nome do remetente (chat privado) ou título do grupo/canal |
| `chat_username` | string ou `null` | `@` sem arroba, quando público |
| `chat_kind` | string | `user`, `group`, `channel`, `community` ou `unknown` |
| `timestamp` | string | data da mensagem, ISO 8601/RFC 3339 em UTC (ex.: `2026-11-19T14:30:00Z`) |
| `message_text` | string | texto ou legenda da mídia; `""` em mensagens de serviço |
| `message_type` | string | `text`, `photo`, `document`, `sticker`, `link`, `poll`, `location`, `contact` ou `other` |
| `action_type` | string | `post`, `reply` (resposta a outra mensagem) ou `forward` |
| `has_links` | booleano | `true` se o texto contém `http://`, `https://` ou `t.me/` |
| `has_hashes` | booleano | reservado; hoje sempre `false` |

### Remetente

| Campo | Tipo | Descrição |
|---|---|---|
| `sender_id` | inteiro | id no formato Bot API; `0` quando desconhecido (post de canal, admin anônimo) |
| `sender_username` | string ou `null` | `@` sem arroba |
| `sender_first_name` | string ou `null` | primeiro nome / nome do canal |
| `sender_last_name` | string ou `null` | sobrenome |
| `sender_phone` | string ou `null` | telefone, quando o Telegram expõe (contato salvo) |
| `sender_is_bot` | booleano | conta é bot |
| `sender_is_verified` | booleano | conta verificada |
| `sender_is_scam` | booleano | marcada como golpe pelo Telegram |
| `sender_is_deleted` | booleano | conta excluída |

### Mídia

| Campo | Tipo | Descrição |
|---|---|---|
| `media_name` | string ou `null` | nome do arquivo (documentos/áudio/vídeo) |
| `media_mime` | string ou `null` | MIME type |
| `media_size` | inteiro ou `null` | tamanho em bytes |
| `media_duration` | número ou `null` | duração em segundos (áudio/vídeo) |

O **conteúdo binário** da mídia não é baixado nem enviado; só os metadados.
Fotos de perfil de remetentes e chats são baixadas uma vez para `avatars/` no
lado do monitor (ver "Catálogo" abaixo), mas não vão no JSON.

### Conteúdo estruturado

| Campo | Tipo | Descrição |
|---|---|---|
| `geo_lat` / `geo_lon` | número ou `null` | coordenadas de localização/venue |
| `venue_title` | string ou `null` | nome do local |
| `venue_address` | string ou `null` | endereço do local |
| `contact_name` | string ou `null` | nome do contato compartilhado |
| `contact_phone` | string ou `null` | telefone do contato compartilhado |
| `poll_question` | string ou `null` | pergunta da enquete |
| `poll_answers` | string ou `null` | opções separadas por ` \| ` |

### Encadeamento e métricas

| Campo | Tipo | Descrição |
|---|---|---|
| `forward_from_id` | inteiro ou `null` | chat/autor original do forward (formato Bot API) |
| `forward_from_name` | string ou `null` | nome exibido no forward (canais ocultos) |
| `forward_date` | string ou `null` | data original, ISO 8601 |
| `reply_to_message_id` | inteiro ou `null` | id da mensagem respondida |
| `reply_quote` | string ou `null` | trecho citado na resposta |
| `view_count` | inteiro ou `null` | visualizações |
| `forward_count` | inteiro ou `null` | forwards |
| `reply_count` | inteiro ou `null` | respostas |
| `reaction_count` | inteiro ou `null` | total de reações |
| `grouped_id` | inteiro ou `null` | id do álbum, quando a mensagem faz parte de um |
| `post_author` | string ou `null` | assinatura do autor (canais) |
| `edit_date` | string ou `null` | data da última edição, ISO 8601 |

Observações:

- Mensagens **enviadas pela própria conta** também são enviadas à API
  (o monitor não filtra remetente).
- O JSON é compacto (sem espaços/quebras) e os campos aparecem na ordem acima.
- A tabela `messages` do `messages.db` local espelha exatamente esses campos.
- O monitor mantém localmente `users`/`chats` (catálogo) e `avatars/`; nada
  disso é enviado no POST. Se a CTI precisar de fotos, crie um endpoint
  separado e o monitor passa a enviá-las sob demanda.

## Exemplo de requisição

```json
{
  "message_id": 12345,
  "chat_id": -1001234567890,
  "chat_title": "Grupo de Ofertas",
  "chat_username": "grupodeofertas",
  "chat_kind": "channel",
  "timestamp": "2026-11-19T14:30:00Z",
  "message_text": "Oferta relâmpago: http://exemplo.com/promo",
  "message_type": "text",
  "action_type": "post",
  "has_links": true,
  "has_hashes": false,
  "sender_id": 987654321,
  "sender_username": "fulano",
  "sender_first_name": "Fulano",
  "sender_last_name": "De Tal",
  "sender_phone": null,
  "sender_is_bot": false,
  "sender_is_verified": false,
  "sender_is_scam": true,
  "sender_is_deleted": false,
  "media_name": null,
  "media_mime": null,
  "media_size": null,
  "media_duration": null,
  "geo_lat": null,
  "geo_lon": null,
  "venue_title": null,
  "venue_address": null,
  "contact_name": null,
  "contact_phone": null,
  "poll_question": null,
  "poll_answers": null,
  "forward_from_id": null,
  "forward_from_name": null,
  "forward_date": null,
  "reply_to_message_id": null,
  "reply_quote": null,
  "view_count": 150,
  "forward_count": 12,
  "reply_count": 3,
  "reaction_count": 7,
  "grouped_id": null,
  "post_author": null,
  "edit_date": null
}
```

Teste manual:

```bash
curl -X POST http://localhost:8080/api/telegram/message \
  -H 'Content-Type: application/json' \
  -d '{"message_id":1,"sender_id":2,"sender_username":null,"chat_id":-1001234567890,"chat_title":"Teste","timestamp":"2026-11-19T14:30:00Z","message_text":"oi","message_type":"text","has_links":false,"has_hashes":false,"action_type":"post"}'
```

## Como o monitor trata a resposta

O envio usa `curl -s` (sem `-f`), então:

- **2xx**: sucesso, se o corpo não contiver a palavra `error`.
- O código HTTP em si **não é verificado**: um `500` com corpo sem `error`
  conta como sucesso; um `200` com `{"error":"..."}` conta como falha.
- Em falha, o monitor apenas imprime `[ERRO] ...` no console e **não tenta de
  novo**. A mensagem continua no `messages.db` local.

Recomendação prática:

| Situação | Responda |
|---|---|
| Gravou/processou | `200` (ou `201`) com `{"ok":true}` |
| JSON inválido / campo faltando | `400` com `{"detail":"..."}` (evite a palavra `error`) |
| Falha interna | `500` com `{"error":"..."}` **de propósito**, para o operador ver no console |

Evite respostas como `{"errors":[]}` no sucesso: a palavra `error` (comparação
literal, minúscula) faria o monitor registrar falha.

## Endpoints da plataforma CTI

O monitor só chama **um** endpoint (o de ingestão). Os demais são para a
plataforma CTI consultar/gerenciar o que foi recebido. A implementação deles
pertence ao repositório da CTI; o contrato fica fixado aqui para o monitor e a
plataforma evoluírem juntos.

| Método | Caminho | Uso |
|---|---|---|
| `POST` | `CTI_API_URL` (sugestão: `/api/v1/messages`) | ingestão, chamada pelo monitor |
| `GET` | `/api/v1/messages/filter` | consulta com filtros |
| `POST` | `/api/v1/actors` | cadastrar/atualizar ator suspeito |
| `GET` | `/api/v1/actors/:username` | perfil de um ator |

### GET /api/v1/messages/filter

Query string (todos opcionais e combináveis):

| Parâmetro | Tipo | Descrição |
|---|---|---|
| `chat_id` | inteiro | filtra pelo chat |
| `sender_id` | inteiro | filtra pelo remetente |
| `message_type` | string | `text`, `photo`, `document`, ... |
| `has_links` | `true`/`false` | filtra por presença de links |
| `from` / `to` | ISO 8601 | intervalo de `timestamp` (inclusivo) |
| `q` | string | busca em `message_text` (substring, sem FTS) |
| `limit` / `offset` | inteiro | paginação (padrão `limit=100`, máx. 1000) |

Resposta `200`:

```json
{
  "success": true,
  "count": 1,
  "limit": 100,
  "offset": 0,
  "items": [ { "chat_id": -1001234567890, "message_id": 12345, "timestamp": "2026-11-19T14:30:00Z", "message_text": "...", "has_links": true } ]
}
```

Os `items` são os mesmos objetos do POST (todos os campos), ordenados por
`timestamp` decrescente. Use `(chat_id, message_id)` como chave estável.

### POST /api/v1/actors

Cadastra ou atualiza (upsert por `username`) um ator monitorado.

```json
{
  "username": "fulano",
  "first_seen": "2026-11-01T10:00:00Z",
  "last_seen": "2026-11-19T14:30:00Z",
  "associated_groups": [-1001234567890, -1009876543210],
  "tags": ["scam", "link-suspeito"]
}
```

Resposta `200` (ou `201` na criação):

```json
{ "success": true, "stored_id": 42 }
```

### GET /api/v1/actors/:username

Resposta `200`:

```json
{
  "success": true,
  "actor": {
    "username": "fulano",
    "stored_id": 42,
    "first_seen": "2026-11-01T10:00:00Z",
    "last_seen": "2026-11-19T14:30:00Z",
    "associated_groups": [-1001234567890],
    "tags": ["scam"],
    "message_count": 137
  }
}
```

`404` com `{"error":"ator não encontrado"}` quando o `username` não existe.

**Envelope:** toda resposta traz `success` (booleano) e, quando aplicável,
`stored_id` (registro criado) ou `count`/`items` (consultas). Erros usam
`{"error":"..."}` — a palavra `error` minúscula também sinaliza falha no
monitor, conforme a seção anterior.

## Idempotência (importante)

O monitor usa `catch_up`, então após reiniciar ele pode reenviar mensagens que
chegaram enquanto estava offline. **A API deve deduplicar** pela chave
`(chat_id, message_id)` — a mesma chave usada pelo SQLite local. Assim, repetir
o POST é inofensivo.

## Autenticação

O monitor não envia token. Para produção, escolha uma das opções:

1. **Rede**: exponha a API só em `localhost`, VPN ou firewall liberando apenas
   o IP do monitor (mais simples, nenhuma mudança no código).
2. **Token**: acrescente um header no envio. Em
   `src/main.rs`, na função `send_to_cti_api`, adicione ao `args`:

   ```rust
   "-H", "Authorization: Bearer SEU_TOKEN",
   ```

   e configure o segredo no receptor.

Nunca exponha a API na internet aberta sem uma dessas proteções — os dados
podem conter telefones, nomes e links de vítimas.

## Implementação de referência (FastAPI)

```python
import json
import sqlite3

from fastapi import FastAPI, HTTPException, Request

app = FastAPI()
db = sqlite3.connect("cti.db", check_same_thread=False)
db.execute("""
CREATE TABLE IF NOT EXISTS messages (
  chat_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  received_at TEXT DEFAULT CURRENT_TIMESTAMP,
  raw_json TEXT NOT NULL,
  PRIMARY KEY (chat_id, message_id)
)
""")

@app.post("/api/telegram/message")
async def receive(request: Request):
    payload = await request.json()
    for key in ("chat_id", "message_id", "timestamp", "message_text"):
        if key not in payload:
            raise HTTPException(status_code=400, detail=f"campo ausente: {key}")
    db.execute(
        "INSERT OR IGNORE INTO messages (chat_id, message_id, raw_json)"
        " VALUES (?, ?, ?)",
        (payload["chat_id"], payload["message_id"],
         json.dumps(payload, ensure_ascii=False)),
    )
    db.commit()
    return {"ok": True}
```

Guardar o JSON cru não perde nenhum campo novo. Depois você pode criar
colunas/índices (por `sender_username`, `chat_id`, `timestamp`) e preenchê-las
a partir de `raw_json`, ou trocar por um `INSERT` com as colunas desejadas.

Rodar e configurar no monitor (`.env`):

```bash
pip install fastapi uvicorn
uvicorn main:app --host 127.0.0.1 --port 8080
```

```ini
CTI_API_URL=http://127.0.0.1:8080/api/telegram/message
```

---

## Anexo: o "app do Telegram" (api_id/api_hash)

Se a dúvida for a credencial do lado do Telegram: o monitor precisa de um app
criado em <https://my.telegram.org> → **API development tools**. De lá saem
`TELEGRAM_API_ID` e `TELEGRAM_API_HASH`. Não há endpoint a implementar: o
monitor fala MTProto direto com os servidores do Telegram, e esses valores são
pedidos automaticamente no primeiro `make run` (veja o `README.md`).
