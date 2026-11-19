# telegramonitor

Monitor de chats de uma conta do Telegram que envia as mensagens para uma
plataforma de CTI. Usa MTProto (grammers), não a API de bots — então enxerga
todos os chats da sua conta.

## Requisitos

- Rust (cargo) — https://rustup.rs
- Compilador C/linker (`build-essential` no Ubuntu/WSL)

No WSL/Ubuntu:

```bash
sudo apt-get update && sudo apt-get install -y build-essential
```

## Início rápido

```bash
cd telegram_cti_monitor
make setup     # cria o .env e verifica as dependências
```

1. Crie um app em <https://my.telegram.org> → **API development tools**.
2. Rode `make run` e responda o que ele pedir: `API_ID`, `API_HASH`, o
   telefone, o código (chega dentro do app Telegram) e a senha 2FA, se houver.
   As credenciais são salvas no `.env` e a sessão em `monitor.session`, então
   isso é só na primeira vez.

Variáveis (todas opcionais depois do primeiro uso):

| Variável | Obrigatória | Descrição |
|---|---|---|
| `TELEGRAM_API_ID` | sim | `api_id` do app |
| `TELEGRAM_API_HASH` | sim | `api_hash` do app |
| `TELEGRAM_PHONE` | 1º login | telefone internacional, ex.: `+5511999999999` |
| `SESSION_PATH` | não | arquivo de sessão (padrão `monitor.session`) |
| `DB_PATH` | não | banco SQLite com as mensagens (padrão `messages.db`) |
| `CTI_API_URL` | não | endpoint que recebe as mensagens; vazio = só console |
| `MONITORED_CHATS` | não | ids separados por vírgula; vazio = todos |

3. O programa também lista os chats com os ids para você montar o
   `MONITORED_CHATS`.

Outros comandos: `make help`.

## Ver as mensagens gravadas

Toda mensagem monitorada é gravada em `messages.db` (SQLite). Para listar as
últimas 50 (pode rodar com o monitor ativo — o banco usa WAL):

```bash
make messages
```

Ou consulte direto, se tiver o `sqlite3` instalado:

```bash
sqlite3 messages.db "SELECT timestamp, chat_title, sender_username, message_text FROM messages ORDER BY id DESC LIMIT 20;"
```

### Visualizador SQLite no Windows

Abrir o `messages.db` direto pelo caminho `\\wsl.localhost\...` **não
funciona**: o SQLite fica em modo WAL (para monitor e leitura conviverem), e
WAL não é suportado nesse tipo de compartilhamento — o visualizador mostra
`database is locked`. Gere uma cópia consolidada, sem WAL:

```bash
make export      # cria messages-export.db
```

Abra o `messages-export.db` no visualizador. Rode `make export` de novo quando
quiser atualizar a cópia.

A tabela `messages` tem: `message_id`, `sender_id`, `sender_username`,
`chat_id`, `chat_title`, `timestamp`, `message_text`, `message_type`,
`has_links`, `has_hashes`, `action_type` e `saved_at`.

## Como funciona

- Login como conta de usuário (telefone + código + 2FA opcional).
- Sessão persistida em SQLite (`monitor.session`). `make logout` apaga.
- Escuta os updates do Telegram e, para cada mensagem nova, grava no SQLite,
  imprime no console e faz `POST` do JSON em `CTI_API_URL` (via `curl`).

Contrato da API que recebe esses dados: [API_CTI.md](API_CTI.md).

> Automação de contas de usuário é tolerada para leitura, mas o Telegram pode
> limitar contas que fazem ações em massa. Este monitor só lê mensagens.
