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

| Campo | Tipo | Obrigatório | Descrição |
|---|---|---|---|
| `message_id` | inteiro | sim | id da mensagem dentro do chat |
| `sender_id` | inteiro | sim | id do remetente no formato Bot API; `0` quando desconhecido (post de canal, admin anônimo) |
| `sender_username` | string ou `null` | sim | `@` sem o arroba; `null` para muitos grupos/canais |
| `chat_id` | inteiro | sim | id do chat no formato Bot API: usuário positivo; grupo `-123456`; supergrupo/canal `-100...` |
| `chat_title` | string | sim | nome do remetente (chat privado) ou título do grupo/canal |
| `timestamp` | string | sim | data da mensagem, ISO 8601/RFC 3339 em UTC (ex.: `2026-11-19T14:30:00Z`) |
| `message_text` | string | sim | texto ou legenda da mídia; `""` em mensagens de serviço |
| `message_type` | string | sim | `text`, `photo`, `document`, `sticker`, `link`, `poll`, `location`, `contact` ou `other` |
| `has_links` | booleano | sim | `true` se o texto contém `http://`, `https://` ou `t.me/` |
| `has_hashes` | booleano | sim | reservado; hoje sempre `false` |
| `action_type` | string | sim | `post`, `reply` (resposta a outra mensagem) ou `forward` |

Observações:

- Mensagens **enviadas pela própria conta** também são enviadas à API
  (o monitor não filtra remetente).
- O JSON é compacto (sem espaços/quebras) e os campos aparecem na ordem acima.
- A tabela `messages` do `messages.db` local espelha exatamente esses campos.

## Exemplo de requisição

```json
{
  "message_id": 12345,
  "sender_id": 987654321,
  "sender_username": "fulano",
  "chat_id": -1001234567890,
  "chat_title": "Grupo de Ofertas",
  "timestamp": "2026-11-19T14:30:00Z",
  "message_text": "Oferta relâmpago: http://exemplo.com/promo",
  "message_type": "text",
  "has_links": true,
  "has_hashes": false,
  "action_type": "post"
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
import sqlite3
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()
db = sqlite3.connect("cti.db", check_same_thread=False)
db.execute("""
CREATE TABLE IF NOT EXISTS messages (
  chat_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  sender_id INTEGER,
  sender_username TEXT,
  chat_title TEXT,
  timestamp TEXT,
  message_text TEXT,
  message_type TEXT,
  has_links INTEGER,
  has_hashes INTEGER,
  action_type TEXT,
  received_at TEXT DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (chat_id, message_id)
)
""")

class Message(BaseModel):
    message_id: int
    sender_id: int
    sender_username: str | None = None
    chat_id: int
    chat_title: str
    timestamp: str
    message_text: str
    message_type: str
    has_links: bool
    has_hashes: bool
    action_type: str

@app.post("/api/telegram/message")
def receive(msg: Message):
    db.execute(
        "INSERT OR IGNORE INTO messages (chat_id, message_id, sender_id,"
        " sender_username, chat_title, timestamp, message_text, message_type,"
        " has_links, has_hashes, action_type)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        (msg.chat_id, msg.message_id, msg.sender_id, msg.sender_username,
         msg.chat_title, msg.timestamp, msg.message_text, msg.message_type,
         msg.has_links, msg.has_hashes, msg.action_type),
    )
    db.commit()
    return {"ok": True}
```

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
