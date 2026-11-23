# API que recebe os dados do monitor

O `telegram_cti_monitor` envia **uma requisição HTTP por mensagem** nova do
Telegram para a URL configurada em `CTI_API_URL`. Este documento descreve o
contrato que essa API precisa cumprir. A API pode ser escrita em qualquer
linguagem/stack; o exemplo abaixo usa FastAPI só como referência.

Fotos (JPEG) viajam **no mesmo POST da mensagem**, como base64. O catálogo de
atores (usuários/grupos/canais com as fotos) usa `POST /api/v1/actors`, num
segundo request, independente da mensagem.

## Escopo desta versão (fotos)

1. `POST /api/v1/telegram/messages` aceita `media_base64` (JPEG, ≤ 8 MiB
   decodificados) e expõe `GET /api/v1/telegram/messages/<chat_id>/<message_id>/photo`.
2. `POST /api/v1/actors` aceita `telegram_id` (chave alternativa ao `username`),
   `kind`, `photo_base64` e `photo_mime`; expõe `GET /api/v1/actors/<ref>/photo`.
3. Regra comum: foto ausente/inválida **nunca** descarta o registro, e
   reenvio com foto `null` não apaga a foto já gravada.

O resto do contrato (campos existentes, autenticação, envelopes) não muda.

## Fluxo

```
Telegram (MTProto) --update--> monitor --POST JSON + foto base64--> CTI_API_URL
                                   |
                                   +--> messages.db (SQLite local)

monitor --POST /api/v1/actors + foto base64--> CTI (catálogo de atores)
```

## Triagem anúncio/conversa

Quando `OPENROUTER_API_KEY` está configurada, cada mensagem nova é classificada
como anúncio (`ad`) ou conversa (`chat`) antes do envio. **Anúncios não geram
POST de mensagem**: ficam só no `messages.db` local (`messages.msg_kind`). Se o
modelo falhar, a mensagem é enviada normalmente. O formato do JSON abaixo não
muda. O **catálogo de atores** é enviado independentemente da triagem: um
anúncio também traz um ator (e a foto) para a CTI.

## Requisição

