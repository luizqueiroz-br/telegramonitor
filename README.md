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

Dashboard no terminal (atualiza a cada 2 s, lendo só o SQLite):

```bash
make dash
```

Teclas: `s` alterna feed/estatísticas, `t` troca a janela (1 h → 24 h → 7 d →
tudo), `q` sai, `p` pausa/retoma, `r` recarrega do zero. As estatísticas
(total, chats, remetentes, links, top chats/remetentes, tipos, domínios e
mensagens por hora UTC) atualizam a cada ~5 s; `p` congela tudo.

Filtros (combináveis com a janela): `/` busca textual (Enter aplica, Esc
cancela), `g` escolhe o chat (busca por título ou id), `m` escolhe o tipo, `l`
liga/desliga "só com links" e `c` limpa tudo. Setas, PgUp/PgDn, Home/End
navegam; Enter abre o detalhe com todos os campos da mensagem (Esc volta).
Pode deixar rodando ao lado do monitor — o banco usa WAL para leitura
concorrente. Documentação completa da TUI (abas, filtros, detalhe e
solução de problemas): [`telegram_cti_monitor/TUI.md`](telegram_cti_monitor/TUI.md).

### Agente de links (opcional)

Com uma chave do OpenRouter (`OPENROUTER_API_KEY`), o `make run` também liga um
agente que lê os links `t.me` já gravados e classifica o grupo com um modelo
barato. Com `AGENT_AUTO_JOIN=true`, ele entra sozinho nos aprovados; por padrão
só classifica. Ele usa a mesma sessão do monitor e respeita limites
conservadores por hora/dia (contando tentativas) para reduzir risco de ban.

Comece em modo de revisão (`AGENT_AUTO_JOIN=false`) e veja a tabela
`group_candidates`. Guia completo, variáveis, limites e riscos:
[`telegram_cti_monitor/AGENTE.md`](telegram_cti_monitor/AGENTE.md).

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

A tabela `messages` guarda o envelope completo: identidade da mensagem,
remetente (nome, `@username`, telefone quando visível, bot/verificado/scam),
chat (título, `@username`, tipo), mídia (nome/MIME/tamanho/duração), dados
estruturados (localização, contato, enquete), forward/resposta e contadores
(views, forwards, respostas, reações). A lista oficial de colunas está em
`MESSAGE_COLUMNS` no código e em [API_CTI.md](API_CTI.md).

## Catálogo de remetentes e chats

O update do Telegram já traz os objetos de usuário/chat, então eles são
catalogados **sem chamada extra** nas tabelas `users` e `chats` do mesmo
`messages.db` (id, `@username`, nome/título, tipo e flags).

As fotos de perfil são baixadas **uma única vez por entidade** e ficam em:

```
avatars/user-<id>.jpg
avatars/chat-<id>.jpg
```

Se a foto já está no catálogo (mesmo `photo_id`), o monitor não consulta o
Telegram de novo; se o usuário trocar a foto, a nova é baixada na próxima
mensagem. Entidades sem foto são marcadas para não serem consultadas de novo.

## Como funciona

- Login como conta de usuário (telefone + código + 2FA opcional).
- Sessão persistida em SQLite (`monitor.session`). `make logout` apaga.
- Escuta os updates do Telegram e, para cada mensagem nova, grava no SQLite,
  imprime no console e faz `POST` do JSON em `CTI_API_URL` (via `curl`).

Contrato da API que recebe esses dados: [API_CTI.md](API_CTI.md).

> Automação de contas de usuário é tolerada para leitura, mas o Telegram pode
> limitar contas que fazem ações em massa. Este monitor só lê mensagens.
