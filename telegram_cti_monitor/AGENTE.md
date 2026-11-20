# Agente de links de grupos

O agente lê os links `t.me` que o monitor já gravou em `messages.db`,
classifica cada grupo com um modelo do [OpenRouter](https://openrouter.ai) e
entra nos grupos aprovados (pelo modelo ou por você no TUI). Ele roda **dentro
do `make run`** e usa a mesma sessão do Telegram do monitor.

> Entrar em grupos é uma ação externa na sua conta. Leia os limites e riscos
> abaixo antes de ligar o modo autônomo.

## Como funciona

A cada varredura (padrão: 5 min):

1. pega mensagens novas com link (`messages.has_links = 1`);
2. extrai links de grupo (`t.me/+hash`, `t.me/joinchat/hash`, `t.me/usuario`,
   `tg://join?invite=...` e `tg://resolve?domain=...`), ignorando links
   reservados (share, proxy, addstickers etc.);
3. grava em `group_candidates` (um registro por link, sem duplicar);
4. antes de classificar, faz um *preview* sem entrar:
   - convite: `messages.checkChatInvite` (título, descrição, participantes,
     flags de scam/fake);
   - público: resolve o `@usuario` e confirma que é grupo;
   - já membro, canal ou entrada por aprovação são marcados como `skipped`/`already`;
5. filtra repetidos: se o mesmo chat (pelo id) ou o mesmo título exato já está
   `approved`/`joined`/`already`, marca `duplicate` e não gasta classificação;
6. manda link + título + descrição + (opcional) texto da mensagem para o modelo;
7. se `fraud = true` e `score >= AGENT_MIN_SCORE`, marca `approved`;
8. se for entrar (`AGENT_AUTO_JOIN=true` ou aprovação manual no TUI) e houver
   orçamento, entra (`ImportChatInvite` ou `JoinChannel`) e marca `joined`.

A classificação usa `temperature = 0`, `max_tokens = 200` e no máximo
`AGENT_CLASSIFY_PER_SCAN` links por varredura, para controlar custo.

Falhas são separadas em dois grupos:

- **permanentes** (convite expirado/inválido, chat privado ou apagado, grupo
  cheio, banido, `@usuario` inexistente): o convite vira `skipped` de primeira,
  sem gastar tentativas nem orçamento;
- **transitórias**: voltam para a fila até 3 tentativas e depois viram `error`
  na triagem ou `failed` na entrada.

`FLOOD_WAIT` e `PEER_FLOOD` (anti-spam da conta) não marcam o convite como
falho: pausam as entradas por até 1 h e o convite volta para `approved`.

## Configuração

Coloque no `.env` (ou no ambiente):

```env
OPENROUTER_API_KEY=sk-or-...
OPENROUTER_MODEL=google/gemini-2.5-flash
AGENT_AUTO_JOIN=true
AGENT_DRY_RUN=false
AGENT_SEND_CONTEXT=true
AGENT_MIN_SCORE=0.8
```

Sem `AGENT_AUTO_JOIN=true` o agente não entra nos aprovados pelo modelo. Ele
continua entrando nos que você aprovar manualmente no TUI (tecla `y`), porque
essa é uma decisão explícita sua. `AGENT_DRY_RUN=true` bloqueia toda entrada,
inclusive as manuais.

| Variável | Padrão | Efeito |
|---|---|---|
| `OPENROUTER_API_KEY` | — | sem ela o agente não liga |
| `OPENROUTER_MSG_MODEL` | — | modelo leve, compartilhado com a triagem de mensagens; se definido, vale para o agente também |
| `OPENROUTER_MODEL` | `google/gemini-2.5-flash` | modelo do OpenRouter (usado quando `OPENROUTER_MSG_MODEL` não existe) |
| `AGENT_ENABLED` | `true` | `false` desliga |
| `AGENT_AUTO_JOIN` | `false` | `true` entra sozinho nos aprovados pelo modelo; `false` só entra os aprovados no TUI |
| `AGENT_DRY_RUN` | `false` | `true` nunca entra, só registra |
| `AGENT_SEND_CONTEXT` | `true` | envia o texto da mensagem como contexto |
| `AGENT_MIN_SCORE` | `0.8` | score mínimo para entrar |
| `AGENT_MAX_JOINS_PER_HOUR` | `2` | tentativas por hora (teto duro: 10) |
| `AGENT_MAX_JOINS_PER_DAY` | `6` | tentativas por dia (teto duro: 50) |
| `AGENT_CLASSIFY_PER_SCAN` | `5` | links classificados por varredura (máx. 20) |
| `AGENT_SCAN_SECS` | `300` | intervalo entre varreduras (mín. 30) |
| `AGENT_JOIN_DELAY_SECS` | `60` | pausa entre entradas (mín. 10) |

Modelos baratos para testar: `google/gemini-2.5-flash`,
`openai/gpt-4.1-mini`, `deepseek/deepseek-chat`. Confirme o id atual em
<https://openrouter.ai/models>.

## Limites contra ban

- Os limites contam **tentativas**, não só entradas bem-sucedidas. Uma falha
  também consome orçamento.
- `attempted_at` fica no SQLite, então reiniciar o monitor **não** zera a
  janela de 1 h / 1 dia.
- `FLOOD_WAIT` do Telegram interrompe **entradas e consultas de preview** e
  respeita o tempo pedido (teto de 24 h). `PEER_FLOOD` para tudo por 24 h.
- Há uma pausa (`AGENT_JOIN_DELAY_SECS`) entre entradas e 3 s entre consultas
  de preview das triagens.
- Fotos de remetentes/grupos são baixadas uma vez por versão, espaçadas em 2 s.
- O teto diário (`AGENT_MAX_JOINS_PER_DAY`, padrão 6) vale até para retry
  forçado no TUI; só o limite por hora é ignorado.
- Os padrões (2/h, 6/dia) são conservadores, mas **não há garantia** de que a
  conta não seja limitada. Contas novas e muitos grupos por dia são o cenário
  de maior risco. Aumente só se souber o que está fazendo.

Para revisar sem risco, rode com:

```env
AGENT_DRY_RUN=true
```

ou

```env
AGENT_AUTO_JOIN=false
```

## TUI: acompanhar e decidir

`make dash` → tecla `a` abre a aba **Agente**.

- Lista as análises: status, score, veredito, título, link e chat de origem.
- Por padrão **oculta** `joined`, `already` e `duplicate` (já entrou ou repetido).
  `f` mostra tudo.
- `Enter` abre o detalhe com o motivo do modelo, o texto de origem e o
  histórico de ações.
- `y` aprova e enfileira a entrada; `n` rejeita (status `skipped`);
  `x` faz retry forçado, ignorando o orçamento por hora/dia daquele convite.
- `r` recarrega, `s` vai para estatísticas, `a` volta para as mensagens.

Aprovar no TUI **não** entra na hora: o dashboard não fala com o Telegram. A
decisão grava `manual_approved=1` e a ação `manual_approve`; o agente do
`make run` executa na próxima varredura, respeitando os limites de 1 h / 1 dia.
Isso vale mesmo com `AGENT_AUTO_JOIN=false`. Com `AGENT_DRY_RUN=true` nada é
executado até você desligar o dry-run.

O `x` (retry forçado) marca `force_join=1`: o agente tenta aquele convite mesmo
com o limite por hora esgotado. O teto diário continua valendo. Não vale para
convites `pending` (sem triagem) nem para os já tratados
(`joined`/`already`/`duplicate`). O flag é consumido na tentativa e não passa
por cima de `FLOOD_WAIT`/`PEER_FLOOD` (espera mandada pelo Telegram) nem do
dry-run.

## Tabela `group_candidates`

Estados: `pending` → `approved`/`skipped` → `joined`/`already`/`failed`/`error`.
`duplicate` é terminal (mesmo chat/título de um grupo já aprovado ou entrado).
Aprovações manuais têm `manual_approved = 1`.

Acompanhe:

```bash
sqlite3 messages.db "SELECT status, COUNT(*) FROM group_candidates GROUP BY status;"
sqlite3 messages.db "SELECT link, score, reason, status FROM group_candidates ORDER BY id DESC LIMIT 20;"
```

Reprocessar erros de classificação (ex.: depois de corrigir a chave/modelo):

```bash
sqlite3 messages.db "UPDATE group_candidates SET status='pending', tries=0, error=NULL WHERE status='error';"
```

Reenfileirar falhas de entrada como aprovação manual (consome orçamento):

```bash
sqlite3 messages.db "UPDATE group_candidates SET status='approved', manual_approved=1, tries=0, error=NULL WHERE status='failed';"
```

Forçar nova tentativa num grupo específico (sem zerar `attempted_at`, para não
burlar o limite):

```bash
sqlite3 messages.db "UPDATE group_candidates SET status='approved', manual_approved=1, tries=0, error=NULL WHERE link='https://t.me/+...';"
```

## Log de ações (`group_actions`)

Cada ação do agente e do TUI vira uma linha (`link`, `action`, `detail`,
`actor` = `agent`/`manual`, `created_at`):

- `approved`/`skipped` (detalhe traz score e motivo), `duplicate`, `already`;
- `join_attempt`, `joined`, `failed`, `retry`, `error`;
- `manual_approve`, `manual_skip`.

```bash
sqlite3 messages.db "SELECT created_at, actor, action, detail FROM group_actions ORDER BY id DESC LIMIT 30;"
```

## Privacidade e dados enviados

- Com `AGENT_SEND_CONTEXT=true`, até 2.000 caracteres do texto da mensagem
  capturada (que pode conter PII) vão para o OpenRouter. Nome/telefone do
  remetente **não** são enviados.
- Para enviar só link, título, descrição e métricas do grupo, use
  `AGENT_SEND_CONTEXT=false`.
- O veredito, o modelo e o motivo ficam no SQLite local.

## Monitorar os grupos novos

O filtro `MONITORED_CHATS` vale para mensagens recebidas. Se ele estiver
preenchido, os grupos em que o agente entrar **não** serão coletados até você
adicionar os ids. Para coletar de tudo que o agente entrar, deixe
`MONITORED_CHATS` vazio.

## Solução de problemas

| Sintoma | Causa provável |
|---|---|
| `[agente] desativado (OPENROUTER_API_KEY ausente)` | falta a chave |
| `401` / `invalid api key` | chave errada ou sem crédito |
| `model not found` | id do modelo errado; confira em openrouter.ai/models |
| agente ligado mas nenhum link novo | não há `t.me` em `messages`, ou o link já foi visto |
| `FLOOD_WAIT` | Telegram pediu pausa; o agente para e tenta na próxima |
| `limite de entradas atingido` | orçamento por hora/dia; espere a janela |
| aprovação no TUI não entra | agente não está no `make run`, `AGENT_DRY_RUN=true`, `AGENT_ENABLED=false` ou limite atingido |
| convite sumiu da lista do TUI | virou `joined`/`already`/`duplicate`; `f` mostra tudo |
| grupo entrou mas nada é coletado | `MONITORED_CHATS` está preenchido sem esse id |

## Aviso

Automatizar entrada em grupos pode violar os Termos do Telegram e expor a
conta usada. Os limites e o preview reduzem o risco, mas não eliminam ban,
limitação ou remoção. Use uma conta dedicada, mantenha o volume baixo e revise
os primeiros dias em `AGENT_DRY_RUN=true`.