| Item | Valor |
|---|---|
| Método | `POST` |
| URL | exatamente o valor de `CTI_API_URL`, **com o path**: `https://SEU_HOST/api/v1/telegram/messages` (o monitor não adiciona path) |
| Cabeçalho | `Content-Type: application/json` |
| Corpo | um objeto JSON (a mensagem), sem envelope e sem lote |
| Autenticação | `Authorization: Bearer $CTI_API_TOKEN` (obrigatória; ver [Autenticação](#autenticação)) |
| Timeout | 30 s (`curl -m 30`) |
| Tamanho | até ~12 MiB de corpo (foto decodificada de até 8 MiB → base64 ~10,7 MiB) |

O monitor processa as mensagens **uma de cada vez** (aguarda a resposta antes
da próxima). Uma mensagem com foto e ator novo pode gerar até **três** POSTs
sequenciais: mensagem, mensagem+foto e ator. Responda rápido; trabalho pesado
deve ser assíncrono no servidor.

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
| `media_mime` | string ou `null` | MIME type; foto é sempre JPEG — se vier `null`, trate como `image/jpeg` |
| `media_size` | inteiro ou `null` | tamanho em bytes (tamanho do arquivo original anunciado pelo Telegram) |
| `media_duration` | número ou `null` | duração em segundos (áudio/vídeo) |
| `media_base64` | string ou `null` | foto JPEG em base64 — só quando `message_type="photo"`; regras na seção [Fotos](#fotos-mídia-binária) |

Só **fotos** têm o conteúdo binário enviado. Documentos, vídeos, áudios e
stickers continuam apenas com metadados (podem ser centenas de MB). As fotos
de perfil de remetentes e chats são guardadas no `messages.db` local
(`users.avatar_blob` / `chats.avatar_blob`) e vão à CTI pelo
`POST /api/v1/actors` — não pelo POST de mensagens.

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
- O JSON é compacto (sem espaços/quebras); a ordem das chaves é ilustrativa —
  **não** faça parsing por posição.
- A tabela `messages` do `messages.db` local espelha exatamente esses campos
  (mais `media_blob` e `msg_kind`, que não são enviados).
- O catálogo local `users`/`chats` (fotos em BLOB) é enviado à parte por
  `POST /api/v1/actors`; ele não vai no POST de mensagens.
- Os campos novos (`media_base64`, e os de ator) são **opcionais**: monitores
  antigos não os enviam e a API precisa continuar aceitando o payload sem eles.

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
  "media_base64": null,
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
curl -X POST http://localhost:8000/api/v1/telegram/messages \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer SEU_TOKEN' \
  -d '{"message_id":1,"sender_id":2,"sender_username":null,"chat_id":-1001234567890,"chat_title":"Teste","timestamp":"2026-11-19T14:30:00Z","message_text":"oi","message_type":"text","has_links":false,"has_hashes":false,"action_type":"post"}'
```

## Fotos (mídia binária)

Foto de mensagem vai no campo `media_base64` do POST de ingestão. O catálogo
de atores (incluindo as fotos de perfil) vai em `POST /api/v1/actors`.

> **Como o monitor envia:** a mensagem vai primeiro **sem** foto (ingestão
> rápida) e, depois do download, o mesmo POST é repetido com `media_base64` —
> por isso o upsert precisa aceitar `media_base64` em qualquer POST e preservar
> a foto quando ele vier `null`. Fotos acima de 8 MiB ficam só no banco local.
> O catálogo de atores é enviado uma vez por ator a cada execução do monitor.

**Formato do base64:** alfabeto padrão RFC 4648 com padding `=`; **sem** o
prefixo `data:image/jpeg;base64,`, **sem** quebras de linha/espaços. Se
`media_mime` vier `null`, assuma `image/jpeg`.

**Limite:** até **8 MiB decodificados** por foto. O monitor não envia foto
maior que isso: manda `media_base64: null` e a foto fica só no `messages.db`
local (o BLOB local pode ser maior; o teto de envio é este). O corpo máximo do
request fica em ~12 MiB (base64 cresce ~33%). Fotos típicas do Telegram ficam
abaixo de 1 MiB; perto do teto, o base64 pode esbarrar no timeout de 30 s do
monitor — por isso o limite é 8 MiB e não os 20 MiB do banco local.

**Falha não descarta a mensagem.** A mensagem é o dado principal; a foto é
opcional. Se o base64 for inválido ou passar do limite, a API deve:

1. gravar a mensagem normalmente, sem a foto;
2. responder `200` com `{"success": true, "media_stored": false}`;
3. registrar um aviso no log do servidor.

Não responda `4xx` por causa da foto: o monitor não reenvia e a mensagem
inteira se perderia para a CTI.

**Armazenamento:** decodifique uma vez e guarde os bytes como `BLOB` (não
guarde o base64). Não mantenha o `media_base64` cru dentro do `raw_json` —
isso duplicaria a foto com ~33% de overhead; anule o campo antes de persistir
o JSON.

**Validação (o corpo é dado não confiável):**

- aceite só JPEG: os primeiros bytes são `FF D8 FF`; rejeite/ignore o resto;
- ao servir, force `Content-Type: image/jpeg` — nunca ecoe um MIME vindo do
  cliente nem renderize o arquivo como HTML/SVG;
- aplique o limite de tamanho **antes** de decodificar quando possível e
  configure o limite do proxy (ex.: nginx `client_max_body_size 12m`), senão
  o proxy corta o request antes da aplicação;
- uma foto por request (o monitor já envia uma mensagem por vez).

**Idempotência:** reenvio com `media_base64: null` **não apaga** foto já
gravada; reenvio com foto substitui. Vale para mensagens e atores.

**Leitura da foto (mensagens):**

```
GET /api/v1/telegram/messages/<chat_id>/<message_id>/photo
Authorization: Bearer $CTI_API_TOKEN
```

- `200` com o JPEG cru (`Content-Type: image/jpeg`, `Content-Length`) quando
  existe foto;
- `404` com `{"error":"foto não encontrada"}` quando a mensagem não tem foto
  ou não existe.

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
| Gravou/processou | `200` (ou `201`) com `{"success":true}`; inclua `media_stored`/`photo_stored` quando houver foto no request |
| Foto inválida ou acima de 8 MiB | `200` com `{"success":true,"media_stored":false}` — grave a mensagem sem a foto |
| JSON inválido / campo faltando | `400` com `{"detail":"..."}` (evite a palavra `error`) |
| Falha interna | `500` com `{"error":"..."}` **de propósito**, para o operador ver no console |

Evite respostas como `{"errors":[]}` no sucesso: a palavra `error` (comparação
literal, minúscula) faria o monitor registrar falha.

## Endpoints da plataforma CTI

O monitor chama **dois** endpoints de escrita: a ingestão de mensagens e
`POST /api/v1/actors` para o catálogo de atores. Os demais são
para a plataforma CTI consultar/gerenciar o que foi recebido. A implementação
deles pertence ao repositório da CTI; o contrato fica fixado aqui para o
monitor e a plataforma evoluírem juntos.

| Método | Caminho | Uso |
|---|---|---|
| `POST` | `/api/v1/telegram/messages` | ingestão, chamada pelo monitor (valor de `CTI_API_URL`) |
| `GET` | `/api/v1/telegram/messages/filter` | consulta com filtros |
| `GET` | `/api/v1/telegram/messages/<chat_id>/<message_id>/photo` | foto (JPEG) da mensagem |
| `POST` | `/api/v1/actors` | cadastrar/atualizar ator suspeito (com foto opcional) |
| `GET` | `/api/v1/actors/<username_ou_id>` | perfil de um ator |
| `GET` | `/api/v1/actors/<username_ou_id>/photo` | foto (JPEG) do ator |

Todos exigem o mesmo Bearer do `CTI_API_TOKEN` e ficam fora de qualquer
prefixo de idioma. Em `<username_ou_id>`, um valor que case com `^-?\d+$` é
tratado como `telegram_id`; qualquer outro é tratado como `username`.

### GET /api/v1/telegram/messages/filter

Query string (todos opcionais e combináveis):

| Parâmetro | Tipo | Descrição |
|---|---|---|
| `chat_id` | inteiro | filtra pelo chat |
| `sender_id` | inteiro | filtra pelo remetente |
| `message_type` | string | `text`, `photo`, `document`, ... |
| `has_links` | `true`/`false` | filtra por presença de links |
| `has_photo` | `true`/`false` | filtra por mensagens com foto armazenada |
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
  "items": [ { "chat_id": -1001234567890, "message_id": 12345, "timestamp": "2026-11-19T14:30:00Z", "message_text": "...", "has_links": true, "has_photo": false } ]
}
```

Os `items` são os mesmos objetos do POST (todos os campos, exceto
`media_base64` — ele não é devolvido, use o endpoint `/photo` para o binário)
mais o booleano `has_photo`, ordenados por `timestamp` decrescente. Use
`(chat_id, message_id)` como chave estável.

### POST /api/v1/actors

Cadastra ou atualiza um ator monitorado. É o caminho para enviar o catálogo de
usuários/grupos/canais do monitor, **incluindo a foto de perfil**.

```json
{
  "telegram_id": -1001234567890,
  "username": "grupodeofertas",
  "kind": "channel",
  "first_seen": "2026-11-01T10:00:00Z",
  "last_seen": "2026-11-19T14:30:00Z",
  "associated_groups": [-1001234567890, -1009876543210],
  "tags": ["scam", "link-suspeito"],
  "photo_base64": "…",
  "photo_mime": "image/jpeg"
}
```

| Campo | Tipo | Obrigatório | Descrição |
|---|---|---|---|
| `telegram_id` | inteiro ou `null` | um dos dois | id no formato Bot API, o mesmo `sender_id`/`chat_id` das mensagens (usuário/bot positivo; grupo/canal negativo) |
| `username` | string ou `null` | um dos dois | `@` sem arroba |
| `kind` | string ou `null` | não | `user`, `group`, `channel` ou `community` |
| `first_seen` / `last_seen` | ISO 8601 ou `null` | não | se vierem `null`, a API mantém (primeiro insert define `first_seen`; cada upsert atualiza `last_seen`) |
| `associated_groups` | lista de inteiros ou `null` | não | ids de chats onde o ator foi visto |
| `tags` | lista de strings ou `null` | não | rótulos operacionais |
| `photo_base64` | string ou `null` | não | foto JPEG em base64, mesmas regras e limite de 8 MiB da seção [Fotos](#fotos-mídia-binária) |
| `photo_mime` | string ou `null` | não | trate `null` como `image/jpeg` |

**Chave de upsert:** `telegram_id` quando presente; senão `username`. Se os
dois vierem e apontarem para linhas diferentes, prevalece `telegram_id` e o
`username` da linha é atualizado. Sem nenhum dos dois → `400`. O monitor envia
cada ator **uma vez por execução** (deduplicado em memória); reiniciar pode
reenviar, e o upsert idempotente torna isso inofensivo.

Fora deste contrato ficam os outros campos que o monitor já guarda no catálogo
local (flags como `is_scam`/`is_fake`/`is_premium`, lista completa de
`usernames`, `participants` de chats). Se a CTI precisar deles, entram numa
próxima versão do contrato — não os invente agora.

**Foto:** se o base64 for inválido ou passar do limite, grave o ator normalmente
sem a foto e responda `200` com `"photo_stored": false` (não devolva `4xx`
só pela foto). `photo_base64: null` **não apaga** a foto já gravada.

Resposta `200` (ou `201` na criação):

```json
{ "success": true, "stored_id": 42, "photo_stored": true }
```

### GET /api/v1/actors/<username_ou_id>

Resposta `200`:

```json
{
  "success": true,
  "actor": {
    "stored_id": 42,
    "telegram_id": -1001234567890,
    "username": "grupodeofertas",
    "kind": "channel",
    "first_seen": "2026-11-01T10:00:00Z",
    "last_seen": "2026-11-19T14:30:00Z",
    "associated_groups": [-1001234567890],
    "tags": ["scam"],
    "message_count": 137,
    "has_photo": true,
    "photo_url": "/api/v1/actors/-1001234567890/photo"
  }
}
```

`404` com `{"error":"ator não encontrado"}` quando a referência não existe.
A referência é `telegram_id` se casar com `^-?\d+$`; senão `username`.

### GET /api/v1/actors/<username_ou_id>/photo

- `200` com o JPEG cru (`Content-Type: image/jpeg`, `Content-Length`);
- `404` com `{"error":"foto não encontrada"}` quando o ator não existe ou não
  tem foto.

**Envelope:** toda resposta traz `success` (booleano) e, quando aplicável,
`stored_id` (registro criado) ou `count`/`items` (consultas). Erros usam
`{"error":"..."}` — a palavra `error` minúscula também sinaliza falha no
monitor, conforme a seção anterior.

## Idempotência (importante)

O monitor usa `catch_up`, então após reiniciar ele pode reenviar mensagens que
chegaram enquanto estava offline. **A API deve deduplicar** pela chave
`(chat_id, message_id)` — a mesma chave usada pelo SQLite local. Assim, repetir
o POST é inofensivo.

Regras de atualização no upsert:

- mensagem: `raw_json` sempre atualiza; `media`/`media_mime` só quando
  `media_base64` vem preenchido — `null` preserva o que já existe;
- ator: mesma regra para `photo`/`photo_mime`; `telegram_id` é a chave
  preferencial (ver [POST /api/v1/actors](#post-apiv1actors));
- nunca trate um reenvio como `409`/erro: responda `200`. O monitor não tenta
  de novo e a mensagem se perderia.

## Autenticação

Obrigatória. Todo request leva:

```
Authorization: Bearer $CTI_API_TOKEN
```

O token **não** vem do `.env` da plataforma: ele é gerado e mantido no portal
da CTICyber, em `/gestao/configuracoes/` → **“Token da API de monitoramento”**
(acesso staff com 2FA). Gere o token lá, copie para o `CTI_API_TOKEN` do
monitor e trate como segredo.

Do lado da plataforma:

- token ausente ou errado → `401` (corpo com `error`);
- nenhum token gerado/configurado → `503` (falha fechada, nada é gravado);
- rotacionar no portal invalida o token anterior **na hora** — atualize o
  monitor logo depois, senão a ingestão passa a responder `401`;
- em produção, sempre use `https://`. Se o host responder `301` (corpo vazio)
  para `http://`, o `curl -s` do monitor considera sucesso sem ingerir nada.

**No monitor:** `send_to_cti_api` (`src/main.rs`) lê `CTI_API_TOKEN` no boot
e envia o header em toda requisição. Se `CTI_API_URL` estiver definida sem
`CTI_API_TOKEN`, o monitor avisa no boot e envia mesmo assim — a ingestão
responde `401`, conforme acima.

Nunca exponha a API na internet aberta sem o token — os dados podem conter
telefones, nomes e links de vítimas.

## Implementação de referência (FastAPI)

```python
import base64
import json
import os
import secrets
import sqlite3

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse, Response

TOKEN = os.environ.get("CTI_API_TOKEN")
MAX_PHOTO = 8 * 1024 * 1024  # 8 MiB decodificados
app = FastAPI()
db = sqlite3.connect("cti.db", check_same_thread=False)
db.execute("""
CREATE TABLE IF NOT EXISTS messages (
  chat_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  received_at TEXT DEFAULT CURRENT_TIMESTAMP,
  raw_json TEXT NOT NULL,
  media BLOB,
  media_mime TEXT,
  PRIMARY KEY (chat_id, message_id)
)
""")

def decode_photo(value):
    """JPEG válido até MAX_PHOTO, ou None. Nunca levanta."""
    if not value or not isinstance(value, str):
        return None
    try:
        data = base64.b64decode(value, validate=True)
    except (ValueError, TypeError):  # binascii.Error é ValueError
        return None
    if len(data) > MAX_PHOTO or not data.startswith(b"\xff\xd8\xff"):
        return None
    return data

def authorized(request):
    sent = request.headers.get("authorization", "").removeprefix("Bearer ")
    return bool(TOKEN) and secrets.compare_digest(sent.encode(), TOKEN.encode())

@app.post("/api/v1/telegram/messages")
async def receive(request: Request):
    if not TOKEN:
        return JSONResponse({"error": "token não configurado"}, status_code=503)
    if not authorized(request):
        return JSONResponse({"error": "não autorizado"}, status_code=401)
    payload = await request.json()
    for key in ("chat_id", "message_id", "timestamp", "message_text", "message_type"):
        if key not in payload:
            raise HTTPException(status_code=400, detail=f"campo ausente: {key}")
    media = decode_photo(payload.get("media_base64"))
    mime = payload.get("media_mime") or ("image/jpeg" if media else None)
    payload.pop("media_base64", None)  # não duplica a foto no raw_json
    db.execute(
        "INSERT INTO messages (chat_id, message_id, raw_json, media, media_mime)"
        " VALUES (?, ?, ?, ?, ?)"
        " ON CONFLICT(chat_id, message_id) DO UPDATE SET"
        "   raw_json = excluded.raw_json,"
        "   media = COALESCE(excluded.media, messages.media),"
        "   media_mime = COALESCE(excluded.media_mime, messages.media_mime)",
        (payload["chat_id"], payload["message_id"],
         json.dumps(payload, ensure_ascii=False), media, mime),
    )
    db.commit()
    return {"success": True, "media_stored": media is not None}

@app.get("/api/v1/telegram/messages/{chat_id}/{message_id}/photo")
async def message_photo(chat_id: int, message_id: int, request: Request):
    if not authorized(request):
        return JSONResponse({"error": "não autorizado"}, status_code=401)
    row = db.execute(
        "SELECT media FROM messages WHERE chat_id = ? AND message_id = ?",
        (chat_id, message_id),
    ).fetchone()
    if not row or row[0] is None:
        return JSONResponse({"error": "foto não encontrada"}, status_code=404)
    return Response(content=row[0], media_type="image/jpeg")
```

Banco criado por uma versão anterior precisa das colunas novas:

```sql
ALTER TABLE messages ADD COLUMN media BLOB;
ALTER TABLE messages ADD COLUMN media_mime TEXT;
```

Para atores, a mesma ideia, com a chave dupla do contrato:

```sql
CREATE TABLE IF NOT EXISTS actors (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  telegram_id INTEGER UNIQUE,
  username TEXT UNIQUE,
  kind TEXT,
  first_seen TEXT,
  last_seen TEXT,
  associated_groups TEXT,
  tags TEXT,
  photo BLOB,
  photo_mime TEXT,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
  CHECK (telegram_id IS NOT NULL OR username IS NOT NULL)
)
-- upsert: procure por telegram_id; se não houver, por username.
-- photo = COALESCE(excluded.photo, actors.photo) na atualização.
```

Guardar o JSON cru não perde nenhum campo novo. Depois você pode criar
colunas/índices (por `sender_username`, `chat_id`, `timestamp`) e preenchê-las
a partir de `raw_json`, ou trocar por um `INSERT` com as colunas desejadas.

Rodar e configurar no monitor (`.env`):

```bash
pip install fastapi uvicorn
uvicorn main:app --host 127.0.0.1 --port 8000
```

```ini
CTI_API_URL=http://127.0.0.1:8000/api/v1/telegram/messages
CTI_API_TOKEN=token-gerado-no-portal
```

---

## Anexo: o "app do Telegram" (api_id/api_hash)

Se a dúvida for a credencial do lado do Telegram: o monitor precisa de um app
criado em <https://my.telegram.org> → **API development tools**. De lá saem
`TELEGRAM_API_ID` e `TELEGRAM_API_HASH`. Não há endpoint a implementar: o
monitor fala MTProto direto com os servidores do Telegram, e esses valores são
pedidos automaticamente no primeiro `make run` (veja o `README.md`).
